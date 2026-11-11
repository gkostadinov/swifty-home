//! The single decode worker: a dedicated thread that loads the model once and
//! then takes jobs from the bounded queue.

use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};

use crate::logging::Logger;
use crate::recognizer::Recognize;
use crate::wav::Audio;

const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct Decoded {
    pub text: String,
    pub decode_ms: u64,
}

/// Decoding failed inside the service. Carries no detail on purpose.
#[derive(Debug, PartialEq, Eq)]
pub struct InternalFailure;

pub struct Job {
    pub audio: Audio,
    pub reply: oneshot::Sender<Result<Decoded, InternalFailure>>,
}

/// Start the worker. `ready` is set once the model is loaded. A load failure is
/// sent on `fatal`, and the thread ends.
pub fn spawn<R, L>(
    load: L,
    mut jobs: mpsc::Receiver<Job>,
    ready: Arc<AtomicBool>,
    fatal: oneshot::Sender<String>,
    logger: Logger,
) -> io::Result<()>
where
    R: Recognize,
    L: FnOnce() -> Result<R, String> + Send + 'static,
{
    thread::Builder::new().name("decode".to_string()).stack_size(WORKER_STACK_BYTES).spawn(move || {
        let started = Instant::now();
        let recognizer = match catch_unwind(AssertUnwindSafe(load)) {
            Ok(Ok(recognizer)) => recognizer,
            Ok(Err(message)) => {
                let _ = fatal.send(message);
                return;
            }
            Err(_) => {
                let _ = fatal.send("the model loader failed unexpectedly".to_string());
                return;
            }
        };
        drop(fatal);
        logger.info(&format!("model loaded in {} ms", started.elapsed().as_millis()));
        ready.store(true, Ordering::Release);

        while let Some(job) = jobs.blocking_recv() {
            run_job(&recognizer, job);
        }
    })?;
    Ok(())
}

fn run_job<R: Recognize>(recognizer: &R, job: Job) {
    // The request was abandoned while this job waited, for example at the
    // connection deadline or during shutdown: nobody wants the answer. A client
    // that merely disconnects is not noticed here, because half-closed clients
    // are served, so its job is decoded and the answer dropped.
    if job.reply.is_closed() {
        return;
    }

    let started = Instant::now();
    let outcome = catch_unwind(AssertUnwindSafe(|| recognizer.recognize(job.audio.sample_rate, &job.audio.samples)));
    let decode_ms = started.elapsed().as_millis() as u64;
    drop(job.audio);

    let result = match outcome {
        Ok(Ok(text)) => Ok(Decoded { text, decode_ms }),
        Ok(Err(_)) | Err(_) => Err(InternalFailure),
    };
    let _ = job.reply.send(result);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted;

    impl Recognize for Scripted {
        fn recognize(&self, _rate: u32, samples: &[f32]) -> Result<String, &'static str> {
            match samples.first().copied() {
                Some(value) if value < 0.0 => panic!("scripted panic"),
                Some(value) if value > 0.5 => Err("scripted failure"),
                _ => Ok("fine".to_string()),
            }
        }
    }

    fn audio(first: f32) -> Audio {
        Audio { sample_rate: 16_000, samples: vec![first, 0.0], audio_ms: 0 }
    }

    fn submit(jobs: &mpsc::Sender<Job>, first: f32) -> oneshot::Receiver<Result<Decoded, InternalFailure>> {
        let (reply, receiver) = oneshot::channel();
        jobs.try_send(Job { audio: audio(first), reply }).ok().unwrap();
        receiver
    }

    fn start() -> (mpsc::Sender<Job>, Arc<AtomicBool>) {
        let (sender, receiver) = mpsc::channel(4);
        let ready = Arc::new(AtomicBool::new(false));
        let (fatal, _unused) = oneshot::channel();
        spawn(|| Ok(Scripted), receiver, ready.clone(), fatal, Logger::discard()).unwrap();
        (sender, ready)
    }

    #[test]
    fn a_panic_is_reported_and_the_worker_keeps_going() {
        // The panic message of the scripted panic is expected in test output.
        let (jobs, _ready) = start();
        let first = submit(&jobs, -1.0);
        assert_eq!(first.blocking_recv().unwrap().unwrap_err(), InternalFailure);
        let second = submit(&jobs, 0.0);
        assert_eq!(second.blocking_recv().unwrap().unwrap().text, "fine");
        let third = submit(&jobs, -1.0);
        assert_eq!(third.blocking_recv().unwrap().unwrap_err(), InternalFailure);
        let fourth = submit(&jobs, 0.0);
        assert_eq!(fourth.blocking_recv().unwrap().unwrap().text, "fine");
    }

    #[test]
    fn a_recognizer_error_is_reported() {
        let (jobs, _ready) = start();
        let failed = submit(&jobs, 1.0);
        assert_eq!(failed.blocking_recv().unwrap().unwrap_err(), InternalFailure);
    }

    #[test]
    fn ready_is_set_after_the_model_loads() {
        let (jobs, ready) = start();
        let answer = submit(&jobs, 0.0);
        answer.blocking_recv().unwrap().unwrap();
        assert!(ready.load(Ordering::Acquire));
    }

    #[test]
    fn a_failed_load_is_fatal_and_leaves_the_service_not_ready() {
        let (_sender, receiver) = mpsc::channel(1);
        let ready = Arc::new(AtomicBool::new(false));
        let (fatal, reason) = oneshot::channel();
        spawn(|| Err::<Scripted, String>("no model".to_string()), receiver, ready.clone(), fatal, Logger::discard())
            .unwrap();
        assert_eq!(reason.blocking_recv().unwrap(), "no model");
        assert!(!ready.load(Ordering::Acquire));
    }

    #[test]
    fn a_job_nobody_waits_for_is_skipped() {
        struct NeverCalled;
        impl Recognize for NeverCalled {
            fn recognize(&self, _rate: u32, _samples: &[f32]) -> Result<String, &'static str> {
                panic!("the recognizer must not run for an abandoned job");
            }
        }
        let (reply, receiver) = oneshot::channel();
        drop(receiver);
        // The assertion is that this returns without reaching the panic.
        let job = Job { audio: audio(0.0), reply };
        run_job(&NeverCalled, job);
    }
}

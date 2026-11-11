//! What the plugin logs through `tracing` during a run, for check 9.
//!
//! The suite runs under its own subscriber, at every level, writing into a
//! buffer. Calls run in their own tasks carry it along. Text a plugin prints
//! with `println!` or logs through the `log` crate is not seen.

use std::io;
use std::sync::{Arc, Mutex};

use tracing::Dispatch;
use tracing_subscriber::fmt::MakeWriter;

/// The buffer the run's log lines go to.
#[derive(Clone, Default)]
pub(super) struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// A subscriber that writes every event and span field into this buffer.
    pub(super) fn dispatch(&self) -> Dispatch {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(self.clone())
            .finish();
        Dispatch::new(subscriber)
    }

    /// Everything logged so far.
    pub(super) fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())).into_owned()
    }
}

impl io::Write for Capture {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

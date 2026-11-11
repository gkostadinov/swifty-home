//! An RTSP camera.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::LOOPBACK;
use crate::conformance::Switches;

pub const CAMERA_LOGIN: &str = "viewer:camera-login-0123456789";

// ----- RTSP camera

pub struct MockRtsp {
    pub port: u16,
    pub connections: Arc<AtomicUsize>,
    pub received: Arc<Mutex<Vec<String>>>,
    /// While set, connections are accepted and never answered.
    pub hang: Arc<AtomicBool>,
    /// What to answer with. The camera's refusal is still RTSP.
    pub reply: Arc<Mutex<String>>,
    /// The conformance suite's switches: a fault spoils the reply, and a name
    /// to report goes into a `Server` header.
    pub switches: Switches,
    task: JoinHandle<()>,
}

impl Drop for MockRtsp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockRtsp {
    pub async fn start() -> Self {
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(Mutex::new(Vec::new()));
        let hang = Arc::new(AtomicBool::new(false));
        let reply = Arc::new(Mutex::new("RTSP/1.0 200 OK\r\nCSeq: 1\r\nPublic: DESCRIBE, SETUP\r\n\r\n".to_string()));
        let switches = Switches::default();
        let (c, r, h, a, w) = (connections.clone(), received.clone(), hang.clone(), reply.clone(), switches.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { continue };
                c.fetch_add(1, Ordering::SeqCst);
                let (r, h, a, w) = (r.clone(), h.clone(), a.clone(), w.clone());
                tokio::spawn(async move {
                    let mut buffer = [0u8; 2048];
                    let Ok(length) = stream.read(&mut buffer).await else { return };
                    r.lock().unwrap().push(String::from_utf8_lossy(&buffer[..length]).to_string());
                    if h.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_secs(120)).await;
                    }
                    let mut text = a.lock().unwrap().clone();
                    if w.has_text() {
                        // Into the header block, before the empty line that ends it.
                        let at = text
                            .find(
                                "

",
                            )
                            .map_or(text.len(), |end| end + 2);
                        text.insert_str(
                            at,
                            &format!(
                                "Server: {}
",
                                w.device_text("")
                            ),
                        );
                    }
                    let _ = stream.write_all(&w.garble(text.into_bytes())).await;
                });
            }
        });
        Self { port, connections, received, hang, reply, switches, task }
    }

    pub fn traffic(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

//! Generic devices for a new plugin's tests: a UDP device, a TCP device and a
//! websocket device. Each answers through a handler, records what reached it,
//! counts every datagram or connection, and stops when dropped. The HTTP one is
//! [`MockHttp`](super::MockHttp).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use super::LOOPBACK;

/// What a handler of raw bytes looks like: the bytes that arrived, and what to
/// send back, if anything.
pub type BytesHandler = dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static;

/// What a handler of text frames looks like: the frame that arrived, and what
/// to send back, if anything.
pub type TextHandler = dyn Fn(&str) -> Option<String> + Send + Sync + 'static;

/// A device that speaks UDP datagrams.
pub struct MockUdp {
    /// Where it listens.
    pub addr: SocketAddr,
    /// Every datagram that reached it, oldest first.
    pub datagrams: Arc<Mutex<Vec<Vec<u8>>>>,
    /// While set, datagrams are recorded and never answered.
    pub silent: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for MockUdp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockUdp {
    /// Starts on loopback. `handler` gets each datagram and may answer it.
    pub async fn start(handler: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
        let socket = UdpSocket::bind(LOOPBACK).await.unwrap();
        let addr = socket.local_addr().unwrap();
        let datagrams = Arc::new(Mutex::new(Vec::new()));
        let silent = Arc::new(AtomicBool::new(false));
        let (seen, quiet) = (datagrams.clone(), silent.clone());
        let task = tokio::spawn(async move {
            let mut buffer = vec![0u8; 65_536];
            loop {
                // A closed client makes recv fail on Windows, so errors are skipped.
                let Ok((length, from)) = socket.recv_from(&mut buffer).await else { continue };
                let datagram = buffer[..length].to_vec();
                seen.lock().unwrap().push(datagram.clone());
                if quiet.load(Ordering::SeqCst) {
                    continue;
                }
                if let Some(reply) = handler(&datagram) {
                    let _ = socket.send_to(&reply, from).await;
                }
            }
        });
        Self { addr, datagrams, silent, task }
    }

    /// Every datagram that has reached it.
    pub fn traffic(&self) -> usize {
        self.datagrams.lock().unwrap().len()
    }
}

/// A device that speaks a protocol of its own over TCP. Each read is handed to
/// the handler, and its answer, if any, is written back on the same connection.
/// The connection then stays open for the next request, unless
/// [`close_after_reply`](MockTcp::close_after_reply) is set: a device that sends
/// its answer and closes (an HTTP/1.0 server, a one-shot protocol) is
/// [`MockTcp::start_closing`].
pub struct MockTcp {
    /// Where it listens.
    pub addr: SocketAddr,
    /// Every connection it accepted.
    pub connections: Arc<AtomicUsize>,
    /// Every chunk that was read, oldest first.
    pub received: Arc<Mutex<Vec<Vec<u8>>>>,
    /// While set, connections are accepted and nothing is answered.
    pub hang: Arc<AtomicBool>,
    /// While set, each connection is closed once the chunk it sent has been
    /// answered (or, when the handler has no answer, once it has been read).
    pub close_after_reply: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for MockTcp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockTcp {
    /// Starts on loopback. `handler` gets each chunk read from a connection.
    pub async fn start(handler: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
        Self::listen(handler, false).await
    }

    /// As [`MockTcp::start`], for a device that closes the connection after it has
    /// answered: the client sees the answer and then the end of the stream.
    pub async fn start_closing(handler: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
        Self::listen(handler, true).await
    }

    async fn listen(handler: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static, closing: bool) -> Self {
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(Mutex::new(Vec::new()));
        let hang = Arc::new(AtomicBool::new(false));
        let close_after_reply = Arc::new(AtomicBool::new(closing));
        let handler: Arc<BytesHandler> = Arc::new(handler);
        let (count, seen, quiet) = (connections.clone(), received.clone(), hang.clone());
        let closes = close_after_reply.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { continue };
                count.fetch_add(1, Ordering::SeqCst);
                let (seen, quiet, handler, closes) = (seen.clone(), quiet.clone(), handler.clone(), closes.clone());
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 65_536];
                    loop {
                        let Ok(length) = stream.read(&mut buffer).await else { return };
                        if length == 0 {
                            return;
                        }
                        let chunk = buffer[..length].to_vec();
                        seen.lock().unwrap().push(chunk.clone());
                        if quiet.load(Ordering::SeqCst) {
                            continue;
                        }
                        if let Some(reply) = handler(&chunk) {
                            if stream.write_all(&reply).await.is_err() {
                                return;
                            }
                        }
                        if closes.load(Ordering::SeqCst) {
                            // The answer is written and flushed, then the stream ends: the client reads
                            // the reply and then end of file, as from a device that hangs up.
                            let _ = stream.shutdown().await;
                            return;
                        }
                    }
                });
            }
        });
        Self { addr, connections, received, hang, close_after_reply, task }
    }

    /// Every connection that has reached it.
    pub fn traffic(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// A device that speaks text frames on a plain websocket.
pub struct MockWs {
    /// `ws://` and its address.
    pub url: String,
    /// Every connection it accepted, before or after the handshake.
    pub connections: Arc<AtomicUsize>,
    /// Every text frame that reached it, oldest first.
    pub frames: Arc<Mutex<Vec<String>>>,
    /// While set, frames are recorded and never answered.
    pub hang: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for MockWs {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockWs {
    /// Starts on loopback. `handler` gets each text frame and may answer it.
    pub async fn start(handler: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let connections = Arc::new(AtomicUsize::new(0));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let hang = Arc::new(AtomicBool::new(false));
        let handler: Arc<TextHandler> = Arc::new(handler);
        let (count, seen, quiet) = (connections.clone(), frames.clone(), hang.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                count.fetch_add(1, Ordering::SeqCst);
                let (seen, quiet, handler) = (seen.clone(), quiet.clone(), handler.clone());
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
                    while let Some(Ok(message)) = ws.next().await {
                        let Message::Text(text) = message else { continue };
                        seen.lock().unwrap().push(text.to_string());
                        if quiet.load(Ordering::SeqCst) {
                            continue;
                        }
                        if let Some(reply) = handler(text.as_str()) {
                            if ws.send(Message::text(reply)).await.is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
        Self { url, connections, frames, hang, task }
    }

    /// Every connection that has reached it.
    pub fn traffic(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

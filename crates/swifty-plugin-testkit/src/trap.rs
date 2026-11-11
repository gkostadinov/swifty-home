//! Trap listeners: addresses that must never be contacted. A trap accepts TCP
//! connections and UDP datagrams on one loopback port, answers nothing, and
//! counts everything that reaches it. A test points a config at traps and then
//! asserts that [`Trap::traffic`] is still zero.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinHandle;

/// One address, listening on TCP and UDP at the same port.
pub struct Trap {
    /// The address. TCP and UDP share its port.
    pub addr: SocketAddr,
    /// TCP connections accepted.
    pub connections: Arc<AtomicUsize>,
    /// UDP datagrams received.
    pub datagrams: Arc<AtomicUsize>,
    tasks: [JoinHandle<()>; 2],
}

impl Drop for Trap {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Trap {
    /// A trap on `127.0.0.1` and a port the system picks.
    pub async fn start() -> Self {
        Self::start_on("127.0.0.1").await
    }

    /// A trap on a chosen loopback address, such as `127.0.0.2`, at a port the
    /// system picks that is free for both TCP and UDP.
    pub async fn start_on(ip: &str) -> Self {
        for _ in 0..64 {
            let listener = TcpListener::bind(format!("{ip}:0")).await.unwrap();
            let addr = listener.local_addr().unwrap();
            // The port is free for TCP. It may not be for UDP: then try another.
            let Ok(socket) = UdpSocket::bind(addr).await else { continue };
            return Self::from_parts(addr, listener, socket);
        }
        panic!("no port on {ip} was free for both TCP and UDP");
    }

    fn from_parts(addr: SocketAddr, listener: TcpListener, socket: UdpSocket) -> Self {
        let connections = Arc::new(AtomicUsize::new(0));
        let datagrams = Arc::new(AtomicUsize::new(0));
        let count = connections.clone();
        let tcp = tokio::spawn(async move {
            // Accepted connections are held until the trap stops, and never answered.
            let mut held = Vec::new();
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                count.fetch_add(1, Ordering::SeqCst);
                held.push(stream);
            }
        });
        let count = datagrams.clone();
        let udp = tokio::spawn(async move {
            let mut buffer = vec![0u8; 65_536];
            loop {
                // The trap never sends, so an error here is not a datagram.
                if socket.recv_from(&mut buffer).await.is_ok() {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        Self { addr, connections, datagrams, tasks: [tcp, udp] }
    }

    /// Every connection and every datagram that has reached it.
    pub fn traffic(&self) -> usize {
        self.connections.load(Ordering::SeqCst) + self.datagrams.load(Ordering::SeqCst)
    }
}

/// Several traps, read as one.
#[derive(Default)]
pub struct Traps(pub Vec<Trap>);

impl Traps {
    /// `count` traps on `127.0.0.1`.
    pub async fn start(count: usize) -> Self {
        let mut traps = Vec::with_capacity(count);
        for _ in 0..count {
            traps.push(Trap::start().await);
        }
        Self(traps)
    }

    /// Their addresses, in order.
    pub fn addrs(&self) -> Vec<SocketAddr> {
        self.0.iter().map(|trap| trap.addr).collect()
    }

    /// Everything that has reached any of them.
    pub fn traffic(&self) -> usize {
        self.0.iter().map(Trap::traffic).sum()
    }
}

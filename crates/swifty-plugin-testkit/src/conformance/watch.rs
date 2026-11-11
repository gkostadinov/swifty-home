//! Traps on every loopback address the fixture names, for checks 3 and 4.
//!
//! The fixture's own device already counts what reaches it. Its address may
//! also be reached over the other protocol (UDP to a TCP device, say), and the
//! config may name further addresses. For each loopback address and port found
//! in the config or in the adopted devices' attributes, a TCP listener and a
//! UDP socket are bound where the port is free, and count whatever arrives.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinHandle;

use super::Fixture;

/// Counters on the fixture's other loopback addresses.
pub(super) struct Watch {
    hits: Arc<AtomicUsize>,
    tasks: Vec<JoinHandle<()>>,
    /// The addresses watched, for a problem's text.
    pub(super) addrs: Vec<SocketAddr>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Watch {
    pub(super) async fn start(fixture: &Fixture) -> Self {
        let mut found = BTreeSet::new();
        collect(&fixture.config, &mut found);
        for record in &fixture.adopted {
            collect(&record.attrs, &mut found);
        }
        let hits = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        let mut addrs = Vec::new();
        for addr in found {
            let mut watched = false;
            if let Ok(listener) = TcpListener::bind(addr).await {
                let count = hits.clone();
                tasks.push(tokio::spawn(async move {
                    let mut held = Vec::new();
                    loop {
                        let Ok((stream, _)) = listener.accept().await else { continue };
                        count.fetch_add(1, Ordering::SeqCst);
                        held.push(stream);
                    }
                }));
                watched = true;
            }
            if let Ok(socket) = UdpSocket::bind(addr).await {
                let count = hits.clone();
                tasks.push(tokio::spawn(async move {
                    let mut buffer = vec![0u8; 65_536];
                    loop {
                        if socket.recv_from(&mut buffer).await.is_ok() {
                            count.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }));
                watched = true;
            }
            if watched {
                addrs.push(addr);
            }
        }
        Self { hits, tasks, addrs }
    }

    /// Everything that reached a watched address.
    pub(super) fn traffic(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Loopback socket addresses in a JSON document: `ip:port` strings, URLs with a
/// port, and a bare loopback address paired with every number in the same
/// object that could be a port.
fn collect(value: &Value, found: &mut BTreeSet<SocketAddr>) {
    match value {
        Value::String(text) => {
            if let Some(addr) = socket_addr(text) {
                if addr.ip().is_loopback() {
                    found.insert(addr);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect(item, found)),
        Value::Object(map) => {
            let ports: Vec<u16> = map
                .values()
                .filter_map(Value::as_u64)
                .filter_map(|n| u16::try_from(n).ok())
                .filter(|port| *port != 0)
                .collect();
            for item in map.values() {
                if let Some(ip) = item.as_str().and_then(|text| text.parse::<IpAddr>().ok()) {
                    if ip.is_loopback() {
                        found.extend(ports.iter().map(|port| SocketAddr::new(ip, *port)));
                    }
                }
                collect(item, found);
            }
        }
        _ => {}
    }
}

/// `ip:port`, or the authority of `scheme://ip:port/...`.
fn socket_addr(text: &str) -> Option<SocketAddr> {
    let authority = match text.split_once("://") {
        Some((_, rest)) => rest.split(['/', '?', '#']).next().unwrap_or(""),
        None => text,
    };
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    authority.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loopback_addresses_are_found_in_every_shape() {
        let mut found = BTreeSet::new();
        collect(
            &json!({
                "url": "http://127.0.0.1:8080/api?x=1",
                "peer": "127.0.0.1:5000",
                "rows": [{ "host": "127.0.0.1", "port": 6000, "id": 7 }],
                "far": "192.0.2.1:80",
                "name": "lamp"
            }),
            &mut found,
        );
        let found: Vec<String> = found.iter().map(ToString::to_string).collect();
        assert_eq!(found, vec!["127.0.0.1:7", "127.0.0.1:5000", "127.0.0.1:6000", "127.0.0.1:8080"]);
    }
}

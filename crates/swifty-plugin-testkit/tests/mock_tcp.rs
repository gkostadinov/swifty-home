//! `MockTcp` can stand in for a device that answers and then hangs up, as well as
//! for one that keeps the connection open.

use std::sync::atomic::Ordering;
use std::time::Duration;

use swifty_plugin_testkit::devices::MockTcp;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn echo(chunk: &[u8]) -> Option<Vec<u8>> {
    Some([b"re:".as_slice(), chunk].concat())
}

#[tokio::test]
async fn a_closing_device_answers_and_then_the_stream_ends() {
    let device = MockTcp::start_closing(echo).await;
    let mut client = TcpStream::connect(device.addr).await.unwrap();
    client.write_all(b"ping").await.unwrap();
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut reply))
        .await
        .expect("the device hung up after its answer")
        .unwrap();
    assert_eq!(reply, b"re:ping", "the whole answer arrives before the end of the stream");
    assert_eq!(device.traffic(), 1);
    assert_eq!(device.received.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_ordinary_device_keeps_the_connection_open_for_the_next_request() {
    let device = MockTcp::start(echo).await;
    let mut client = TcpStream::connect(device.addr).await.unwrap();
    for word in [b"one".as_slice(), b"two".as_slice()] {
        client.write_all(word).await.unwrap();
        let mut reply = vec![0u8; 3 + word.len()];
        tokio::time::timeout(Duration::from_secs(5), client.read_exact(&mut reply)).await.unwrap().unwrap();
        assert_eq!(reply, [b"re:".as_slice(), word].concat());
    }
    assert_eq!(device.traffic(), 1, "both requests went over one connection");
    let mut more = [0u8; 1];
    let waited = tokio::time::timeout(Duration::from_millis(300), client.read(&mut more)).await;
    assert!(waited.is_err(), "the device did not hang up: {waited:?}");
}

#[tokio::test]
async fn the_switch_closes_a_device_that_has_nothing_to_say_too() {
    let device = MockTcp::start(|_| None).await;
    device.close_after_reply.store(true, Ordering::SeqCst);
    let mut client = TcpStream::connect(device.addr).await.unwrap();
    client.write_all(b"anything").await.unwrap();
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut reply)).await.unwrap().unwrap();
    assert!(reply.is_empty());
    // A hung device still hears and records everything, and closes after hearing.
    assert_eq!(device.received.lock().unwrap().as_slice(), [b"anything".to_vec()]);
}

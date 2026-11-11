//! Trust on first use: `first_use_pin` learns the certificate a TLS server
//! presents and sends it nothing but the handshake. The servers here are test
//! stand-ins on loopback; the crate's own code opens sockets only through its
//! grant-guarded helpers.

use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use swifty_net::{first_use_pin, ws_connect, DeviceError, LiveGrant, Timeouts};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tokio_rustls::rustls::pki_types::PrivateKeyDer;
use tokio_rustls::rustls::{crypto, ServerConfig};

fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(800), read: Duration::from_millis(800) }
}

/// A TLS server with a self-signed certificate for `tv.test`: its pin, its
/// port, and what each connection sent once the handshake was done.
async fn tls_server() -> (String, u16, tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["tv.test".to_string()]).unwrap().self_signed(&key).unwrap();
    let pin = hex::encode(Sha256::digest(cert.der()));
    let private = PrivateKeyDer::try_from(key.serialize_der()).unwrap();
    let config = ServerConfig::builder_with_provider(Arc::new(crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], private)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sender, received) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (acceptor, sender) = (acceptor.clone(), sender.clone());
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else { return };
                let mut data = Vec::new();
                let _ = timeout(Duration::from_secs(2), tls.read_to_end(&mut data)).await;
                let _ = sender.send(data);
            });
        }
    });
    (pin, port, received)
}

#[tokio::test]
async fn first_use_learns_the_pin_and_sends_nothing_but_the_handshake() {
    let grant = LiveGrant::when_live(true).unwrap();
    let (pin, port, mut received) = tls_server().await;
    assert_eq!(first_use_pin(grant, "127.0.0.1", port, timeouts()).await.unwrap(), pin);
    let data = timeout(Duration::from_secs(3), received.recv()).await.unwrap().unwrap();
    assert!(data.is_empty(), "the server was sent {} bytes after the handshake", data.len());

    // A pinned connection then takes that certificate and no other.
    let url = reqwest::Url::parse(&format!("wss://127.0.0.1:{port}/")).unwrap();
    let other = "0".repeat(64);
    assert!(ws_connect(grant, &url, Some(&other), timeouts()).await.is_err(), "another pin is refused");
}

#[tokio::test]
async fn first_use_of_something_that_is_not_tls_is_an_error() {
    let grant = LiveGrant::when_live(true).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        }
    });
    let error = first_use_pin(grant, "127.0.0.1", port, timeouts()).await.unwrap_err();
    assert!(matches!(error, DeviceError::Unreachable(_)), "{error:?}");
    assert!(matches!(first_use_pin(grant, "not a host name", port, timeouts()).await, Err(DeviceError::Disabled(_))));
}

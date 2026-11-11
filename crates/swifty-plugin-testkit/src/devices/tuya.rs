//! A Tuya smart socket on its local protocol 3.3.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use aes::Aes128;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::LOOPBACK;
use crate::conformance::Switches;

/// A socket's local key. Real ones can hold punctuation, so this does too.
pub const TUYA_KEY: &[u8; 16] = b"t0p$ecret|key!='";
/// That key as its key file holds it.
pub const TUYA_KEY_BASE64: &str = "dDBwJGVjcmV0fGtleSE9Jw==";

// ----- Tuya smart socket

/// What a mock socket does with the next frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuyaMode {
    Normal,
    /// Hears everything and answers nothing.
    Silent,
    /// The reply's checksum is wrong.
    BadCrc,
    /// The reply's header claims far more than any socket sends, and nothing follows.
    Oversized,
    /// The reply does not start with the prefix.
    BadPrefix,
    /// Refuses with this return code.
    Refuses(u32),
    /// Accepts commands and changes nothing.
    Ignores,
}

#[derive(Debug, Clone)]
pub struct TuyaState {
    pub on: bool,
    pub mode: TuyaMode,
    /// Puts "3.3" and twelve zero bytes in front of the encrypted part of a reply.
    pub reply_header: bool,
    /// Answers a command with no return code either.
    pub empty_control_reply: bool,
    /// How long it thinks before answering.
    pub hold_ms: u64,
    /// A USB port on data point 7, if this socket has one.
    pub usb: Option<bool>,
}

/// A socket at the frame level, with its own copy of the crypto, so that a mistake in
/// the adapter's cannot agree with itself.
pub struct MockTuya {
    pub id: String,
    pub port: u16,
    pub state: Arc<Mutex<TuyaState>>,
    /// Every request it could decrypt: the command and its JSON.
    pub requests: Arc<Mutex<Vec<(u32, Value)>>>,
    pub connections: Arc<AtomicUsize>,
    /// The most exchanges it has been in the middle of at once.
    pub most_at_once: Arc<AtomicUsize>,
    /// The conformance suite's switches: a fault spoils each reply frame, and a
    /// name to report is sent as the socket's `devId` in a reading.
    pub switches: Switches,
    task: JoinHandle<()>,
}

impl Drop for MockTuya {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn tuya_seal(key: &[u8; 16], plain: &[u8]) -> Vec<u8> {
    let cipher = Aes128::new(&Array::from(*key));
    let pad = 16 - plain.len() % 16;
    let mut padded = plain.to_vec();
    padded.extend(std::iter::repeat_n(pad as u8, pad));
    let mut out = Vec::new();
    for chunk in padded.chunks(16) {
        let mut block = Array::from(<[u8; 16]>::try_from(chunk).unwrap());
        cipher.encrypt_block(&mut block);
        out.extend_from_slice(&block);
    }
    out
}

fn tuya_open(key: &[u8; 16], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.is_empty() || !sealed.len().is_multiple_of(16) {
        return None;
    }
    let cipher = Aes128::new(&Array::from(*key));
    let mut out = Vec::new();
    for chunk in sealed.chunks(16) {
        let mut block = Array::from(<[u8; 16]>::try_from(chunk).unwrap());
        cipher.decrypt_block(&mut block);
        out.extend_from_slice(&block);
    }
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 16 || !out[out.len() - pad..].iter().all(|b| *b as usize == pad) {
        return None;
    }
    out.truncate(out.len() - pad);
    Some(out)
}

fn tuya_frame(sequence: u32, command: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&0x0000_55AAu32.to_be_bytes());
    frame.extend_from_slice(&sequence.to_be_bytes());
    frame.extend_from_slice(&command.to_be_bytes());
    frame.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    let crc = crc32fast::hash(&frame);
    frame.extend_from_slice(&crc.to_be_bytes());
    frame.extend_from_slice(&0x0000_AA55u32.to_be_bytes());
    frame
}

impl MockTuya {
    pub async fn start(id: &str) -> Self {
        Self::start_with_key(id, TUYA_KEY).await
    }

    pub async fn start_with_key(id: &str, key: &'static [u8; 16]) -> Self {
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(TuyaState {
            on: false,
            mode: TuyaMode::Normal,
            reply_header: false,
            empty_control_reply: false,
            hold_ms: 0,
            usb: None,
        }));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let most_at_once = Arc::new(AtomicUsize::new(0));
        let busy = Arc::new(AtomicUsize::new(0));
        let switches = Switches::default();
        let (s, r, c, m, w) =
            (state.clone(), requests.clone(), connections.clone(), most_at_once.clone(), switches.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { continue };
                c.fetch_add(1, Ordering::SeqCst);
                let (s, r, m, busy, w) = (s.clone(), r.clone(), m.clone(), busy.clone(), w.clone());
                tokio::spawn(async move {
                    Self::serve(&mut stream, key, &s, &r, &m, &busy, &w).await;
                });
            }
        });
        Self { id: id.to_string(), port, state, requests, connections, most_at_once, switches, task }
    }

    async fn serve(
        stream: &mut tokio::net::TcpStream,
        key: &[u8; 16],
        state: &Mutex<TuyaState>,
        requests: &Mutex<Vec<(u32, Value)>>,
        most_at_once: &AtomicUsize,
        busy: &AtomicUsize,
        switches: &Switches,
    ) {
        loop {
            let mut header = [0u8; 16];
            if stream.read_exact(&mut header).await.is_err() {
                return;
            }
            let word = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
            assert_eq!(word(0), 0x0000_55AA, "the service sent a bad prefix");
            let (sequence, command, length) = (word(4), word(8), word(12) as usize);
            let mut body = vec![0u8; length];
            if stream.read_exact(&mut body).await.is_err() {
                return;
            }
            let payload = &body[..length - 8];
            let mut whole = header.to_vec();
            whole.extend_from_slice(payload);
            assert_eq!(
                crc32fast::hash(&whole).to_be_bytes(),
                body[length - 8..length - 4],
                "the service sent a bad checksum"
            );
            assert_eq!(&body[length - 4..], &0x0000_AA55u32.to_be_bytes(), "the service sent a bad suffix");
            most_at_once.fetch_max(busy.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            let hold = state.lock().unwrap().hold_ms;
            tokio::time::sleep(Duration::from_millis(hold)).await;
            let reply = Self::answer(key, state, requests, sequence, command, payload, switches)
                .map(|frame| switches.garble(frame));
            let written = match &reply {
                Some(reply) => stream.write_all(reply).await.is_ok(),
                None => true,
            };
            busy.fetch_sub(1, Ordering::SeqCst);
            if !written {
                return;
            }
        }
    }

    fn answer(
        key: &[u8; 16],
        state: &Mutex<TuyaState>,
        requests: &Mutex<Vec<(u32, Value)>>,
        sequence: u32,
        command: u32,
        payload: &[u8],
        switches: &Switches,
    ) -> Option<Vec<u8>> {
        let mut socket = state.lock().unwrap();
        let sealed = if command == 7 {
            assert_eq!(&payload[..15], b"3.3\0\0\0\0\0\0\0\0\0\0\0\0", "a command starts with the version header");
            &payload[15..]
        } else {
            payload
        };
        let Some(plain) = tuya_open(key, sealed) else {
            // As a real socket does: a return code, and a complaint in clear.
            let mut reply = 1u32.to_be_bytes().to_vec();
            reply.extend_from_slice(b"data format error");
            return Some(tuya_frame(sequence, command, &reply));
        };
        let json: Value = serde_json::from_slice(&plain).unwrap();
        requests.lock().unwrap().push((command, json.clone()));
        match socket.mode {
            TuyaMode::Silent => return None,
            TuyaMode::Oversized => {
                let mut frame = tuya_frame(sequence, command, &[0, 0, 0, 0]);
                frame[12..16].copy_from_slice(&0x0100_0000u32.to_be_bytes());
                frame.truncate(16);
                return Some(frame);
            }
            TuyaMode::Refuses(code) => return Some(tuya_frame(sequence, command, &code.to_be_bytes())),
            _ => {}
        }
        let mut reply = 0u32.to_be_bytes().to_vec();
        match command {
            7 => {
                if socket.mode != TuyaMode::Ignores {
                    if let Some(on) = json["dps"]["1"].as_bool() {
                        socket.on = on;
                    }
                    if let Some(on) = json["dps"]["7"].as_bool() {
                        socket.usb = Some(on);
                    }
                }
                if socket.empty_control_reply {
                    reply.clear();
                }
            }
            10 => {
                if socket.reply_header {
                    reply.extend_from_slice(b"3.3\0\0\0\0\0\0\0\0\0\0\0\0");
                }
                let mut points = json!({ "1": socket.on, "9": 0 });
                if let Some(usb) = socket.usb {
                    points["7"] = json!(usb);
                }
                let mut status = json!({ "dps": points });
                if switches.has_text() {
                    status["devId"] = json!(switches.device_text(""));
                }
                let text = status.to_string();
                reply.extend_from_slice(&tuya_seal(key, text.as_bytes()));
            }
            other => panic!("the service sent command {other}"),
        }
        let mut frame = tuya_frame(sequence, command, &reply);
        let end = frame.len();
        match socket.mode {
            TuyaMode::BadCrc => frame[end - 8] ^= 0xFF,
            TuyaMode::BadPrefix => frame[0] ^= 0xFF,
            _ => {}
        }
        Some(frame)
    }

    pub fn traffic(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub fn target(&self) -> String {
        format!("plug-{}", self.id)
    }
}

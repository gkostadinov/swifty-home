//! The Tuya local protocol, version 3.3, as pure encoding and decoding.
//!
//! A frame is big-endian: prefix, sequence, command, length, payload, CRC-32, suffix.
//! The payload is AES-128-ECB with the socket's own local key. Nothing here opens a
//! socket; `driver.rs` does, through `swifty-net`.
//!
//! Every error is fixed text and at most a number, so neither a key nor anything a
//! socket sent can reach a log or a result.

use aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use aes::Aes128;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::Value;
use swifty_plugin_api::DeviceError;

pub(crate) const PREFIX: u32 = 0x0000_55AA;
pub(crate) const SUFFIX: u32 = 0x0000_AA55;
pub(crate) const CONTROL: u32 = 7;
pub(crate) const DP_QUERY: u32 = 10;
pub(crate) const HEADER_LEN: usize = 16;
/// The CRC and the suffix. The length field counts these as well as the payload.
pub(crate) const TRAILER_LEN: usize = 8;
/// A socket's replies are a few dozen bytes. Anything claiming more than this is refused.
pub(crate) const MAX_BODY: usize = 1024;
/// A socket can push a status update ahead of the reply. This bounds how many are skipped.
pub(crate) const MAX_FRAMES: usize = 4;
pub(crate) const BLOCK: usize = 16;
const VERSION: &[u8] = b"3.3";
/// "3.3" and twelve zero bytes.
pub(crate) const VERSION_HEADER_LEN: usize = 15;
const RETURN_CODE_LEN: usize = 4;
/// The data point of the socket's own switch.
pub(crate) const MAIN_SWITCH: u32 = 1;

pub(crate) fn bad_frame() -> DeviceError {
    DeviceError::Rejected("the socket sent a damaged reply".to_string())
}

pub(crate) fn unreadable() -> DeviceError {
    DeviceError::Rejected("the socket's reply could not be read, so its key may be wrong".to_string())
}

/// The big-endian word at `at`. Callers have already checked the length.
pub(crate) fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

pub(crate) fn build_frame(sequence: u32, command: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len() + TRAILER_LEN);
    frame.extend_from_slice(&PREFIX.to_be_bytes());
    frame.extend_from_slice(&sequence.to_be_bytes());
    frame.extend_from_slice(&command.to_be_bytes());
    frame.extend_from_slice(&((payload.len() + TRAILER_LEN) as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    let crc = crc32fast::hash(&frame);
    frame.extend_from_slice(&crc.to_be_bytes());
    frame.extend_from_slice(&SUFFIX.to_be_bytes());
    frame
}

/// How many bytes follow this header, once the header is known to be sound.
pub(crate) fn body_length(header: &[u8; HEADER_LEN]) -> Result<usize, DeviceError> {
    if word(header, 0) != PREFIX {
        return Err(bad_frame());
    }
    let length = word(header, 12) as usize;
    if !(TRAILER_LEN..=MAX_BODY).contains(&length) {
        return Err(bad_frame());
    }
    Ok(length)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) command: u32,
    pub(crate) payload: Vec<u8>,
}

/// Checks a whole frame and takes out its payload.
pub(crate) fn parse_frame(header: &[u8; HEADER_LEN], body: &[u8]) -> Result<Frame, DeviceError> {
    if body.len() != body_length(header)? {
        return Err(bad_frame());
    }
    let (payload, trailer) = body.split_at(body.len() - TRAILER_LEN);
    if word(trailer, 4) != SUFFIX {
        return Err(bad_frame());
    }
    let mut checked = crc32fast::Hasher::new();
    checked.update(header);
    checked.update(payload);
    if checked.finalize() != word(trailer, 0) {
        return Err(bad_frame());
    }
    Ok(Frame { command: word(header, 8), payload: payload.to_vec() })
}

pub(crate) fn encrypt(cipher: &Aes128, plain: &[u8]) -> Vec<u8> {
    let pad = BLOCK - plain.len() % BLOCK;
    let mut padded = plain.to_vec();
    padded.resize(plain.len() + pad, pad as u8);
    let mut out = Vec::with_capacity(padded.len());
    for chunk in padded.as_chunks::<BLOCK>().0 {
        let mut block = Array::from(*chunk);
        cipher.encrypt_block(&mut block);
        out.extend_from_slice(&block);
    }
    out
}

pub(crate) fn decrypt(cipher: &Aes128, data: &[u8]) -> Result<Vec<u8>, DeviceError> {
    if data.is_empty() || !data.len().is_multiple_of(BLOCK) {
        return Err(unreadable());
    }
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.as_chunks::<BLOCK>().0 {
        let mut block = Array::from(*chunk);
        cipher.decrypt_block(&mut block);
        out.extend_from_slice(&block);
    }
    let pad = out.last().copied().map_or(0, usize::from);
    if pad == 0 || pad > BLOCK || !out[out.len() - pad..].iter().all(|byte| usize::from(*byte) == pad) {
        return Err(unreadable());
    }
    out.truncate(out.len() - pad);
    Ok(out)
}

/// The decrypted JSON in a reply, if it has any. A socket can answer a command with
/// nothing at all, which is why a command is confirmed by asking afterwards.
pub(crate) fn reply_json(cipher: &Aes128, payload: &[u8]) -> Result<Option<Vec<u8>>, DeviceError> {
    if payload.is_empty() {
        return Ok(None);
    }
    if payload.len() < RETURN_CODE_LEN {
        return Err(bad_frame());
    }
    let code = word(payload, 0);
    if code != 0 {
        return Err(DeviceError::Rejected(format!("the socket refused with code {code}")));
    }
    let mut body = &payload[RETURN_CODE_LEN..];
    // Ciphertext is whole blocks, so only a header can leave this remainder.
    if body.len() % BLOCK == VERSION_HEADER_LEN && body.starts_with(VERSION) {
        body = &body[VERSION_HEADER_LEN..];
    }
    if body.is_empty() {
        return Ok(None);
    }
    decrypt(cipher, body).map(Some)
}

pub(crate) fn is_on(json: &[u8], point: u32) -> Result<bool, DeviceError> {
    let value: Value = serde_json::from_slice(json).map_err(|_| unreadable())?;
    value.pointer(&format!("/dps/{point}")).and_then(Value::as_bool).ok_or_else(unreadable)
}

/// "3.3", twelve zero bytes, and the encrypted command.
pub(crate) fn control_payload(cipher: &Aes128, id: &str, point: u32, on: bool, seconds: u64) -> Vec<u8> {
    let json = format!(r#"{{"devId":"{id}","uid":"{id}","t":"{seconds}","dps":{{"{point}":{on}}}}}"#);
    let mut payload = Vec::with_capacity(VERSION_HEADER_LEN + json.len() + BLOCK);
    payload.extend_from_slice(VERSION);
    payload.resize(VERSION_HEADER_LEN, 0);
    payload.extend_from_slice(&encrypt(cipher, json.as_bytes()));
    payload
}

pub(crate) fn query_payload(cipher: &Aes128, id: &str, seconds: u64) -> Vec<u8> {
    let json = format!(r#"{{"gwId":"{id}","devId":"{id}","uid":"{id}","t":"{seconds}"}}"#);
    encrypt(cipher, json.as_bytes())
}

/// The cipher for a stored key: base64 of exactly sixteen bytes, as today's key
/// files hold it. Surrounding white space is ignored.
pub(crate) fn cipher_from_stored(stored: &str) -> Option<Aes128> {
    let bytes = STANDARD.decode(stored.trim().as_bytes()).ok()?;
    let key: [u8; BLOCK] = bytes.as_slice().try_into().ok()?;
    Some(Aes128::new(&Array::from(key)))
}

/// A local key as a person or Tuya's cloud gives it, in the form the secret slot
/// stores: base64 of the sixteen key bytes. Accepts the key as Tuya shows it
/// (sixteen characters) or already in base64. `None` when it is neither.
///
/// The two forms cannot be confused: base64 of sixteen bytes is 24 characters.
pub(crate) fn stored_key(entered: &str) -> Option<String> {
    let entered = entered.trim_matches(['\r', '\n']);
    if entered.len() == BLOCK && !entered.chars().any(char::is_control) {
        return Some(STANDARD.encode(entered.as_bytes()));
    }
    cipher_from_stored(entered).map(|_| entered.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> Aes128 {
        Aes128::new(&Array::from(*b"0123456789abcdef"))
    }

    fn split(frame: &[u8]) -> ([u8; HEADER_LEN], Vec<u8>) {
        let mut header = [0u8; HEADER_LEN];
        header.copy_from_slice(&frame[..HEADER_LEN]);
        (header, frame[HEADER_LEN..].to_vec())
    }

    /// The test vector from the AES standard, FIPS 197 appendix C.1, so the cipher is
    /// checked against something other than itself.
    #[test]
    fn the_cipher_is_the_standard_one() {
        let key: [u8; 16] = std::array::from_fn(|i| i as u8);
        let plain: [u8; 16] = std::array::from_fn(|i| (i as u8) * 0x11);
        let sealed = encrypt(&Aes128::new(&Array::from(key)), &plain);
        assert_eq!(hex::encode(&sealed[..16]), "69c4e0d86a7b0430d8cdb78070b4c55a");
        assert_eq!(sealed.len(), 32, "a whole block of padding follows a whole block");
    }

    #[test]
    fn padding_survives_the_trip_at_every_length() {
        for length in 0..40 {
            let plain = vec![b'x'; length];
            let sealed = encrypt(&cipher(), &plain);
            assert_eq!(sealed.len() % BLOCK, 0);
            assert_eq!(decrypt(&cipher(), &sealed).unwrap(), plain);
        }
    }

    #[test]
    fn a_frame_has_the_documented_layout() {
        let frame = build_frame(9, CONTROL, b"abc");
        assert_eq!(frame.len(), 16 + 3 + 8);
        assert_eq!(&frame[..4], &[0, 0, 0x55, 0xAA]);
        assert_eq!(word(&frame, 4), 9);
        assert_eq!(word(&frame, 8), 7);
        assert_eq!(word(&frame, 12), 11);
        assert_eq!(&frame[frame.len() - 4..], &[0, 0, 0xAA, 0x55]);
        assert_eq!(word(&frame, frame.len() - 8), crc32fast::hash(&frame[..frame.len() - 8]));
        let (header, body) = split(&frame);
        assert_eq!(parse_frame(&header, &body).unwrap(), Frame { command: 7, payload: b"abc".to_vec() });
    }

    #[test]
    fn only_a_sound_frame_is_accepted() {
        let good = build_frame(1, DP_QUERY, b"payload");
        for (what, at) in [("prefix", 0), ("crc", good.len() - 8), ("suffix", good.len() - 1), ("payload", HEADER_LEN)]
        {
            let mut bad = good.clone();
            bad[at] ^= 0x40;
            let (header, body) = split(&bad);
            assert!(parse_frame(&header, &body).is_err(), "{what}");
        }
        let (header, body) = split(&good);
        assert!(parse_frame(&header, &body[..body.len() - 1]).is_err(), "too short for its header");

        let mut huge = good.clone();
        huge[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        let (header, _) = split(&huge);
        assert!(body_length(&header).is_err(), "more than a socket ever sends");
        let mut tiny = good;
        tiny[12..16].copy_from_slice(&7u32.to_be_bytes());
        let (header, _) = split(&tiny);
        assert!(body_length(&header).is_err(), "less than its own trailer");
    }

    /// Whatever arrives, the answer is a result and never a panic.
    #[test]
    fn nothing_a_socket_sends_can_panic_this() {
        let mut seed: u32 = 0x1234_5678;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as u8
        };
        for round in 0..4_000usize {
            let bytes: Vec<u8> = (0..round % 90).map(|_| next()).collect();
            let _ = reply_json(&cipher(), &bytes);
            let _ = decrypt(&cipher(), &bytes);
            let _ = is_on(&bytes, MAIN_SWITCH);
            let mut header = [0u8; HEADER_LEN];
            header.iter_mut().for_each(|slot| *slot = next());
            header[0..4].copy_from_slice(&PREFIX.to_be_bytes());
            let _ = body_length(&header);
            let _ = parse_frame(&header, &bytes);
            header[12..16].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
            let _ = parse_frame(&header, &bytes);
        }
    }

    #[test]
    fn a_reply_is_read_with_or_without_the_version_header() {
        let sealed = encrypt(&cipher(), br#"{"dps":{"1":true}}"#);
        let mut plain = vec![0, 0, 0, 0];
        plain.extend_from_slice(&sealed);
        let mut headed = vec![0, 0, 0, 0];
        headed.extend_from_slice(b"3.3");
        headed.extend_from_slice(&[0; 12]);
        headed.extend_from_slice(&sealed);
        for reply in [plain, headed] {
            assert!(is_on(&reply_json(&cipher(), &reply).unwrap().unwrap(), MAIN_SWITCH).unwrap());
        }
    }

    #[test]
    fn an_empty_reply_is_not_an_error_and_a_return_code_is() {
        assert_eq!(reply_json(&cipher(), &[]).unwrap(), None);
        assert_eq!(reply_json(&cipher(), &[0, 0, 0, 0]).unwrap(), None);
        let refused = reply_json(&cipher(), &[0, 0, 0, 2]).unwrap_err();
        assert!(matches!(&refused, DeviceError::Rejected(text) if text.contains("code 2")), "{refused:?}");
        assert!(reply_json(&cipher(), &[0, 0]).is_err());
    }

    #[test]
    fn the_right_json_goes_out() {
        let usb = control_payload(&cipher(), "abc", 7, false, 77);
        let json = decrypt(&cipher(), &usb[15..]).unwrap();
        assert_eq!(String::from_utf8(json).unwrap(), r#"{"devId":"abc","uid":"abc","t":"77","dps":{"7":false}}"#);
        let control = control_payload(&cipher(), "abc", MAIN_SWITCH, true, 77);
        assert_eq!(&control[..15], b"3.3\0\0\0\0\0\0\0\0\0\0\0\0");
        let json = decrypt(&cipher(), &control[15..]).unwrap();
        assert_eq!(String::from_utf8(json).unwrap(), r#"{"devId":"abc","uid":"abc","t":"77","dps":{"1":true}}"#);
        let query = decrypt(&cipher(), &query_payload(&cipher(), "abc", 77)).unwrap();
        assert_eq!(String::from_utf8(query).unwrap(), r#"{"gwId":"abc","devId":"abc","uid":"abc","t":"77"}"#);
    }

    #[test]
    fn a_key_is_stored_as_base64_whichever_way_it_was_entered() {
        let shown = "0123456789abcdef";
        let stored = stored_key(shown).unwrap();
        assert_eq!(stored, "MDEyMzQ1Njc4OWFiY2RlZg==");
        assert_eq!(stored_key(&stored).unwrap(), stored, "base64 is kept as it is");
        assert_eq!(stored_key(&format!("{stored}\n")).unwrap(), stored, "a pasted line end is dropped");
        let sealed = encrypt(&cipher_from_stored(&stored).unwrap(), b"x");
        assert_eq!(decrypt(&cipher(), &sealed).unwrap(), b"x", "both forms are the same key");
        for bad in ["", "short", "c2hvcnQ=", "not*base64*at*all", "0123456789abcde\u{7}", "0123456789abcdefg"] {
            assert!(stored_key(bad).is_none(), "{bad:?}");
        }
        assert!(cipher_from_stored(" MDEyMzQ1Njc4OWFiY2RlZg==\n").is_some(), "a key file's line end is ignored");
    }
}

//! The probe, as pure functions: the one RTSP request and how its answer is read.
//! RTSP 1.0 is RFC 2326; the OPTIONS method and the `CSeq` header are from there.

/// The probe's request: `OPTIONS` on the camera's root, with no credentials.
pub fn options_request(host: &str, port: u16) -> String {
    format!("OPTIONS rtsp://{host}:{port}/ RTSP/1.0\r\nCSeq: 1\r\nUser-Agent: swifty-home\r\n\r\n")
}

/// Whether an answer is RTSP at all. Any RTSP status, even a refusal, shows the
/// camera is up and speaking RTSP. The answer must be a whole header block: a
/// status line, header lines, and the empty line that ends them, with nothing
/// after it but the body its `Content-Length` announces. Anything else (cut
/// short, spoiled, or followed by more than it said) is not taken for RTSP.
pub fn speaks_rtsp(reply: &[u8]) -> bool {
    let Some(end) = reply.windows(4).position(|window| window == b"\r\n\r\n") else { return false };
    let Ok(head) = std::str::from_utf8(&reply[..end]) else { return false };
    let after = reply.len() - (end + 4);
    let mut lines = head.split("\r\n");
    if !lines.next().is_some_and(is_status_line) {
        return false;
    }
    let mut body = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { return false };
        let name_ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic());
        if !name_ok || value.chars().any(|c| c.is_control() && c != '\t') {
            return false;
        }
        if name.eq_ignore_ascii_case("content-length") {
            match value.trim().parse() {
                Ok(length) => body = length,
                Err(_) => return false,
            }
        }
    }
    after <= body
}

/// `RTSP/1.<digit> <three digits> <reason>`.
fn is_status_line(line: &str) -> bool {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default();
    let reason = parts.next().unwrap_or_default();
    version.strip_prefix("RTSP/1.").is_some_and(|minor| minor.len() == 1 && minor.bytes().all(|b| b.is_ascii_digit()))
        && code.len() == 3
        && code.bytes().all(|b| b.is_ascii_digit())
        && !reason.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_asks_for_options_and_carries_no_credentials() {
        let request = options_request("192.0.2.30", 554);
        assert_eq!(request, "OPTIONS rtsp://192.0.2.30:554/ RTSP/1.0\r\nCSeq: 1\r\nUser-Agent: swifty-home\r\n\r\n");
        assert!(!request.to_lowercase().contains("authorization"));
    }

    #[test]
    fn only_an_rtsp_status_line_counts() {
        assert!(speaks_rtsp(b"RTSP/1.0 200 OK\r\n\r\n"));
        assert!(speaks_rtsp(b"RTSP/1.0 401 Unauthorized\r\n\r\n"));
        assert!(!speaks_rtsp(b"HTTP/1.1 200 OK\r\n\r\n"));
        assert!(!speaks_rtsp(b""));
        assert!(!speaks_rtsp(b"RTSP"));
    }

    #[test]
    fn only_a_whole_header_block_counts() {
        let whole = b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nPublic: DESCRIBE, SETUP\r\n\r\n";
        assert!(speaks_rtsp(whole));
        assert!(!speaks_rtsp(&whole[..whole.len() / 2]), "cut short");
        let mut more = whole.to_vec();
        more.extend_from_slice(b"xxxx");
        assert!(!speaks_rtsp(&more), "more than it announced");
        assert!(speaks_rtsp(b"RTSP/1.0 200 OK\r\nContent-Length: 4\r\n\r\nv=0\n"), "the body it announced");
        assert!(!speaks_rtsp(b"RTSP/1.0 200 OK\r\nServer: a\x07b\r\n\r\n"), "a control character");
        assert!(!speaks_rtsp(b"RTSP/1.0 2x0 OK\r\n\r\n"));
        assert!(!speaks_rtsp(b"RTSP/1.0 200 OK\r\nno colon here\r\n\r\n"));
        assert!(!speaks_rtsp(b"RTSP/1.0 200 O\xff\xff\xff\r\n\r\n"));
    }
}

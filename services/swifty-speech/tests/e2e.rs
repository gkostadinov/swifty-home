//! End-to-end checks against the packaged executable.
//!
//! The test is ignored unless asked for, because it needs the package and the
//! sample clips. It starts the executable as a child process, from a different
//! working directory, with a temporary token file, and stops it afterwards.
//!
//!   SWIFTY_E2E_EXE=dist\swifty-speech\swifty-speech.exe
//!   SWIFTY_E2E_CLIPS=<folder of 16-bit PCM .wav clips>
//!   SWIFTY_E2E_PORT=18790            (optional: first of five ports used)
//!   cargo test --release --test e2e -- --ignored --nocapture

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct Report {
    passed: usize,
    failed: Vec<String>,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Display) {
        println!("[{}] {name}: {detail}", if ok { "PASS" } else { "FAIL" });
        if ok {
            self.passed += 1;
        } else {
            self.failed.push(name.to_string());
        }
    }

    fn status(&mut self, name: &str, reply: &Reply, expected: u16) {
        self.check(
            name,
            reply.status == expected,
            format!("got {} expected {expected} {}", reply.status, reply.short_body()),
        );
    }
}

struct Service {
    child: Child,
    address: SocketAddr,
    log: PathBuf,
}

impl Service {
    fn start(exe: &Path, work: &Path, name: &str, port: u16, extra: &[&str]) -> Self {
        let log = work.join(format!("{name}.log"));
        let out = File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let address: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let child = Command::new(exe)
            .current_dir(work)
            .arg("run")
            .args(["--bind", &address.to_string()])
            .args(extra)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .unwrap();
        Self { child, address, log }
    }

    /// Wait until /healthz answers 200. Returns the statuses seen first.
    fn wait_ready(&mut self) -> Vec<u16> {
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut seen = Vec::new();
        loop {
            assert!(self.child.try_wait().unwrap().is_none(), "the service exited during startup");
            if let Ok(reply) = try_exchange(self.address, get("/healthz", &[]).as_slice()) {
                seen.push(reply.status);
                if reply.status == 200 {
                    return seen;
                }
            }
            assert!(Instant::now() < deadline, "the model never loaded");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn alive(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }

    fn log_text(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn short_body(&self) -> String {
        self.text().chars().take(100).collect()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

fn parse_reply(raw: &[u8]) -> Option<Reply> {
    let mut offset = 0;
    loop {
        let end = raw[offset..].windows(4).position(|w| w == b"\r\n\r\n")? + offset;
        let head = String::from_utf8_lossy(&raw[offset..end]).into_owned();
        let status: u16 = head.split(' ').nth(1)?.parse().ok()?;
        if status == 100 {
            offset = end + 4;
            continue;
        }
        let headers = head
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        return Some(Reply { status, headers, body: raw[end + 4..].to_vec() });
    }
}

/// Send a request and read the reply to the end. A write that fails because
/// the server has already answered is not an error.
fn try_exchange(address: SocketAddr, request: &[u8]) -> std::io::Result<Reply> {
    exchange_on(TcpStream::connect_timeout(&address, Duration::from_secs(5))?, request)
}

fn exchange_on(mut stream: TcpStream, request: &[u8]) -> std::io::Result<Reply> {
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let _ = stream.write_all(request);
    let mut raw = Vec::new();
    let read = stream.read_to_end(&mut raw);
    if raw.is_empty() {
        read?;
    }
    parse_reply(&raw).ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "no HTTP reply"))
}

fn exchange(address: SocketAddr, request: &[u8]) -> Reply {
    try_exchange(address, request).unwrap_or_else(|error| Reply {
        status: 0,
        headers: BTreeMap::new(),
        body: format!("transport error: {error}").into_bytes(),
    })
}

/// The same exchange, from a chosen local address.
fn exchange_from(source: IpAddr, address: SocketAddr, request: &[u8]) -> Reply {
    let connected = (|| {
        let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)?;
        socket.bind(&SocketAddr::new(source, 0).into())?;
        socket.connect_timeout(&address.into(), Duration::from_secs(5))?;
        exchange_on(socket.into(), request)
    })();
    connected.unwrap_or_else(|error| Reply {
        status: 0,
        headers: BTreeMap::new(),
        body: format!("transport error: {error}").into_bytes(),
    })
}

/// Remove inherited entries, so that a file written in the test folder is
/// private however that folder happens to be set up.
fn restrict_to_current_user(path: &Path) {
    let grant = format!("{}:F", std::env::var("USERNAME").unwrap());
    let status = Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &grant])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "could not restrict {}", path.display());
}

fn get(path: &str, headers: &[String]) -> Vec<u8> {
    let extra: String = headers.iter().map(|h| format!("{h}\r\n")).collect();
    format!("GET {path} HTTP/1.1\r\nHost: test\r\n{extra}\r\n").into_bytes()
}

fn post(body: &[u8], headers: &[String]) -> Vec<u8> {
    let extra: String = headers.iter().map(|h| format!("{h}\r\n")).collect();
    let mut request =
        format!("POST /v1/transcribe HTTP/1.1\r\nHost: test\r\nContent-Length: {}\r\n{extra}\r\n", body.len())
            .into_bytes();
    request.extend_from_slice(body);
    request
}

fn bearer(token: &str) -> String {
    format!("Authorization: Bearer {token}")
}

fn wav_type() -> String {
    "Content-Type: audio/wav".to_string()
}

fn authorized(token: &str) -> Vec<String> {
    vec![bearer(token), wav_type()]
}

fn fmt_chunk(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
    let block = channels.wrapping_mul(bits / 8);
    let mut out = b"fmt \x10\0\0\0".to_vec();
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&rate.wrapping_mul(u32::from(block)).to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out
}

fn raw_chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn riff(parts: &[Vec<u8>]) -> Vec<u8> {
    let inner = parts.concat();
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&((inner.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(&inner);
    out
}

fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn pcm_wav(channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
    riff(&[fmt_chunk(1, channels, rate, 16), raw_chunk(b"data", &pcm_bytes(samples))])
}

/// The samples of a well-formed clip from the test set.
fn clip_samples(file: &[u8]) -> (u32, Vec<i16>) {
    let rate = u32::from_le_bytes(file[24..28].try_into().unwrap());
    let at = file.windows(4).position(|w| w == b"data").unwrap();
    let size = u32::from_le_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
    let samples = file[at + 8..at + 8 + size].as_chunks::<2>().0.iter().map(|&pair| i16::from_le_bytes(pair)).collect();
    (rate, samples)
}

fn resample(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    let count = (samples.len() as u64 * u64::from(to) / u64::from(from)) as usize;
    (0..count)
        .map(|index| {
            let position = index as f64 * f64::from(from) / f64::from(to);
            let before = position.floor() as usize;
            let fraction = position - before as f64;
            let a = f64::from(samples[before.min(samples.len() - 1)]);
            let b = f64::from(samples[(before + 1).min(samples.len() - 1)]);
            (a + (b - a) * fraction) as i16
        })
        .collect()
}

fn run_exe(exe: &Path, work: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(exe).current_dir(work).args(args).stdin(Stdio::null()).output().unwrap();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn normalized(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>().to_lowercase()
}

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
}

#[test]
#[ignore = "needs the packaged executable: see the top of this file"]
fn packaged_service() {
    let exe = fs::canonicalize(env_path("SWIFTY_E2E_EXE")).unwrap();
    let clips_dir = env_path("SWIFTY_E2E_CLIPS");
    let base_port: u16 = std::env::var("SWIFTY_E2E_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(18790);

    let work = std::env::temp_dir().join(format!("gs-e2e-{}", std::process::id()));
    fs::create_dir_all(&work).unwrap();
    assert!(work != exe.parent().unwrap(), "the test must run from another directory");

    let mut clips: Vec<PathBuf> = fs::read_dir(&clips_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")))
        .collect();
    clips.sort();
    assert!(!clips.is_empty(), "no test clips found");

    let mut report = Report::default();
    startup_refusals(&mut report, &exe, &work);

    let token_path = work.join("run token");
    let token_arg = token_path.to_string_lossy().into_owned();
    let mut service = Service::start(&exe, &work, "main", base_port, &["--token-file", &token_arg, "--threads", "4"]);

    let during_load = early_requests(&mut service);
    let seen = service.wait_ready();
    let saw_loading = during_load.is_some() || seen.first() == Some(&503);
    report.check("healthz is 503 while the model loads", saw_loading, format!("statuses seen first: {seen:?}"));
    if let Some((health, transcribe)) = during_load {
        report.check(
            "POST during load is 503 with Retry-After",
            transcribe.status == 503 && transcribe.header("retry-after").is_some(),
            format!("{} {}", transcribe.status, transcribe.short_body()),
        );
        report.check("healthz during load has Retry-After", health.header("retry-after").is_some(), "");
    }

    let token = fs::read_to_string(&token_path).unwrap();
    report.check(
        "token file is generated as 64 hex characters",
        token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
        format!("length {}", token.len()),
    );
    show_acl(&token_path, &mut report);

    health_and_auth(&mut report, &service, &token);
    let transcripts = clips_over_http(&mut report, &service, &token, &clips);
    half_closed_client(&mut report, &service, &token, &clips[0]);
    transcribe_command(&mut report, &exe, &work, &clips, &transcripts);
    formats(&mut report, &service, &token, &clips[0], &transcripts[0]);
    error_statuses(&mut report, &service, &token, &clips[0]);
    tiny_clips(&mut report, &mut service, &token);
    slow_clients(&mut report, &service, &token);
    connection_limit(&mut report, &service, &token);
    queue_limit(&mut report, &mut service, &token, &clips);

    report.check("service is still running after every test", service.alive(), "");
    turned_away_connections_are_counted(&mut report, &service);
    log_privacy(&mut report, &service, &token, &transcripts, &clips);
    service.stop();
    report.check(
        "port is released after stopping",
        TcpStream::connect_timeout(&service.address, Duration::from_secs(2)).is_err(),
        "connection refused",
    );

    allow_lists(&mut report, &exe, &work, base_port + 1, &token_arg);
    refused_address_cannot_block_a_client(&mut report, &exe, &work, base_port + 4, &token_arg, &clips[0]);
    graceful_stop(&mut report, &exe, &work, base_port + 3, &token_arg);

    println!("\n{} passed, {} failed", report.passed, report.failed.len());
    for name in &report.failed {
        println!("  failed: {name}");
    }
    let _ = fs::remove_dir_all(&work);
    assert!(report.failed.is_empty(), "{} end-to-end checks failed", report.failed.len());
}

fn early_requests(service: &mut Service) -> Option<(Reply, Reply)> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(health) = try_exchange(service.address, &get("/healthz", &[])) {
            if health.status != 503 {
                return None;
            }
            let body = vec![0_u8; 16];
            // The token is not known to be readable yet, so this one is unauthenticated
            // on purpose unless the file already exists.
            let token = fs::read_to_string(service.log.with_file_name("run token")).unwrap_or_default();
            let transcribe = exchange(service.address, &post(&body, &authorized(&token)));
            return Some((health, transcribe));
        }
        thread::sleep(Duration::from_millis(5));
    }
    None
}

fn startup_refusals(report: &mut Report, exe: &Path, work: &Path) {
    let (code, out, _) = run_exe(exe, work, &["--version"]);
    report.check("--version", code == Some(0) && out.starts_with("swifty-speech "), out);

    let refuse = |report: &mut Report, name: &str, contents: &[u8], expected: &str| {
        let path = work.join(format!("{name}.token"));
        fs::write(&path, contents).unwrap();
        restrict_to_current_user(&path);
        let (code, out, err) =
            run_exe(exe, work, &["run", "--bind", "127.0.0.1:0", "--token-file", path.to_str().unwrap()]);
        let value = String::from_utf8_lossy(contents);
        let leaked = !value.trim().is_empty() && err.contains(value.trim());
        report.check(
            &format!("refuses to start with a {name} token file"),
            code == Some(1) && err.contains(expected) && !leaked && out.is_empty(),
            &err,
        );
    };
    refuse(report, "empty", b"", "empty");
    refuse(report, "whitespace-only", b"  \r\n", "empty");
    refuse(report, "short", b"abc123", "too short");

    let (code, _, err) = run_exe(exe, work, &["run", "--bind", "127.0.0.1:0"]);
    report.check(
        "ordinary user without --token-file gets a clear failure",
        code == Some(1) && err.contains("--token-file") && err.contains("administrator"),
        &err,
    );

    let token = work.join("model.token");
    fs::write(&token, "c".repeat(64)).unwrap();
    restrict_to_current_user(&token);
    let token_arg = token.to_str().unwrap();
    let (code, _, err) = run_exe(
        exe,
        work,
        &["run", "--bind", "127.0.0.1:0", "--token-file", token_arg, "--model", work.to_str().unwrap()],
    );
    report.check("a missing model is a clear failure", code == Some(1) && err.contains("encoder.int8.onnx"), &err);

    let shared = work.join("everyone.token");
    fs::write(&shared, "d".repeat(64)).unwrap();
    restrict_to_current_user(&shared);
    let status =
        Command::new("icacls").arg(&shared).args(["/grant", "Everyone:R"]).stdout(Stdio::null()).status().unwrap();
    assert!(status.success());
    let (code, _, err) =
        run_exe(exe, work, &["run", "--bind", "127.0.0.1:0", "--token-file", shared.to_str().unwrap()]);
    report.check(
        "a token file readable by everyone is refused, naming only the path",
        code == Some(1) && err.contains("not private") && err.contains("everyone.token") && !err.contains("dddd"),
        &err,
    );

    let bad_model = work.join("bad model");
    fs::create_dir_all(&bad_model).unwrap();
    for name in ["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx"] {
        fs::write(bad_model.join(name), b"x").unwrap();
    }
    fs::write(bad_model.join("tokens.txt"), b"a b c\n").unwrap();
    let model_arg = bad_model.to_str().unwrap();
    let (code, _, err) =
        run_exe(exe, work, &["run", "--bind", "127.0.0.1:0", "--token-file", token_arg, "--model", model_arg]);
    report.check("run reports a malformed tokens.txt and exits 1", code == Some(1) && err.contains("tokens.txt"), &err);
    let clip = work.join("short.wav");
    fs::write(&clip, pcm_wav(1, 16_000, &[0; 1600])).unwrap();
    let (code, _, err) = run_exe(exe, work, &["transcribe", clip.to_str().unwrap(), "--model", model_arg]);
    report.check(
        "transcribe reports a malformed tokens.txt and exits 1",
        code == Some(1) && err.contains("tokens.txt"),
        &err,
    );
}

fn show_acl(path: &Path, report: &mut Report) {
    let output = Command::new("icacls").arg(path).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let entries: Vec<&str> = text.lines().filter(|l| l.contains(":(")).collect();
    let ok = entries.len() == 3
        && text.contains("NT AUTHORITY\\SYSTEM:(F)")
        && text.contains("BUILTIN\\Administrators:(F)")
        && !text.contains("(I)")
        && !text.contains("Users:")
        && !text.contains("Everyone");
    report.check(
        "token file ACL is the user, Administrators and SYSTEM, not inherited",
        ok,
        text.replace("\r\n", " | "),
    );
}

fn health_and_auth(report: &mut Report, service: &Service, token: &str) {
    let address = service.address;
    let health = exchange(address, &get("/healthz", &[]));
    report.status("GET /healthz without a token", &health, 200);
    report.check("/healthz body is exactly {\"status\":\"ok\"}", health.body == br#"{"status":"ok"}"#, health.text());

    let clip = pcm_wav(1, 16_000, &[0; 1600]);
    let wrong_same_length = "0".repeat(64);
    let cases: [(&str, Vec<String>); 7] = [
        ("no Authorization header", vec![wav_type()]),
        ("empty bearer", vec!["Authorization: Bearer ".to_string(), wav_type()]),
        ("wrong token of the same length", vec![bearer(&wrong_same_length), wav_type()]),
        ("token one character short", vec![bearer(&token[..63]), wav_type()]),
        ("token with an extra character", vec![bearer(&format!("{token}0")), wav_type()]),
        ("Basic scheme", vec![format!("Authorization: Basic {token}"), wav_type()]),
        ("token in another header", vec![format!("X-Token: {token}"), wav_type()]),
    ];
    for (name, headers) in cases {
        let reply = exchange(address, &post(&clip, &headers));
        report.status(&format!("401: {name}"), &reply, 401);
    }
    let reply = exchange(address, &get("/does-not-exist", &[]));
    report.status("401: unknown path without a token", &reply, 401);
    let reply = exchange(address, &get("/does-not-exist", &[bearer(token)]));
    report.status("404: unknown path with a token", &reply, 404);
    let reply = exchange(address, &get("/v1/transcribe", &[bearer(token)]));
    report.status("405: GET /v1/transcribe with a token", &reply, 405);
    let reply = exchange(address, &post(&clip, &[format!("authorization: bearer {token}"), wav_type()]));
    report.status("200: scheme and header name are case-insensitive", &reply, 200);

    // The answer comes before the body is read, and must still reach the client.
    let large = vec![0_u8; 5 * 1024 * 1024];
    let reply = exchange(address, &post(&large, &[wav_type()]));
    report.status("401: refused with a 5 MiB body already on the wire", &reply, 401);
}

fn clips_over_http(report: &mut Report, service: &Service, token: &str, clips: &[PathBuf]) -> Vec<String> {
    let mut transcripts = Vec::new();
    for clip in clips {
        let name = clip.file_name().unwrap().to_string_lossy().into_owned();
        let body = fs::read(clip).unwrap();
        let reply = exchange(service.address, &post(&body, &authorized(token)));
        let value = reply.json();
        let text = value["text"].as_str().unwrap_or("").to_string();
        let keys_ok = value.as_object().is_some_and(|o| {
            o.len() == 3 && o.contains_key("text") && o["audio_ms"].is_u64() && o["decode_ms"].is_u64()
        });
        let (rate, samples) = clip_samples(&body);
        let expected_ms = samples.len() as u64 * 1000 / u64::from(rate);
        let ok = reply.status == 200 && keys_ok && !text.is_empty() && value["audio_ms"].as_u64() == Some(expected_ms);
        report.check(
            &format!("clip {name}"),
            ok,
            format!("{} audio_ms={} decode_ms={} \"{text}\"", reply.status, value["audio_ms"], value["decode_ms"]),
        );
        transcripts.push(text);
    }
    transcripts
}

fn half_closed_client(report: &mut Report, service: &Service, token: &str, clip: &Path) {
    let mut stream = TcpStream::connect(service.address).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    stream.write_all(&post(&fs::read(clip).unwrap(), &authorized(token))).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let status = parse_reply(&raw).map_or(0, |reply| reply.status);
    report.check("200: a client that half-closes after its request gets the answer", status == 200, status);
}

fn transcribe_command(report: &mut Report, exe: &Path, work: &Path, clips: &[PathBuf], transcripts: &[String]) {
    for (clip, expected) in clips.iter().zip(transcripts) {
        let name = clip.file_name().unwrap().to_string_lossy().into_owned();
        let (code, out, err) = run_exe(exe, work, &["transcribe", clip.to_str().unwrap()]);
        report.check(
            &format!("transcribe command matches HTTP for {name}"),
            code == Some(0) && &out == expected,
            format!("\"{out}\" {err}"),
        );
    }
    let bad = work.join("bad clip.wav");
    fs::write(&bad, b"not a wav").unwrap();
    let (code, _, err) = run_exe(exe, work, &["transcribe", bad.to_str().unwrap()]);
    report.check("transcribe refuses a malformed file", code == Some(1) && err.contains("malformed"), &err);
    let (code, _, err) = run_exe(exe, work, &["transcribe", "does-not-exist.wav"]);
    report.check("transcribe reports a missing file", code == Some(1) && err.contains("does-not-exist.wav"), &err);
}

fn formats(report: &mut Report, service: &Service, token: &str, clip: &Path, reference: &str) {
    let file = fs::read(clip).unwrap();
    let (rate, mono) = clip_samples(&file);
    let reference = normalized(reference);
    let heard_same = |text: &str| normalized(text) == reference;

    let try_body = |report: &mut Report, name: &str, body: Vec<u8>, compare: bool| {
        let reply = exchange(service.address, &post(&body, &authorized(token)));
        let text = reply.json()["text"].as_str().unwrap_or("").to_string();
        let ok = reply.status == 200 && !text.is_empty() && (!compare || heard_same(&text));
        report.check(name, ok, format!("{} \"{text}\"", reply.status));
    };

    let stereo_same: Vec<i16> = mono.iter().flat_map(|&s| [s, s]).collect();
    try_body(report, "stereo with both channels the same", pcm_wav(2, rate, &stereo_same), true);
    let stereo_one: Vec<i16> = mono.iter().flat_map(|&s| [s, 0]).collect();
    try_body(report, "stereo with one silent channel is mixed", pcm_wav(2, rate, &stereo_one), false);
    for target in [8_000, 11_025, 22_050, 32_000, 44_100, 48_000] {
        let resampled = resample(&mono, rate, target);
        try_body(report, &format!("{target} Hz mono"), pcm_wav(1, target, &resampled), false);
    }
    let stereo_48k: Vec<i16> = resample(&mono, rate, 48_000).iter().flat_map(|&s| [s, s]).collect();
    try_body(report, "48000 Hz stereo", pcm_wav(2, 48_000, &stereo_48k), false);

    let odd_list = riff(&[
        raw_chunk(b"LIST", b"odd"),
        fmt_chunk(1, 1, rate, 16),
        raw_chunk(b"JUNK", b"x"),
        raw_chunk(b"data", &pcm_bytes(&mono)),
    ]);
    try_body(report, "odd-sized chunks with padding before the audio", odd_list, true);

    let mut wrong_riff_size = pcm_wav(1, rate, &mono);
    wrong_riff_size[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    try_body(report, "wrong size in the RIFF header is ignored", wrong_riff_size, true);
}

fn error_statuses(report: &mut Report, service: &Service, token: &str, clip: &Path) {
    let address = service.address;
    let headers = authorized(token);
    let send = |body: &[u8]| exchange(address, &post(body, &headers));
    let quarter_second = vec![100_i16; 4000];
    let valid = pcm_wav(1, 16_000, &quarter_second);

    let mut declared_too_big = valid.clone();
    let data_at = declared_too_big.windows(4).position(|w| w == b"data").unwrap();
    declared_too_big[data_at + 4..data_at + 8].copy_from_slice(&1_000_000_u32.to_le_bytes());
    let mut declared_max = valid.clone();
    declared_max[data_at + 4..data_at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut fmt_max = valid.clone();
    let fmt_at = fmt_max.windows(4).position(|w| w == b"fmt ").unwrap();
    fmt_max[fmt_at + 4..fmt_at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut truncated = valid.clone();
    truncated.truncate(valid.len() / 2);

    let malformed: [(&str, Vec<u8>); 12] = [
        ("empty body", vec![]),
        ("text instead of audio", b"this is not audio at all".to_vec()),
        ("RIFF header only", b"RIFF\0\0\0\0WAVE".to_vec()),
        ("zero channels", riff(&[fmt_chunk(1, 0, 16_000, 16), raw_chunk(b"data", &[0; 8])])),
        ("zero sample rate", riff(&[fmt_chunk(1, 1, 0, 16), raw_chunk(b"data", &[0; 8])])),
        ("zero bits per sample", riff(&[fmt_chunk(1, 1, 16_000, 0), raw_chunk(b"data", &[0; 8])])),
        ("data size beyond the file", declared_too_big),
        ("data size 0xFFFFFFFF", declared_max),
        ("fmt size 0xFFFFFFFF", fmt_max),
        ("truncated file", truncated),
        ("data before fmt", riff(&[raw_chunk(b"data", &[0; 8]), fmt_chunk(1, 1, 16_000, 16)])),
        ("odd number of data bytes", riff(&[fmt_chunk(1, 1, 16_000, 16), raw_chunk(b"data", &[0; 7])])),
    ];
    for (name, body) in malformed {
        report.status(&format!("400: {name}"), &send(&body), 400);
    }

    let unsupported: [(&str, Vec<u8>); 8] = [
        ("8-bit PCM", riff(&[fmt_chunk(1, 1, 16_000, 8), raw_chunk(b"data", &[0; 8])])),
        ("24-bit PCM", riff(&[fmt_chunk(1, 1, 16_000, 24), raw_chunk(b"data", &[0; 6])])),
        ("32-bit float", riff(&[fmt_chunk(3, 1, 16_000, 32), raw_chunk(b"data", &[0; 8])])),
        ("a-law", riff(&[fmt_chunk(6, 1, 8_000, 8), raw_chunk(b"data", &[0; 8])])),
        ("three channels", riff(&[fmt_chunk(1, 3, 16_000, 16), raw_chunk(b"data", &[0; 12])])),
        ("sample rate below 8 kHz", pcm_wav(1, 7_999, &[0; 16])),
        ("sample rate above 48 kHz", pcm_wav(1, 48_001, &[0; 16])),
        ("RIFX", b"RIFX\0\0\0\0WAVE".to_vec()),
    ];
    for (name, body) in unsupported {
        report.status(&format!("415: {name}"), &send(&body), 415);
    }

    let reply = exchange(address, &post(&valid, &[bearer(token), "Content-Type: text/plain".to_string()]));
    report.status("415: wrong Content-Type", &reply, 415);
    let reply = exchange(address, &post(&valid, &[bearer(token)]));
    report.status("415: no Content-Type", &reply, 415);

    report.status("422: no audio frames", &send(&pcm_wav(1, 16_000, &[])), 422);
    let thirty = vec![0_i16; 30 * 8_000];
    let mut thirty_one_frame_more = thirty.clone();
    thirty_one_frame_more.push(0);
    report.status("422: one frame over 30 seconds", &send(&pcm_wav(1, 8_000, &thirty_one_frame_more)), 422);
    report.status("422: 31 seconds of stereo", &send(&pcm_wav(2, 16_000, &vec![0; 31 * 16_000 * 2])), 422);
    report.status("200: exactly 30 seconds", &send(&pcm_wav(1, 8_000, &thirty)), 200);

    // Size limits. A JUNK chunk pads a real clip to an exact body size.
    let clip_file = fs::read(clip).unwrap();
    let (rate, samples) = clip_samples(&clip_file);
    let exact = |size: usize| {
        let format = fmt_chunk(1, 1, rate, 16);
        let data = raw_chunk(b"data", &pcm_bytes(&samples));
        let junk_body = size - 12 - format.len() - data.len() - 8;
        assert_eq!(junk_body % 2, 0);
        riff(&[raw_chunk(b"JUNK", &vec![0; junk_body]), format, data])
    };
    let at_limit = exact(MAX_BODY);
    assert_eq!(at_limit.len(), MAX_BODY);
    report.status("200: body of exactly 8 MiB", &send(&at_limit), 200);

    let mut over = at_limit.clone();
    over.push(0);
    report.status("413: 8 MiB + 1 byte, whole body sent", &send(&over), 413);
    let huge = vec![0_u8; 9 * 1024 * 1024];
    report.status("413: 9 MiB, whole body sent", &send(&huge), 413);

    let mut with_expect = authorized(token);
    with_expect.push("Expect: 100-continue".to_string());
    let head_only = format!(
        "POST /v1/transcribe HTTP/1.1\r\nHost: test\r\nContent-Length: {}\r\n{}\r\n\r\n",
        MAX_BODY + 1,
        with_expect.join("\r\n")
    );
    report.status(
        "413: declared size is refused before the body is sent",
        &exchange(address, head_only.as_bytes()),
        413,
    );

    // Chunked encoding has no declared size, so the limit applies while reading.
    let mut chunked = format!(
        "POST /v1/transcribe HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\n{}\r\n\r\n",
        authorized(token).join("\r\n")
    )
    .into_bytes();
    for _ in 0..9 {
        chunked.extend_from_slice(b"100000\r\n");
        chunked.extend_from_slice(&vec![0_u8; 1024 * 1024]);
        chunked.extend_from_slice(b"\r\n");
    }
    chunked.extend_from_slice(b"0\r\n\r\n");
    report.status("413: 9 MiB chunked body", &exchange(address, &chunked), 413);

    let mut chunked_ok = format!(
        "POST /v1/transcribe HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\n{}\r\n\r\n",
        authorized(token).join("\r\n")
    )
    .into_bytes();
    chunked_ok.extend_from_slice(format!("{:x}\r\n", clip_file.len()).as_bytes());
    chunked_ok.extend_from_slice(&clip_file);
    chunked_ok.extend_from_slice(b"\r\n0\r\n\r\n");
    report.status("200: chunked body within the limit", &exchange(address, &chunked_ok), 200);

    let mut expecting = authorized(token);
    expecting.push("Expect: 100-continue".to_string());
    report.status("200: upload with Expect: 100-continue", &exchange(address, &post(&at_limit, &expecting)), 200);

    let reply = exchange(address, b"NOT HTTP AT ALL\r\n\r\n");
    report.check(
        "garbage instead of HTTP does not reach a handler",
        reply.status == 400 || reply.status == 0,
        format!("{}", reply.status),
    );
}

fn tiny_clips(report: &mut Report, service: &mut Service, token: &str) {
    for (name, samples) in [
        ("1 sample", vec![500_i16; 1]),
        ("10 ms", vec![500; 160]),
        ("25 ms", vec![500; 400]),
        ("50 ms", vec![500; 800]),
        ("100 ms", vec![500; 1600]),
    ] {
        let reply = exchange(service.address, &post(&pcm_wav(1, 16_000, &samples), &authorized(token)));
        report.check(
            &format!("very short clip: {name}"),
            reply.status == 200 && service.alive(),
            format!("{} {}", reply.status, reply.short_body()),
        );
    }

    // One frame of audio at each rate is what used to abort the process.
    let mut statuses = Vec::new();
    for rate in [8_000, 11_025, 16_000, 22_050, 44_100, 48_000] {
        for length in [1, 2, 3, 7, 40, 79, 80, 81, 100, 160, 400, 1_000] {
            for channels in [1, 2] {
                let samples = vec![300_i16; length * usize::from(channels)];
                let reply = exchange(service.address, &post(&pcm_wav(channels, rate, &samples), &authorized(token)));
                statuses.push(reply.status);
            }
        }
    }
    report.check(
        "every short length at every rate, mono and stereo",
        statuses.iter().all(|&s| s == 200) && service.alive(),
        format!(
            "{} requests, statuses {:?}",
            statuses.len(),
            statuses.iter().collect::<std::collections::BTreeSet<_>>()
        ),
    );

    let mut random = 0x1234_5678_9ABC_DEF0_u64;
    let noise: Vec<i16> = (0..32_000)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random as i16
        })
        .collect();
    let square: Vec<i16> = (0..32_000).map(|i| if i % 40 < 20 { i16::MAX } else { i16::MIN }).collect();
    for (name, samples) in [
        ("two seconds of silence", vec![0_i16; 32_000]),
        ("two seconds of a constant offset", vec![i16::MAX; 32_000]),
        ("two seconds of full-scale noise", noise),
        ("two seconds of a full-scale square wave", square),
    ] {
        let reply = exchange(service.address, &post(&pcm_wav(1, 16_000, &samples), &authorized(token)));
        report.check(name, reply.status == 200 && service.alive(), format!("{} {}", reply.status, reply.short_body()));
    }
}

fn slow_clients(report: &mut Report, service: &Service, token: &str) {
    let address = service.address;

    let idle = thread::spawn(move || {
        let started = Instant::now();
        let mut stream = TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        started.elapsed()
    });

    let partial_headers = thread::spawn(move || {
        let started = Instant::now();
        let mut stream = TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
        stream.write_all(b"POST /v1/transcribe HTTP/1.1\r\nHost: te").unwrap();
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        started.elapsed()
    });

    // Sends a byte now and then: the deadline must not start over.
    let dribbling = thread::spawn(move || {
        let started = Instant::now();
        let mut stream = TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
        for byte in b"POST /v1/transcribe HTTP/1.1\r\nHost: test\r\n" {
            if stream.write_all(&[*byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(400));
        }
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        started.elapsed()
    });

    let started = Instant::now();
    let mut stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
    let head = format!(
        "POST /v1/transcribe HTTP/1.1\r\nHost: test\r\nContent-Length: 100000\r\n{}\r\n\r\nRIFF",
        authorized(token).join("\r\n")
    );
    stream.write_all(head.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let elapsed = started.elapsed();
    let reply = parse_reply(&raw);
    let status = reply.as_ref().map_or(0, |r| r.status);
    report.check(
        "408: body read timeout after 15 seconds",
        status == 408 && (14..=19).contains(&elapsed.as_secs()),
        format!("status {status} after {:.1} s", elapsed.as_secs_f32()),
    );

    let in_time = |elapsed: Duration| (4..=7).contains(&elapsed.as_secs());
    let elapsed = idle.join().unwrap();
    report.check(
        "a connection that sends nothing is closed at the 5 s deadline",
        in_time(elapsed),
        format!("after {:.1} s", elapsed.as_secs_f32()),
    );
    let elapsed = partial_headers.join().unwrap();
    report.check(
        "incomplete headers are closed at the 5 s deadline",
        in_time(elapsed),
        format!("after {:.1} s", elapsed.as_secs_f32()),
    );
    let elapsed = dribbling.join().unwrap();
    report.check(
        "dribbled headers are closed at the 5 s deadline",
        in_time(elapsed),
        format!("after {:.1} s", elapsed.as_secs_f32()),
    );
}

fn wait_until_healthy(address: SocketAddr) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut status = 0;
    while Instant::now() < deadline && status != 200 {
        thread::sleep(Duration::from_millis(20));
        status = exchange(address, &get("/healthz", &[])).status;
    }
    status
}

fn connection_limit(report: &mut Report, service: &Service, token: &str) {
    let address = service.address;

    let idle: Vec<TcpStream> = (0..8).map(|_| TcpStream::connect(address).unwrap()).collect();
    thread::sleep(Duration::from_millis(300));
    let reply = exchange(address, &get("/healthz", &[]));
    report.check(
        "503 with Retry-After beyond 8 connections from one source without complete headers",
        reply.status == 503 && reply.header("retry-after").is_some(),
        format!("{} {}", reply.status, reply.short_body()),
    );
    drop(idle);
    let status = wait_until_healthy(address);
    report.check("those connections are released when the client leaves", status == 200, status);

    // Complete headers and a body still to come: each of these holds a slot.
    let head = format!(
        "POST /v1/transcribe HTTP/1.1\r\nHost: test\r\nContent-Length: 1000\r\n{}\r\n\r\n",
        authorized(token).join("\r\n")
    );
    let held: Vec<TcpStream> = (0..32)
        .map(|_| {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(head.as_bytes()).unwrap();
            stream
        })
        .collect();
    thread::sleep(Duration::from_millis(300));
    let reply = exchange(address, &get("/healthz", &[]));
    report.check(
        "503 with Retry-After beyond 32 open connections",
        reply.status == 503 && reply.header("retry-after").is_some(),
        format!("{} {}", reply.status, reply.short_body()),
    );

    // The 503 must reach clients whose upload the server never reads.
    for size in [0_usize, 8 * 1024, 160 * 1024, 960 * 1024] {
        let body = Arc::new(vec![0_u8; size]);
        let headers = Arc::new(authorized(token));
        let senders: Vec<_> = (0..40)
            .map(|_| {
                let (body, headers) = (body.clone(), headers.clone());
                thread::spawn(move || exchange(address, &post(&body, &headers)))
            })
            .collect();
        let replies: Vec<Reply> = senders.into_iter().map(|sender| sender.join().unwrap()).collect();
        let good = replies.iter().filter(|r| r.status == 503 && r.header("retry-after").is_some()).count();
        let others: Vec<u16> = replies.iter().filter(|r| r.status != 503).map(|r| r.status).collect();
        report.check(
            &format!("every one of 40 refused uploads of {size} bytes reads a 503"),
            good == 40,
            format!("{good} of 40, others {others:?}"),
        );
    }

    drop(held);
    let status = wait_until_healthy(address);
    report.check("connections are released when the clients leave", status == 200, status);
}

fn turned_away_connections_are_counted(report: &mut Report, service: &Service) {
    // The report is written once a minute.
    let deadline = Instant::now() + Duration::from_secs(70);
    loop {
        let text = service.log_text();
        let line = text.lines().find(|line| line.contains(" connections refused_address="));
        if line.is_some() || Instant::now() >= deadline {
            report.check(
                "refused and timed-out connections are counted in the log",
                line.is_some(),
                line.unwrap_or("no line"),
            );
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(windows)]
fn graceful_stop(report: &mut Report, exe: &Path, work: &Path, port: u16, token_arg: &str) {
    use std::os::windows::process::CommandExt;

    extern "system" {
        fn GenerateConsoleCtrlEvent(event: u32, process_group: u32) -> i32;
    }
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
    const CTRL_BREAK_EVENT: u32 = 1;

    let log = work.join("graceful.log");
    let out = File::create(&log).unwrap();
    let err = out.try_clone().unwrap();
    let address: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let child = Command::new(exe)
        .current_dir(work)
        .args(["run", "--bind", &address.to_string(), "--token-file", token_arg])
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .unwrap();
    let mut service = Service { child, address, log };
    service.wait_ready();

    // The new process group contains only the service, so nothing else is signalled.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, service.child.id()) } != 0;
    if !sent {
        println!("[SKIP] graceful stop: this test run has no console to signal through");
        service.stop();
        return;
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut code = None;
    while Instant::now() < deadline {
        if let Some(status) = service.child.try_wait().unwrap() {
            code = status.code();
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let log_says_stopped = service.log_text().contains(" stopped");
    report.check(
        "Ctrl+Break stops the service cleanly",
        code == Some(0) && log_says_stopped,
        format!("exit code {code:?}"),
    );
}

#[cfg(not(windows))]
fn graceful_stop(_report: &mut Report, _exe: &Path, _work: &Path, _port: u16, _token_arg: &str) {}

fn queue_limit(report: &mut Report, service: &mut Service, token: &str, clips: &[PathBuf]) {
    // A clip of 29 seconds keeps the worker busy long enough to fill the queue.
    let rate = clip_samples(&fs::read(&clips[0]).unwrap()).0;
    let wanted = 29 * rate as usize;
    let mut long = Vec::new();
    while long.len() < wanted {
        for clip in clips {
            long.extend_from_slice(&clip_samples(&fs::read(clip).unwrap()).1);
        }
    }
    long.truncate(wanted);
    let long_clip = Arc::new(pcm_wav(1, rate, &long));
    let headers = Arc::new(authorized(token));

    for round in 1..=2 {
        let count = 16;
        let barrier = Arc::new(Barrier::new(count));
        let address = service.address;
        let workers: Vec<_> = (0..count)
            .map(|_| {
                let barrier = barrier.clone();
                let clip = long_clip.clone();
                let headers = headers.clone();
                thread::spawn(move || {
                    barrier.wait();
                    exchange(address, &post(&clip, &headers))
                })
            })
            .collect();

        // While the worker is saturated, health must keep answering.
        thread::sleep(Duration::from_millis(300));
        let health = exchange(address, &get("/healthz", &[]));
        report.status(&format!("round {round}: healthz stays up while saturated"), &health, 200);

        let replies: Vec<Reply> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
        let accepted = replies.iter().filter(|r| r.status == 200).count();
        let busy: Vec<&Reply> = replies.iter().filter(|r| r.status == 503).collect();
        let retry_after = busy.iter().all(|r| r.header("retry-after").is_some_and(|v| v.parse::<u32>().is_ok()));
        let only_expected = accepted + busy.len() == count;
        let statuses: Vec<u16> = replies.iter().map(|r| r.status).collect();
        report.check(
            &format!("round {round}: {count} concurrent requests hit the queue limit"),
            only_expected && accepted == 3 && busy.len() == count - 3,
            format!("{accepted} accepted, {} refused, statuses {statuses:?}", busy.len()),
        );
        report.check(
            &format!("round {round}: every 503 has Retry-After"),
            !busy.is_empty() && retry_after,
            busy.first().and_then(|r| r.header("retry-after")).unwrap_or("none"),
        );
        let identical = replies
            .iter()
            .filter(|r| r.status == 200)
            .map(|r| r.json()["text"].as_str().unwrap_or("").to_string())
            .collect::<std::collections::BTreeSet<_>>();
        report.check(
            &format!("round {round}: accepted requests all got the full text"),
            identical.len() == 1 && identical.iter().next().is_some_and(|t| t.len() > 100),
            format!("{} characters", identical.iter().next().map_or(0, String::len)),
        );

        let after = exchange(address, &post(&fs::read(&clips[0]).unwrap(), &headers));
        report.status(&format!("round {round}: service recovers after the burst"), &after, 200);
        report.check(&format!("round {round}: service process is still running"), service.alive(), "");
    }
}

fn log_privacy(report: &mut Report, service: &Service, token: &str, transcripts: &[String], clips: &[PathBuf]) {
    let log = service.log_text();
    report.check("log does not contain the token", !log.contains(token), format!("{} bytes of log", log.len()));

    let lower = log.to_lowercase();
    let leaked: Vec<&String> = transcripts.iter().filter(|text| lower.contains(&text.to_lowercase())).collect();
    report.check(
        "log does not contain any transcript",
        leaked.is_empty(),
        format!("{} transcripts checked", transcripts.len()),
    );

    let internal_failures = log.matches("status=500").count();
    report.check(
        "no request in the whole run ended in a 500",
        internal_failures == 0,
        format!("{internal_failures} responses with status 500"),
    );

    let names: Vec<String> = clips.iter().map(|c| c.file_name().unwrap().to_string_lossy().into_owned()).collect();
    report.check("log does not contain clip names", !names.iter().any(|n| log.contains(n)), "");

    let request_lines: Vec<&str> = log.lines().filter(|line| line.contains(" status=")).collect();
    let well_formed = request_lines.iter().all(|line| {
        let mut fields = line.split(' ');
        let timestamp = fields.next().unwrap_or("");
        timestamp.len() == 24
            && timestamp.ends_with('Z')
            && fields.all(|field| {
                field.split_once('=').is_some_and(|(key, value)| {
                    ["status", "duration_ms", "body_bytes", "audio_ms", "decode_ms"].contains(&key)
                        && value.bytes().all(|b| b.is_ascii_digit())
                })
            })
    });
    report.check(
        "request log lines hold only a timestamp, status, duration and sizes",
        !request_lines.is_empty() && well_formed,
        format!("{} request lines; first: {}", request_lines.len(), request_lines.first().unwrap_or(&"")),
    );

    let connection_lines: Vec<&str> = log.lines().filter(|line| line.contains(" connections ")).collect();
    let numbers_only = connection_lines.iter().all(|line| {
        line.split(' ').skip(2).all(|field| {
            field.split_once('=').is_some_and(|(key, value)| {
                ["refused_address", "refused_busy", "header_timeouts"].contains(&key)
                    && value.bytes().all(|b| b.is_ascii_digit())
            })
        })
    });
    report.check(
        "connection report lines hold only numbers",
        numbers_only,
        format!("{} lines", connection_lines.len()),
    );

    let others: Vec<&str> =
        log.lines().filter(|line| !line.contains(" status=") && !line.contains(" connections ")).collect();
    let others_fine = others.iter().all(|line| {
        ["starting swifty-speech", "model directory:", "token file:", "listening on", "model loaded in"]
            .iter()
            .any(|known| line.contains(known))
    });
    let shown: Vec<&&str> = others.iter().take(8).collect();
    report.check(
        "the only other log lines are startup messages",
        others_fine,
        format!("{} lines: {shown:?}", others.len()),
    );
}

fn refused_address_cannot_block_a_client(
    report: &mut Report,
    exe: &Path,
    work: &Path,
    port: u16,
    token_arg: &str,
    clip: &Path,
) {
    let allowed: IpAddr = "127.0.0.2".parse().unwrap();
    let mut service =
        Service::start(exe, work, "refused-flood", port, &["--token-file", token_arg, "--allow", "127.0.0.2"]);
    let address = service.address;
    let token = fs::read_to_string(token_arg).unwrap();

    let deadline = Instant::now() + Duration::from_secs(120);
    while exchange_from(allowed, address, &get("/healthz", &[])).status != 200 {
        assert!(service.alive(), "the service exited during startup");
        assert!(Instant::now() < deadline, "the model never loaded");
        thread::sleep(Duration::from_millis(20));
    }

    let before = exchange(address, &get("/healthz", &[]));
    report.status("403 from the refused address before it opens idle sockets", &before, 403);

    let idle: Vec<TcpStream> = (0..40).map(|_| TcpStream::connect(address).unwrap()).collect();
    thread::sleep(Duration::from_millis(300));

    let during = exchange(address, &get("/healthz", &[]));
    report.status("403, not 503, from the refused address while it holds 40 sockets", &during, 403);
    let health = exchange_from(allowed, address, &get("/healthz", &[]));
    report.status("the allowed client's /healthz is unaffected by the idle sockets", &health, 200);
    let transcribed = exchange_from(allowed, address, &post(&fs::read(clip).unwrap(), &authorized(&token)));
    report.status("the allowed client's request is served during the flood", &transcribed, 200);

    drop(idle);
    service.stop();
}

fn allow_lists(report: &mut Report, exe: &Path, work: &Path, port: u16, token_arg: &str) {
    let mut refusing = Service::start(
        exe,
        work,
        "refusing",
        port,
        &["--token-file", token_arg, "--allow", "192.0.2.77", "--allow", "192.0.2.78"],
    );
    let token = fs::read_to_string(token_arg).unwrap();
    let clip = pcm_wav(1, 16_000, &[0; 1600]);

    let deadline = Instant::now() + Duration::from_secs(20);
    let health = loop {
        match try_exchange(refusing.address, &get("/healthz", &[])) {
            Ok(reply) => break reply,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Err(error) => panic!("no reply: {error}"),
        }
    };
    report.status("403: /healthz from an address not on the allow list", &health, 403);
    report.status(
        "403: POST with a valid token from an address not on the allow list",
        &exchange(refusing.address, &post(&clip, &authorized(&token))),
        403,
    );
    report.status(
        "403: POST without a token from an address not on the allow list",
        &exchange(refusing.address, &post(&clip, &[wav_type()])),
        403,
    );

    let taken = refusing.address.to_string();
    let (code, _, err) = run_exe(exe, work, &["run", "--bind", &taken, "--token-file", token_arg]);
    report.check("a port already in use is a clear failure", code == Some(1) && err.contains("cannot listen"), &err);
    refusing.stop();

    let mut admitting = Service::start(
        exe,
        work,
        "admitting",
        port + 1,
        &["--token-file", token_arg, "--allow", "192.0.2.77", "--allow", "127.0.0.1", "--threads", "2"],
    );
    admitting.wait_ready();
    report.status(
        "200: POST from an address on the allow list",
        &exchange(admitting.address, &post(&clip, &authorized(&token))),
        200,
    );
    admitting.stop();
}

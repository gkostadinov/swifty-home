//! Not a check of its own: the whole service on loopback, with the real adapters and
//! every device a local mock, so the console page can be driven in a browser. It never
//! ends, so it is ignored unless asked for by name:
//!
//! ```text
//! cargo test --test browser_stack -- --ignored --nocapture
//! ```
//!
//! It lives under `tests/` so that it cannot be part of the service binary. Everything
//! it opens is on 127.0.0.1, and the page has to reach it through a TLS proxy of your
//! own at `ORIGIN`, because the session cookie is `__Host-` and passkeys need a secure
//! origin. The ports are fixed and nothing here reads the environment.

mod common;

use std::time::Duration;

use common::devices::*;
use common::*;
use serde_json::json;
use swifty_hub::config::SpeechConfig;
use swifty_hub::inventory::{self, PRIMARY_HOST};
use swifty_hub::passkeys::Passkeys;
use swifty_plugin_api::SeenMachine;

const ORIGIN_HERE: &str = "https://localhost:18843";
const LISTEN: &str = "127.0.0.1:18890";
const SPEECH_URL: &str = "http://127.0.0.1:18891";

fn snapshot() -> serde_json::Value {
    json!({
        "pkg_temp_c": 64.5,
        "pkg_watts": 118,
        "cpu_usage": 31,
        "memory_used_bytes": 126_000_000_000u64,
        "cores": (0..32).map(|core| 5 + core * 3).collect::<Vec<i32>>(),
        "faults": { "segfault_count_24h": 2, "oom_count_24h": 1 },
        "disks": [{ "name": "nvme0", "health": "PASSED", "media_errors": 0 }],
        "storage": {
            "read_errors": 0, "write_errors": 0, "checksum_errors": 1, "scrub_errors": 0,
            "pools": [{ "name": "fast", "read_errors": 0, "write_errors": 0, "checksum_errors": 1, "scrub_errors": 0 }],
        },
        "bluetooth": { "controllers": { "drops_24h": 0 }, "headsets": { "drops_24h": 1 } },
        "sources": { "faults": "ok", "disks": true, "storage": { "reporting": true }, "bluetooth": "ok" },
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs until killed, for driving the page in a browser"]
async fn serve_the_console_against_mock_devices() {
    let rig = Rig::start().await;
    rig.bulbs[3].set_mode(BulbMode::Silent);
    *rig.snapshot.lock().unwrap() = snapshot();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("speech.token"), "speech-test-token-0123456789\n").unwrap();
    let harness = Harness::with_site(|site| {
        site.live = true;
        site.timing.confirm_timeout_ms = Some(1_200);
        site.timing.poll_ms = 5;
        rig.configure_all(site);
        site.webauthn.rp_id = "localhost".to_string();
        site.webauthn.origin = ORIGIN_HERE.to_string();
        site.speech = Some(SpeechConfig {
            url: SPEECH_URL.to_string(),
            token_file: dir.path().join("speech.token"),
            connect_timeout_secs: 2,
            timeout_secs: 5,
        });
    });
    let now = harness.now();
    harness
        .app
        .store
        .with(|conn| {
            inventory::record_seen(
                conn,
                now,
                "proxmox",
                &[SeenMachine { host: PRIMARY_HOST.to_string(), vmid: 104, name: "new-vm".to_string() }],
            )
        })
        .unwrap();
    harness.app.sync_directory().unwrap();
    for _ in 0..3 {
        swifty_hub::history::sample_collector(&harness.app).await.unwrap();
        harness.advance(MINUTE_MS);
    }
    let (token, _) = Passkeys::issue_enroll_token(&harness.app.store, harness.now()).unwrap();
    println!("ENROLL-TOKEN {token}");

    let router = harness.router.clone();
    let listener = tokio::net::TcpListener::bind(LISTEN).await.unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    println!("LISTENING {LISTEN}");
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        harness.advance(250);
    }
}

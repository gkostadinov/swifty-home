//! Starts the real binary on loopback and talks to it over a real socket. The
//! child process is always stopped, even if an assertion fails.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_loopback_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[tokio::test]
async fn serve_runs_dry_on_loopback_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_loopback_port();
    let config = dir.path().join("config.toml");
    let database = dir.path().join("home.db");
    std::fs::write(
        &config,
        format!(
            "config_version = 2\nlisten = '127.0.0.1:{port}'\ndatabase = '{}'\n\
             [webauthn]\norigin = 'https://home.example.org'\nrp_id = 'home.example.org'\nrp_name = 'Swifty Home'\n",
            database.display()
        ),
    )
    .unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_swifty"))
        .arg("--config")
        .arg(&config)
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _server = Server(child);

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let started = Instant::now();
    loop {
        match client.get(format!("{base}/healthz")).send().await {
            Ok(response) => {
                assert_eq!(response.text().await.unwrap(), "ok");
                break;
            }
            Err(_) if started.elapsed() < Duration::from_secs(20) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("the server never answered: {error}"),
        }
    }

    let page = client.get(&base).send().await.unwrap();
    assert!(page.status().is_success());
    let csp = page.headers()["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'none'"));

    let no_origin = client.post(format!("{base}/api/v1/auth/login/start")).send().await.unwrap();
    assert_eq!(no_origin.status(), 403);

    let no_session = client.get(format!("{base}/api/v1/catalog")).send().await.unwrap();
    assert_eq!(no_session.status(), 401);

    // No passkey exists yet, and the real origin is what the guard expects.
    let login = client
        .post(format!("{base}/api/v1/auth/login/start"))
        .header("origin", "https://home.example.org")
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 409);
}

#[test]
fn serve_refuses_a_version_1_file_and_names_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let database = dir.path().join("home.db");
    let old = format!(
        "database = '{}'\nconsole_vm = 102\n[devices.sonos]\narc_url = 'http://192.0.2.30:1400'\n",
        database.display()
    );
    std::fs::write(&config, old).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_swifty")).arg("--config").arg(&config).arg("serve").output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("This is a glimdock-home configuration"), "{stderr}");
    assert!(stderr.contains(&format!("swifty migrate --from {}", config.display())), "{stderr}");
    assert!(!database.exists(), "nothing was started");
}

/// Waits for the hub to answer `/healthz`.
async fn wait_for(client: &reqwest::Client, base: &str) {
    let started = Instant::now();
    loop {
        match client.get(format!("{base}/healthz")).send().await {
            Ok(response) => {
                assert_eq!(response.text().await.unwrap(), "ok");
                return;
            }
            Err(_) if started.elapsed() < Duration::from_secs(20) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("the server never answered: {error}"),
        }
    }
}

#[tokio::test]
async fn a_fresh_install_serves_and_its_token_opens_enrollment() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_loopback_port();
    let config = dir.path().join("etc").join("swifty.toml");
    let origin = format!("http://localhost:{port}");
    let init = Command::new(env!("CARGO_BIN_EXE_swifty"))
        .arg("--config")
        .arg(&config)
        .args(["init", "--yes", "--origin", &origin, "--data-dir"])
        .arg(dir.path().join("data"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(init.status.success(), "{}", String::from_utf8_lossy(&init.stderr));
    // The listen port follows the port of a localhost origin.
    assert!(std::fs::read_to_string(&config).unwrap().contains(&format!("listen = \"127.0.0.1:{port}\"")));
    let shown = String::from_utf8(init.stdout).unwrap();
    let token = shown
        .lines()
        .map(str::trim)
        .find(|line| line.len() == 43 && line.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        .expect("init prints the enrollment token")
        .to_string();

    let child = Command::new(env!("CARGO_BIN_EXE_swifty"))
        .args(["serve", "--config"])
        .arg(&config)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _server = Server(child);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    wait_for(&client, &base).await;

    let start = |token: String| {
        client
            .post(format!("{base}/api/v1/auth/enroll/start"))
            .header("origin", &origin)
            .header("content-type", "application/json")
            .body(serde_json::json!({ "token": token, "name": "First phone" }).to_string())
            .send()
    };
    assert!(start("not-the-token".to_string()).await.unwrap().status().is_client_error());
    let opened = start(token).await.unwrap();
    assert!(opened.status().is_success(), "{}", opened.status());
}

//! `swifty.toml`, version 2: what it accepts, what it refuses, and that a version 1
//! file is recognised and sent to `swifty migrate` instead of being read.

use std::net::Ipv4Addr;
use std::path::Path;

use swifty_hub::config::shared::{ConfigError, DEFAULT_CLIENT_HEADER, DEFAULT_CLIENT_NAME, DEFAULT_SESSION_PREFIX};
use swifty_hub::config::v2::{Config, NetworkConfig};

const EXAMPLE: &str = include_str!("../../../packaging/swifty.example.toml");
const VERSION_1_EXAMPLE: &str = include_str!("fixtures/legacy-config.example.toml");

const WEBAUTHN: &str =
    "[webauthn]\nrp_id = \"home.example.org\"\norigin = \"https://home.example.org\"\nrp_name = \"Swifty Home\"\n";

/// A valid file with `top` among its top-level keys and `sections` after `[webauthn]`.
fn file(top: &str, sections: &str) -> String {
    format!("config_version = 2\ndata_dir = \"/var/lib/swifty-home\"\n{top}\n{WEBAUTHN}{sections}\n")
}

fn parse(top: &str, sections: &str) -> Result<Config, ConfigError> {
    Config::parse(&file(top, sections))
}

fn refusal(top: &str, sections: &str) -> String {
    match parse(top, sections) {
        Ok(_) => panic!("accepted:\n{}", file(top, sections)),
        Err(error) => error.to_string(),
    }
}

/// The example with every commented-out setting switched on.
fn example_with_everything() -> String {
    EXAMPLE
        .lines()
        .map(|line| {
            let uncommented = line.strip_prefix("# ").unwrap_or(line);
            let setting = uncommented.starts_with('[')
                || uncommented
                    .split_once(" = ")
                    .is_some_and(|(key, _)| key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
            if setting && line.starts_with("# ") {
                uncommented
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_example_is_a_dry_run_version_2_file() {
    let config = Config::parse(EXAMPLE).unwrap();
    assert!(!config.live, "the example must not enable live mode");
    assert_eq!(config.config_version, 2);
    assert_eq!(config.listen, "127.0.0.1:8790");
    assert_eq!(config.database_path(), Path::new("/var/lib/swifty-home").join("home.db"));
    assert_eq!(config.secrets_path(), Path::new("/var/lib/swifty-home").join("secrets"));
    assert_eq!(config.webauthn.origin, "https://home.example.org");
    assert_eq!(config.audit.key_file.as_deref(), Some(Path::new("/etc/swifty-home/audit.key")));
    assert_eq!(config.network, NetworkConfig::default());
    assert!(config.runs_on.is_none() && config.speech.is_none() && config.assistant.is_none());
}

#[test]
fn every_optional_setting_of_the_example_is_valid_once_switched_on() {
    let text = example_with_everything();
    let config = Config::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(config.runs_on.as_deref(), Some("vm102"));
    let speech = config.speech.as_ref().unwrap();
    assert_eq!(speech.url, "http://127.0.0.1:8791", "the speech service's own default port");
    let assistant = config.assistant.as_ref().unwrap();
    let agent = assistant.agent.as_ref().unwrap();
    assert_eq!(agent.session_prefix, DEFAULT_SESSION_PREFIX);
    assert!(assistant.weather.is_some());
    assert!(config.health.cpu_canary_file.is_some());
}

#[test]
fn a_written_file_reads_back_the_same() {
    for text in [EXAMPLE.to_string(), example_with_everything(), file("live = true\nruns_on = \"host\"", "")] {
        let config = Config::parse(&text).unwrap();
        let written = config.to_toml().unwrap();
        assert!(written.starts_with("# Swifty Home configuration, version 2.\n"), "{written}");
        assert_eq!(Config::parse(&written).unwrap(), config, "{written}");
    }
}

#[test]
fn a_version_1_file_is_sent_to_swifty_migrate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, VERSION_1_EXAMPLE).unwrap();
    match Config::load(&path) {
        Err(ConfigError::Legacy { from }) => assert_eq!(from, path.display().to_string()),
        other => panic!("not refused as version 1: {other:?}"),
    }
    let message = Config::load(&path).unwrap_err().to_string();
    assert_eq!(
        message,
        format!("This is a glimdock-home configuration. Run: swifty migrate --from {}", path.display())
    );

    // Each key only version 1 has is enough to tell, and nothing is guessed from the rest.
    for text in [
        "database = \"home.db\"\nconsole_vm = 102\n",
        "database = \"home.db\"\n[devices]\nconnect_timeout_ms = 1500\n",
        "database = \"home.db\"\n[devices.hue]\nurl = \"http://192.0.2.30\"\nkey_file = \"/k\"\n",
        "database = \"home.db\"\n[console]\ntargets = []\n",
        "database = \"home.db\"\n[machines]\nallowed_host_networks = []\n",
        "database = \"home.db\"\n[lights]\nwiz_broadcast = \"192.0.2.255:38899\"\n",
        "database = \"home.db\"\n[agents]\npairing_path = \"/pair\"\n",
    ] {
        match Config::parse(text) {
            Err(ConfigError::Legacy { from }) => assert_eq!(from, "<that file>"),
            other => panic!("{text}: {other:?}"),
        }
    }
    // A version 2 file that names a version 1 section is not version 1: it is refused for the key.
    let message = refusal("", "[devices]\nconnect_timeout_ms = 1500");
    assert!(message.contains("unknown field `devices`"), "{message}");
}

#[test]
fn a_missing_webauthn_section_is_refused() {
    let message = Config::parse("config_version = 2\ndata_dir = \"/var/lib/swifty-home\"\n").unwrap_err().to_string();
    assert!(message.contains("webauthn"), "{message}");
    for missing in ["rp_id", "origin", "rp_name"] {
        let partial: String = WEBAUTHN.lines().filter(|line| !line.starts_with(missing)).collect::<Vec<_>>().join("\n");
        let text = format!("config_version = 2\ndata_dir = \"/d\"\n{partial}\n");
        let message = Config::parse(&text).unwrap_err().to_string();
        assert!(message.contains(missing), "{missing}: {message}");
    }
}

#[test]
fn the_origin_rules_hold() {
    let with = |origin: &str, rp_id: &str, rp_name: &str| {
        let text = format!(
            "config_version = 2\ndata_dir = \"/d\"\n[webauthn]\nrp_id = \"{rp_id}\"\norigin = \"{origin}\"\nrp_name = \"{rp_name}\"\n"
        );
        Config::parse(&text)
    };
    assert!(with("https://home.example.org", "home.example.org", "Swifty Home").is_ok());
    assert!(with("https://home.example.org", "example.org", "Swifty Home").is_ok(), "a parent domain");
    assert!(with("http://localhost:8790", "localhost", "Swifty Home").is_ok());
    for (origin, rp_id, rp_name) in [
        ("http://home.example.org", "home.example.org", "Swifty Home"),
        ("https://home.example.org/", "home.example.org", "Swifty Home"),
        ("https://home.example.org/app", "home.example.org", "Swifty Home"),
        ("https://evil.example", "home.example.org", "Swifty Home"),
        ("https://home.example.org", "other.example.org", "Swifty Home"),
        ("home.example.org", "home.example.org", "Swifty Home"),
        ("https://home.example.org", "home.example.org", ""),
        ("https://home.example.org", "home.example.org", "   "),
    ] {
        assert!(with(origin, rp_id, rp_name).is_err(), "{origin} {rp_id} {rp_name:?}");
    }
}

#[test]
fn the_version_must_be_2() {
    let without = format!("data_dir = \"/d\"\n{WEBAUTHN}");
    let message = Config::parse(&without).unwrap_err().to_string();
    assert!(message.contains("config_version"), "{message}");
    for version in ["1", "3", "\"2\"", "2.0", "-2"] {
        let text = format!("config_version = {version}\ndata_dir = \"/d\"\n{WEBAUTHN}");
        let message = Config::parse(&text).unwrap_err().to_string();
        assert!(message.contains("config_version"), "{version}: {message}");
    }
}

#[test]
fn a_data_directory_or_a_database_is_required() {
    let text = format!("config_version = 2\n{WEBAUTHN}");
    assert!(Config::parse(&text).unwrap_err().to_string().contains("data_dir"));
    let only_database = format!("config_version = 2\ndatabase = \"/srv/home/hub.db\"\n{WEBAUTHN}");
    let config = Config::parse(&only_database).unwrap();
    assert_eq!(config.database_path(), Path::new("/srv/home/hub.db"));
    assert_eq!(config.secrets_path(), Path::new("/srv/home").join("secrets"));
    let elsewhere = parse("database = \"/srv/db/home.db\"\nsecrets_dir = \"/srv/keys\"", "").unwrap();
    assert_eq!(elsewhere.database_path(), Path::new("/srv/db/home.db"));
    assert_eq!(elsewhere.secrets_path(), Path::new("/srv/keys"));
    for empty in ["database = \"\"", "secrets_dir = \"\""] {
        assert!(parse(empty, "").is_err(), "{empty}");
    }
}

#[test]
fn runs_on_is_a_device_target() {
    for good in ["vm102", "host", "two-vm200", "nas_1"] {
        assert_eq!(parse(&format!("runs_on = \"{good}\""), "").unwrap().runs_on.as_deref(), Some(good));
    }
    for bad in ["", "VM102", "vm 102", "vm102/../x", "-vm", "102\n"] {
        let message = refusal(&format!("runs_on = {bad:?}"), "");
        assert!(message.contains("runs_on"), "{bad:?}: {message}");
    }
    assert!(parse("", "").unwrap().runs_on.is_none(), "there is no default");
}

#[test]
fn listen_must_be_an_address_and_a_port() {
    assert_eq!(parse("", "").unwrap().listen, "127.0.0.1:8790");
    assert!(parse("listen = \"[::1]:8790\"", "").is_ok());
    for bad in ["localhost:8790", "127.0.0.1", "", "127.0.0.1:99999"] {
        assert!(refusal(&format!("listen = \"{bad}\""), "").contains("listen"), "{bad}");
    }
}

#[test]
fn the_network_section_is_checked() {
    let network = parse("", "").unwrap().network;
    assert_eq!((network.connect_timeout_ms, network.read_timeout_ms), (1500, 2500));
    assert_eq!(network.allowed_host_networks, ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"]);
    let with = |body: &str| parse("", &format!("[network]\n{body}"));
    assert_eq!(
        with("allowed_host_networks = [\"192.0.2.0/24\"]").unwrap().network.allowed_host_networks,
        ["192.0.2.0/24"]
    );
    assert!(with("connect_timeout_ms = 30000\nread_timeout_ms = 1").is_ok());
    for bad in [
        "connect_timeout_ms = 0",
        "read_timeout_ms = 30001",
        "allowed_host_networks = [\"0.0.0.0/0\"]",
        "allowed_host_networks = [\"::/0\"]",
        "allowed_host_networks = [\"10.0.0.0\"]",
        "allowed_host_networks = [\"nope/8\"]",
        "timeout_ms = 5",
    ] {
        assert!(with(bad).is_err(), "{bad}");
    }
}

#[test]
fn session_and_timing_keep_their_limits() {
    assert!(parse("", "[session]\nabsolute_secs = 28801\nidle_secs = 60").is_err());
    assert!(parse("", "[session]\nabsolute_secs = 600\nidle_secs = 601").is_err());
    let timing = "[timing]\npoll_ms = 250\nsync_wait_ms = 4000\nduplicate_window_ms = 10000\nconfirm_timeout_ms = 1";
    assert!(parse("", timing).is_ok(), "allowed while nothing is live");
    assert!(refusal("live = true", timing).contains("confirm_timeout_ms"));
    assert!(parse("", "[timing]\npoll_ms = 10\nsync_wait_ms = 4000\nduplicate_window_ms = 10000").is_err());
}

#[test]
fn live_must_be_a_real_boolean() {
    assert!(parse("live = true", "").unwrap().live);
    assert!(!parse("", "").unwrap().live);
    for bad in ["live = \"true\"", "live = 1", "live = \"yes\""] {
        assert!(parse(bad, "").is_err(), "{bad}");
    }
}

#[test]
fn a_pasted_secret_is_refused_and_never_repeated() {
    const PASTED: &str = "PASTED-PRIVATE-VALUE-0123456789";
    for (top, sections) in [
        (format!("token = \"{PASTED}\""), String::new()),
        (String::new(), format!("[audit]\nkey_file = \"/k\"\nkey = \"{PASTED}\"")),
        (
            String::new(),
            format!("[speech]\nurl = \"http://127.0.0.1:8791\"\ntoken_file = \"/t\"\ntoken = \"{PASTED}\""),
        ),
        (format!("live = \"{PASTED}\""), String::new()),
        (format!("runs_on = [\"{PASTED}\"]"), String::new()),
        (String::new(), format!("[network]\nconnect_timeout_ms = \"{PASTED}\"")),
        (PASTED.to_string(), String::new()),
    ] {
        let message = refusal(&top, &sections);
        assert!(!message.contains("PASTED"), "{message}");
        assert!(message.starts_with("invalid configuration:"), "{message}");
    }
    let message = refusal(&format!("token = \"{PASTED}\""), "");
    assert!(message.contains("line 3") && message.contains("unknown field `token`"), "{message}");
}

#[test]
fn the_agent_identity_has_new_defaults_and_can_be_pinned() {
    let assistant = "[assistant]\nurls = [\"http://127.0.0.1:8080/v1\"]\nmodel = \"my-model\"\nkey_file = \"/etc/swifty-home/assistant.key\"\n\
                     [assistant.agent]\nowner_url = \"http://127.0.0.1:8801\"\nguest_url = \"http://127.0.0.1:8802\"\nconnection = \"c\"\nmodel = \"m\"\n";
    let agent = parse("", assistant).unwrap().assistant.unwrap().agent.unwrap();
    assert_eq!(
        (agent.session_prefix.as_str(), agent.client_header.as_str(), agent.client_name.as_str()),
        (DEFAULT_SESSION_PREFIX, DEFAULT_CLIENT_HEADER, DEFAULT_CLIENT_NAME)
    );
    assert_eq!(
        (DEFAULT_SESSION_PREFIX, DEFAULT_CLIENT_HEADER, DEFAULT_CLIENT_NAME),
        ("swifty", "x-swifty-client", "Swifty Home")
    );

    let pinned = format!(
        "{assistant}session_prefix = \"oldhub\"\nclient_header = \"x-oldhub-client\"\nclient_name = \"Old Hub\"\n"
    );
    let agent = parse("", &pinned).unwrap().assistant.unwrap().agent.unwrap();
    assert_eq!(
        (agent.session_prefix.as_str(), agent.client_header.as_str(), agent.client_name.as_str()),
        ("oldhub", "x-oldhub-client", "Old Hub")
    );
    for bad in [
        "session_prefix = \"Swifty\"",
        "session_prefix = \"\"",
        "client_header = \"X Client\"",
        "client_header = \"x-client:\"",
        "client_name = \"\"",
        "client_name = \"two\\nlines\"",
    ] {
        assert!(parse("", &format!("{assistant}{bad}\n")).is_err(), "{bad}");
    }
}

#[test]
fn optional_sections_keep_their_rules() {
    let speech = |url: &str| parse("", &format!("[speech]\nurl = \"{url}\"\ntoken_file = \"/t\""));
    assert!(speech("http://127.0.0.1:8791").is_ok());
    assert!(speech("https://speech.example.org:8791").is_ok());
    for bad in ["http://192.0.2.9:8791", "http://user:x@127.0.0.1:8791", "http://127.0.0.1:8791/path"] {
        assert!(speech(bad).is_err(), "{bad}");
    }
    let assistant =
        |urls: &str| parse("", &format!("[assistant]\nurls = {urls}\nmodel = \"my-model\"\nkey_file = \"/k\""));
    assert!(assistant("[\"http://192.0.2.20:8080/v1\"]").is_err(), "plain http only on a private network");
    // Plain http is for a private network, which a documentation address is not: built, not written.
    let private = Ipv4Addr::new(192, 168, 1, 20);
    assert!(assistant(&format!("[\"http://{private}:8080/v1\"]")).is_ok());
    assert!(assistant("[]").is_err());
    assert!(parse("", "[health]\ncpu_canary_file = \"/var/lib/swifty-home/cpu-canary.json\"").is_ok());
    assert!(parse("", "[health]\nsomething = 1").is_err());
}

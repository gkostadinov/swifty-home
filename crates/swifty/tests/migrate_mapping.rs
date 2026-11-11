//! The legacy mapping: a version 1 configuration, with every section filled in,
//! maps to plugin instances that each plugin's typed config accepts, to secret
//! slots holding what the old files held, and to a version 2 `swifty.toml` that
//! behaves the same.
//!
//! The hub does not link the plugins, so each plugin's typed config is described
//! by a JSON fixture in `tests/fixtures/plugin_configs/`, written from
//! `plugins/<id>/src/config.rs` and the plugin's manifest. One test here holds
//! every fixture to the plugin's source, so a plugin that changes its config
//! shape fails this file until the fixture (and the mapping) follow.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use swifty_hub::config::{legacy, v2};
use swifty_hub::migrate::{self, FileKind, Mapped, Places, INSTANCES};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(plugin: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/plugin_configs/{plugin}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

// ----- The fixture checker: what each plugin's typed config accepts, as data.

fn problems(fixture: &Value, config: &Value) -> Vec<String> {
    let mut found = Vec::new();
    let root = fixture["root"].as_str().unwrap();
    check_struct(fixture, root, config, "", &mut found);
    for pair in fixture["unique"].as_array().into_iter().flatten() {
        let (rows, key) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
        let mut seen = BTreeSet::new();
        for row in config[rows].as_array().into_iter().flatten() {
            if !seen.insert(row[key].to_string()) {
                found.push(format!("{rows}: {key} {} is listed twice", row[key]));
            }
        }
    }
    if fixture["pin_only_for_wss"].as_bool() == Some(true) {
        let wss = config["url"].as_str().is_some_and(|url| url.starts_with("wss:"));
        let pinned = config.get("cert_sha256").is_some_and(|pin| !pin.is_null());
        if wss != pinned {
            found.push("cert_sha256: needed for wss and only for wss".to_string());
        }
    }
    found
}

fn check_struct(fixture: &Value, name: &str, value: &Value, at: &str, found: &mut Vec<String>) {
    let fields = fixture["structs"][name].as_object().unwrap_or_else(|| panic!("the fixture has no struct {name}"));
    let Some(object) = value.as_object() else {
        found.push(format!("{at}: not an object"));
        return;
    };
    for key in object.keys().filter(|key| !fields.contains_key(*key)) {
        found.push(format!("{at}{key}: unknown field"));
    }
    for (key, field) in fields {
        let optional = field["optional"].as_bool().unwrap_or(false);
        let nullable = field["nullable"].as_bool().unwrap_or(false);
        match object.get(key) {
            None if optional => {}
            None => found.push(format!("{at}{key}: missing")),
            Some(Value::Null) if nullable => {}
            Some(value) => check_value(fixture, field, value, &format!("{at}{key}"), found),
        }
    }
}

fn check_value(fixture: &Value, field: &Value, value: &Value, at: &str, found: &mut Vec<String>) {
    let kind = field["type"].as_str().unwrap();
    let mut wrong = |why: &str| found.push(format!("{at}: {why}"));
    match kind {
        "string" => match value.as_str() {
            Some(text) => check_string(field, text, at, found),
            None => wrong("not a string"),
        },
        "bool" if !value.is_boolean() => wrong("not a boolean"),
        "bool" => {}
        "u16" | "u32" | "u64" | "i32" => {
            let (low, high): (i128, i128) = match kind {
                "u16" => (0, u16::MAX.into()),
                "u32" => (0, u32::MAX.into()),
                "u64" => (0, u64::MAX.into()),
                _ => (i32::MIN.into(), i32::MAX.into()),
            };
            let number = value.as_u64().map(i128::from).or_else(|| value.as_i64().map(i128::from));
            let low = field["min"].as_i64().map_or(low, i128::from);
            let high = field["max"].as_i64().map_or(high, i128::from);
            match number {
                Some(number) if (low..=high).contains(&number) => {}
                Some(_) => wrong(&format!("out of {low}..={high}")),
                None => wrong("not an integer"),
            }
        }
        "rows" => {
            let Some(rows) = value.as_array() else { return wrong("not a list of rows") };
            let min = field["min_rows"].as_u64().unwrap_or(0) as usize;
            let max = field["max_rows"].as_u64().map_or(usize::MAX, |max| max as usize);
            if !(min..=max).contains(&rows.len()) {
                wrong(&format!("{} rows, not {min}..={max}", rows.len()));
            }
            let of = field["of"].as_str().unwrap();
            for (index, row) in rows.iter().enumerate() {
                check_struct(fixture, of, row, &format!("{at}[{index}]."), found);
            }
        }
        other => panic!("the fixture names an unknown type {other}"),
    }
}

fn check_string(field: &Value, text: &str, at: &str, found: &mut Vec<String>) {
    if let Some(max) = field["max_len"].as_u64() {
        if text.chars().count() as u64 > max {
            found.push(format!("{at}: longer than {max}"));
        }
    }
    let Some(format) = field["format"].as_str() else { return };
    let lower_digits = |text: &str| text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let ok = match format {
        "url" => url_problem(field, text).map(|why| found.push(format!("{at}: {why}"))).is_none(),
        "local_id" => {
            text.len() <= 64
                && text.as_bytes().first().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        }
        "plug_id" => (10..=32).contains(&text.len()) && lower_digits(text),
        "aircon_id" => (8..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "unique_id" => {
            !text.is_empty()
                && text.len() <= 128
                && text.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\')
        }
        "ipv4" => {
            text.parse::<Ipv4Addr>().is_ok_and(|ip| !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast())
        }
        "ip_or_socket" => text.parse::<IpAddr>().is_ok() || text.parse::<SocketAddr>().is_ok(),
        "ipv4_socket" => text.parse::<SocketAddr>().is_ok_and(|address| address.is_ipv4()),
        "socket" => text.parse::<SocketAddr>().is_ok(),
        "mac" => {
            let parts: Vec<&str> = text.split([':', '-']).collect();
            parts.len() == 6 && parts.iter().all(|part| part.len() == 2 && u8::from_str_radix(part, 16).is_ok())
        }
        "sha256_hex" => text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()),
        "hex" => !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit()),
        "plain_path" => {
            text.starts_with('/')
                && !text.starts_with("//")
                && !text.contains(['?', '#', '\\'])
                && !text.contains("..")
                && !text.chars().any(char::is_control)
        }
        "absolute_path" => Path::new(text).is_absolute(),
        "host" => !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')),
        "node" => !text.is_empty() && text.len() <= 63 && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "vm" => {
            let (host, rest) = match text.split_once('-') {
                Some((host, rest)) => (Some(host), rest),
                None => (None, text),
            };
            let host_ok = host.is_none_or(|host| {
                host != "primary"
                    && host.len() <= 16
                    && host.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                    && lower_digits(host)
            });
            let digits = rest.strip_prefix("vm").unwrap_or("");
            host_ok
                && !digits.is_empty()
                && digits.len() <= 9
                && !digits.starts_with('0')
                && digits.bytes().all(|b| b.is_ascii_digit())
        }
        "region" => !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-')),
        "timezone" => {
            !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
        }
        "plain_name" => !text.trim().is_empty() && text.trim() == text && !text.chars().any(char::is_control),
        other => panic!("the fixture names an unknown format {other}"),
    };
    if !ok && format != "url" {
        found.push(format!("{at}: not a {format}"));
    }
}

/// A device's base URL: the scheme and authority only, as every plugin checks it.
fn url_problem(field: &Value, text: &str) -> Option<String> {
    let Ok(url) = url::Url::parse(text) else { return Some("not a URL".to_string()) };
    let schemes: Vec<&str> = field["schemes"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
    if !schemes.contains(&url.scheme()) {
        return Some(format!("the scheme must be one of {schemes:?}"));
    }
    if url.host_str().is_none()
        || url.authority().contains('@')
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Some("must be only a scheme, host and port".to_string());
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(name)) => name == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if field["https_unless_loopback"].as_bool() == Some(true) && url.scheme() != "https" && !loopback {
        return Some("must use https unless it is this machine".to_string());
    }
    None
}

/// Whether a mapped slot is one the plugin declares: `local_key/{id}` stands for
/// `local_key/<a row's id>`.
fn declared_slot<'a>(fixture: &'a Value, slot: &str) -> Option<&'a Value> {
    fixture["secrets"].as_array().unwrap().iter().find(|declared| {
        let name = declared["slot"].as_str().unwrap();
        match name.strip_suffix("/{id}") {
            Some(base) => {
                slot.strip_prefix(base).and_then(|rest| rest.strip_prefix('/')).is_some_and(|id| !id.is_empty())
            }
            None => name == slot,
        }
    })
}

// ----- Holding the fixtures to the plugins' sources.

struct RustField {
    name: String,
    ty: String,
    defaulted: bool,
}

/// The fields of `pub struct <name>` in a plugin's source, with whether serde
/// defaults each one, and whether the struct refuses unknown fields.
fn rust_struct(source: &str, name: &str) -> (bool, Vec<RustField>) {
    let header = format!("pub struct {name} {{");
    let start = source.find(&header).unwrap_or_else(|| panic!("no `{header}` in the source"));
    let refuses_unknown = source[..start]
        .trim_end()
        .lines()
        .rev()
        .take_while(|line| line.trim().starts_with("#[") || line.trim().starts_with("///"))
        .any(|line| line.contains("deny_unknown_fields"));
    let body = &source[start + header.len()..];
    let body = &body[..body.find("\n}").expect("the struct ends")];
    let mut fields = Vec::new();
    let mut defaulted = false;
    for line in body.lines().map(str::trim) {
        if line.starts_with("#[serde(") {
            defaulted |= line.contains("default");
        } else if let Some(rest) = line.strip_prefix("pub ") {
            let (field, ty) = rest.split_once(':').expect("a field has a type");
            fields.push(RustField {
                name: field.trim().to_string(),
                ty: ty.trim().trim_end_matches(',').trim().to_string(),
                defaulted,
            });
            defaulted = false;
        }
    }
    (refuses_unknown, fields)
}

/// A Rust field type as the fixture writes it: (type, row struct, nullable).
fn fixture_type(ty: &str) -> (String, Option<String>, bool) {
    if let Some(inner) = ty.strip_prefix("Option<").and_then(|inner| inner.strip_suffix('>')) {
        let (kind, of, _) = fixture_type(inner);
        return (kind, of, true);
    }
    if let Some(inner) = ty.strip_prefix("Vec<").and_then(|inner| inner.strip_suffix('>')) {
        return ("rows".to_string(), Some(inner.to_string()), false);
    }
    let kind = match ty {
        "String" => "string",
        "bool" => "bool",
        other => other,
    };
    (kind.to_string(), None, false)
}

/// Every `slot:` a plugin's manifest declares, with whether it is required.
fn manifest_slots(plugin: &str) -> BTreeMap<String, bool> {
    let src = workspace().join("plugins").join(plugin).join("src");
    let mut all = String::new();
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            all.push_str(&std::fs::read_to_string(path).unwrap());
        }
    }
    let lib = std::fs::read_to_string(src.join("lib.rs")).unwrap();
    let lines: Vec<&str> = lib.lines().collect();
    let mut slots = BTreeMap::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(expression) = line.trim().strip_prefix("slot: ").and_then(|e| e.strip_suffix(',')) else { continue };
        let name = match expression.strip_prefix('"').and_then(|e| e.strip_suffix('"')) {
            Some(literal) => literal.to_string(),
            None => {
                let declaration = format!("const {expression}: &str = \"");
                let at = all.find(&declaration).unwrap_or_else(|| panic!("{plugin}: no {declaration}"));
                let rest = &all[at + declaration.len()..];
                rest[..rest.find('"').unwrap()].to_string()
            }
        };
        let required = lines[index..]
            .iter()
            .find_map(|line| line.trim().strip_prefix("required: "))
            .unwrap_or_else(|| panic!("{plugin}: {name} says whether it is required"));
        slots.insert(name, required.trim_end_matches(',') == "true");
    }
    slots
}

#[test]
fn every_fixture_matches_its_plugins_source() {
    for plugin in INSTANCES {
        let fixture = fixture(plugin);
        assert_eq!(fixture["plugin"], json!(plugin));
        let source_path = workspace().join(fixture["source"].as_str().unwrap());
        let source = std::fs::read_to_string(&source_path).unwrap_or_else(|e| panic!("{}: {e}", source_path.display()));

        let mut reached = BTreeSet::new();
        let mut pending = vec![fixture["root"].as_str().unwrap().to_string()];
        while let Some(name) = pending.pop() {
            if !reached.insert(name.clone()) {
                continue;
            }
            let (refuses_unknown, fields) = rust_struct(&source, &name);
            assert!(refuses_unknown, "{plugin}: {name} must refuse unknown fields, as the fixture assumes");
            let described = fixture["structs"][&name].as_object().unwrap_or_else(|| panic!("{plugin}: no {name}"));
            let in_source: BTreeSet<&str> = fields.iter().map(|field| field.name.as_str()).collect();
            let in_fixture: BTreeSet<&str> = described.keys().map(String::as_str).collect();
            assert_eq!(in_fixture, in_source, "{plugin}: the fields of {name}");
            for field in &fields {
                let entry = described.get(&field.name).unwrap();
                let (kind, of, nullable) = fixture_type(&field.ty);
                let at = format!("{plugin}: {name}.{}", field.name);
                assert_eq!(entry["type"].as_str(), Some(kind.as_str()), "{at}");
                assert_eq!(entry["of"].as_str(), of.as_deref(), "{at}");
                assert_eq!(entry["nullable"].as_bool().unwrap_or(false), nullable, "{at}: nullable");
                assert_eq!(entry["optional"].as_bool().unwrap_or(false), nullable || field.defaulted, "{at}: optional");
                if let Some(of) = of {
                    pending.push(of);
                }
            }
        }
        let described: BTreeSet<String> = fixture["structs"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(described, reached, "{plugin}: the fixture describes exactly the structs of the config");

        let lib = std::fs::read_to_string(workspace().join(format!("plugins/{plugin}/src/lib.rs"))).unwrap();
        let version = fixture["config_version"].as_u64().unwrap();
        assert!(lib.contains(&format!("config_version: {version},")), "{plugin}: config_version {version}");
        let slots: BTreeMap<String, bool> = fixture["secrets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|slot| (slot["slot"].as_str().unwrap().to_string(), slot["required"].as_bool().unwrap()))
            .collect();
        assert_eq!(slots, manifest_slots(plugin), "{plugin}: the secret slots");
    }
}

#[test]
fn every_fixture_accepts_its_example_and_refuses_what_its_plugin_refuses() {
    for plugin in INSTANCES {
        let fixture = fixture(plugin);
        let example = &fixture["example"];
        assert_eq!(problems(&fixture, example), Vec::<String>::new(), "{plugin}: its own example");

        let mut unknown = example.clone();
        unknown["key_file"] = json!("/k");
        assert!(!problems(&fixture, &unknown).is_empty(), "{plugin}: a file path is not a field any more");

        let root = fixture["root"].as_str().unwrap();
        for (key, field) in fixture["structs"][root].as_object().unwrap() {
            if field["optional"].as_bool() == Some(true) {
                continue;
            }
            let mut without = example.clone();
            without.as_object_mut().unwrap().remove(key);
            assert!(!problems(&fixture, &without).is_empty(), "{plugin}: {key} is required");
            let mut mistyped = example.clone();
            mistyped[key] = json!({ "wrong": "shape" });
            assert!(!problems(&fixture, &mistyped).is_empty(), "{plugin}: {key} has a type");
        }
    }
}

// ----- The mapping.

/// An assistant on plain http has to be on a private network, which a documentation
/// address is not: so this one is built here and not written out.
fn private_host() -> Ipv4Addr {
    Ipv4Addr::new(192, 168, 1, 21)
}

/// A version 1 file with every section the old service read, its credential
/// files beside it in `<dir>/old`, and its database in `<dir>/old-data`.
fn full_legacy(dir: &Path) -> String {
    let at = dir.display().to_string().replace('\\', "/");
    let pin = "ab".repeat(32);
    LEGACY_FULL.replace("@@DIR@@", &at).replace("@@PIN@@", &pin).replace("@@LAN@@", &private_host().to_string())
}

const LEGACY_FULL: &str = r#"
live = false
listen = "127.0.0.1:8792"
database = "@@DIR@@/old-data/home.db"
console_vm = 102

[webauthn]
rp_id = "home.example.org"
origin = "https://home.example.org"
rp_name = "Glimdock Home"

[session]
absolute_secs = 28800
idle_secs = 1800

[timing]
poll_ms = 250
sync_wait_ms = 4000
duplicate_window_ms = 10000

[audit]
key_file = "@@DIR@@/old/audit.key"

[speech]
url = "http://127.0.0.1:8790"
token_file = "@@DIR@@/old/speech.token"
connect_timeout_secs = 3
timeout_secs = 20

[assistant]
urls = ["http://127.0.0.1:8080/v1", "http://@@LAN@@:8080/v1"]
model = "my-model"
key_file = "@@DIR@@/old/assistant.key"

[assistant.weather]
place = "Example City"
latitude = 51.48
longitude = 0.0

[assistant.agent]
owner_url = "http://127.0.0.1:8801"
guest_url = "http://127.0.0.1:8802"
connection = "my-connection"
model = "my-model"

[devices]
connect_timeout_ms = 1600
read_timeout_ms = 2600

[devices.wiz]
port = 38899

[devices.wiz.bulbs]
bulb1 = "192.0.2.21"
bulb2 = "192.0.2.22"
bulb3 = "192.0.2.23:38900"
bulb4 = "192.0.2.24"

[devices.hue]
url = "http://hue.example.org"
key_file = "@@DIR@@/old-data/hue.key"

[devices.sonos]
arc_url = "http://192.0.2.31:1400"
sub_url = "http://192.0.2.32:1400"

[devices.webos]
url = "wss://tv.example.org:3001"
cert_sha256 = "@@PIN@@"
client_key_file = "@@DIR@@/old/lgtv.key"
wol_mac = "AA:BB:CC:DD:EE:FF"
wol_broadcast = "192.0.2.255:9"

[devices.moonraker]
url = "http://printer.example.org:7125"
api_key_file = "@@DIR@@/old/moonraker.key"

[devices.plex]
url = "http://plex.example.org:32400"
token_file = "@@DIR@@/old/plex.token"

[devices.camera]
host = "192.0.2.36"
port = 8554
credentials_file = "@@DIR@@/old/camera.login"

[devices.proxmox]
url = "https://pve.example.org:8006"
node = "pve"
token_file = "@@DIR@@/old/proxmox.token"
ca_file = "@@DIR@@/old/pve-root-ca.pem"

[devices.collector]
url = "http://192.0.2.18:8765"
snapshot_path = "/api/v1/snapshot"
token_file = "@@DIR@@/old/collector.token"

[[devices.tuya.plugs]]
id = "bf0123456789abcdef"
address = "192.0.2.40"
key_file = "@@DIR@@/old/tuya-bf0123456789abcdef.key"
usb_dp = 7

[[devices.tuya.plugs]]
id = "bf0123456789abcde2"
address = "192.0.2.41"
port = 6669
name = "Desk lamp"
key_file = "@@DIR@@/old/tuya-bf0123456789abcde2.key"

[devices.petkit]
region = "DE"
credentials_file = "@@DIR@@/old/petkit.login"
timezone = "Europe/Berlin"
utc_offset_minutes = 60
commands = false
feeder_id = 123456
litter_id = 234567
poll_secs = 90

[devices.toshiba]
credentials_file = "@@DIR@@/old/toshiba.login"

[[devices.toshiba.units]]
id = "0a1b2c3d-1111-2222-3333-444455556666"
unique_id = "AB-12:cd"
name = "Lounge"

[[devices.toshiba.units]]
id = "0a1b2c3d-1111-2222-3333-444455557777"
unique_id = "AB-12:ce"

[console]
targets = ["vm101", "vm103"]

[machines]
allowed_host_networks = ["198.51.100.0/24"]
secrets_dir = "@@DIR@@/old"

[lights]
wiz_broadcast = "192.0.2.255:38899"

[agents]
pairing_path = "/api/v1/agents/pair"

[health]
cpu_canary_file = "@@DIR@@/canary.json"
"#;

/// Every credential file of [`LEGACY_FULL`], by instance and slot, and what it
/// holds. The values are padded the way a file written by hand often is.
const CREDENTIALS: &[(&str, &str, &str, &str)] = &[
    ("old-data/hue.key", "hue", "app_key", "hue-app-key-fixture"),
    ("old/lgtv.key", "webos", "client_key", "webos-client-key-fixture"),
    ("old/moonraker.key", "moonraker", "api_key", "moonraker-key-fixture"),
    ("old/plex.token", "plex", "token", "plex-fixture-value"),
    ("old/camera.login", "camera", "login", "viewer:camera-fixture"),
    ("old/proxmox.token", "proxmox", "token", "swifty@pve!home=fixture-value"),
    ("old/pve-root-ca.pem", "proxmox", "ca", "-----BEGIN CERTIFICATE-----\nMIIfixture\n-----END CERTIFICATE-----"),
    ("old/collector.token", "glimdock", "token", "collector-fixture-value"),
    ("old/tuya-bf0123456789abcdef.key", "tuya", "local_key/bf0123456789abcdef", "MDEyMzQ1Njc4OWFiY2RlZg=="),
    ("old/tuya-bf0123456789abcde2.key", "tuya", "local_key/bf0123456789abcde2", "ZmVkY2JhOTg3NjU0MzIxMA=="),
    ("old/petkit.login", "petkit", "login", "someone@example.org:0123456789abcdef0123456789abcdef"),
    ("old/toshiba.login", "toshiba", "login", "eyJ1c2VybmFtZSI6InNvbWVvbmUifQ=="),
];

fn write_credentials(dir: &Path) {
    for sub in ["old", "old-data"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    for (file, _, _, value) in CREDENTIALS {
        std::fs::write(dir.join(file), format!("  {value}\n\n")).unwrap();
    }
}

fn places(dir: &Path) -> Places {
    Places { config_dir: dir.join("etc-new"), data_dir: dir.join("data-new") }
}

fn read(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

fn map(text: &str, places: &Places) -> Mapped {
    let old = legacy::Config::parse(text).unwrap_or_else(|e| panic!("the version 1 file parses: {e}"));
    migrate::map_legacy_to(&old, places, read).unwrap()
}

#[test]
fn a_legacy_config_with_every_section_maps_to_what_each_plugin_accepts() {
    let dir = tempfile::tempdir().unwrap();
    write_credentials(dir.path());
    let places = places(dir.path());
    let mapped = map(&full_legacy(dir.path()), &places);

    assert_eq!(mapped.dropped, Vec::new(), "nothing of a complete file is lost");
    let ids: Vec<&str> = mapped.instances.iter().map(|instance| instance.id.as_str()).collect();
    assert_eq!(ids, INSTANCES, "one instance per section");
    for instance in &mapped.instances {
        let fixture = fixture(&instance.plugin);
        assert_eq!(instance.plugin, instance.id);
        assert_eq!(u64::from(instance.config_version), fixture["config_version"].as_u64().unwrap());
        assert_eq!(problems(&fixture, &instance.config), Vec::<String>::new(), "{}: {}", instance.id, instance.config);
    }

    // Every slot is one its plugin declares, and holds what the old file held.
    let mut filled = BTreeSet::new();
    for value in &mapped.secrets {
        let fixture = fixture(&value.instance);
        assert!(declared_slot(&fixture, &value.slot).is_some(), "{} declares no slot {}", value.instance, value.slot);
        assert!(filled.insert((value.instance.clone(), value.slot.clone())), "{} {} twice", value.instance, value.slot);
    }
    for (_, instance, slot, expected) in CREDENTIALS {
        let found = mapped.slot(instance, slot).unwrap_or_else(|| panic!("{instance} {slot}"));
        assert_eq!(found.expose(), *expected, "{instance} {slot}: trimmed, as the old service read it");
    }
    assert_eq!(mapped.secrets.len(), CREDENTIALS.len());
    for instance in &mapped.instances {
        for required in fixture(&instance.plugin)["secrets"].as_array().unwrap() {
            if required["required"].as_bool() != Some(true) {
                continue;
            }
            let slot = required["slot"].as_str().unwrap();
            let present =
                match slot.strip_suffix("/{id}") {
                    Some(base) => instance.config["plugs"].as_array().unwrap().iter().all(|row| {
                        mapped.slot(&instance.id, &format!("{base}/{}", row["id"].as_str().unwrap())).is_some()
                    }),
                    None => mapped.slot(&instance.id, slot).is_some(),
                };
            assert!(present, "{}: the required slot {slot} is filled", instance.id);
        }
    }

    // The values, section by section.
    let config = |id: &str| mapped.instance(id).unwrap().config.clone();
    assert_eq!(
        config("wiz"),
        json!({
            "bulbs": [
                { "id": "bulb1", "address": "192.0.2.21" },
                { "id": "bulb2", "address": "192.0.2.22" },
                { "id": "bulb3", "address": "192.0.2.23:38900" },
                { "id": "bulb4", "address": "192.0.2.24" },
            ],
            "port": 38899,
            "broadcast": "192.0.2.255:38899",
        })
    );
    assert_eq!(config("hue"), json!({ "url": "http://hue.example.org" }));
    assert_eq!(
        config("sonos"),
        json!({ "speakers": [
            { "id": "sonos", "url": "http://192.0.2.31:1400" },
            { "id": "sub", "url": "http://192.0.2.32:1400", "read_only": true },
        ] })
    );
    assert_eq!(
        config("webos"),
        json!({
            "url": "wss://tv.example.org:3001",
            "cert_sha256": "ab".repeat(32),
            "wol_mac": "AA:BB:CC:DD:EE:FF",
            "wol_broadcast": "192.0.2.255:9",
        })
    );
    assert_eq!(config("moonraker"), json!({ "url": "http://printer.example.org:7125" }));
    assert_eq!(config("plex"), json!({ "url": "http://plex.example.org:32400" }));
    assert_eq!(config("camera"), json!({ "host": "192.0.2.36", "port": 8554 }));
    assert_eq!(
        config("proxmox"),
        json!({
            "url": "https://pve.example.org:8006",
            "node": "pve",
            "public_ca": false,
            "console_targets": [{ "vm": "vm101" }, { "vm": "vm103" }],
            "host_files_dir": places.config_dir.join("hosts").to_string_lossy(),
        })
    );
    assert_eq!(
        config("glimdock"),
        json!({ "url": "http://192.0.2.18:8765", "snapshot_path": "/api/v1/snapshot", "pairing_path": "/api/v1/agents/pair" })
    );
    assert_eq!(
        config("tuya"),
        json!({ "plugs": [
            { "id": "bf0123456789abcdef", "address": "192.0.2.40", "port": 6668, "usb_dp": 7 },
            { "id": "bf0123456789abcde2", "address": "192.0.2.41", "port": 6669, "name": "Desk lamp" },
        ] })
    );
    assert_eq!(
        config("petkit"),
        json!({
            "region": "DE",
            "timezone": "Europe/Berlin",
            "utc_offset_minutes": 60,
            "commands": false,
            "feeder_id": 123456,
            "litter_id": 234567,
            "passport_url": "https://passport.petkt.com/",
            "poll_secs": 90,
        })
    );
    assert_eq!(
        config("toshiba"),
        json!({
            "api_url": "https://mobileapi.toshibahomeaccontrols.com",
            "hub_url": "https://toshibasmaciothubprod.azure-devices.net",
            "timeout_ms": 10000,
            "units": [
                { "id": "0a1b2c3d-1111-2222-3333-444455556666", "unique_id": "AB-12:cd", "name": "Lounge" },
                { "id": "0a1b2c3d-1111-2222-3333-444455557777", "unique_id": "AB-12:ce" },
            ],
        })
    );

    // swifty.toml: the same behaviour, with the hub's own files in their new places.
    let new = &mapped.config;
    assert_eq!(new.config_version, 2);
    assert!(!new.live);
    assert_eq!(new.listen, "127.0.0.1:8792");
    assert_eq!(new.runs_on.as_deref(), Some("vm102"));
    assert_eq!(new.data_dir.as_deref(), Some(places.data_dir.as_path()));
    assert_eq!(new.database_path(), places.data_dir.join("home.db"));
    assert_eq!(new.secrets_path(), places.data_dir.join("secrets"));
    assert_eq!(
        (new.webauthn.rp_id.as_str(), new.webauthn.origin.as_str(), new.webauthn.rp_name.as_str()),
        ("home.example.org", "https://home.example.org", "Glimdock Home")
    );
    assert_eq!((new.session.absolute_secs, new.session.idle_secs), (28800, 1800));
    assert_eq!((new.timing.poll_ms, new.timing.sync_wait_ms, new.timing.duplicate_window_ms), (250, 4000, 10000));
    assert_eq!((new.network.connect_timeout_ms, new.network.read_timeout_ms), (1600, 2600));
    assert_eq!(new.network.allowed_host_networks, ["198.51.100.0/24"]);
    assert_eq!(new.audit.key_file, Some(places.config_dir.join("audit.key")));
    let speech = new.speech.as_ref().unwrap();
    assert_eq!(speech.url, "http://127.0.0.1:8790");
    assert_eq!(speech.token_file, places.config_dir.join("speech.token"));
    let assistant = new.assistant.as_ref().unwrap();
    assert_eq!(assistant.urls, ["http://127.0.0.1:8080/v1".to_string(), format!("http://{}:8080/v1", private_host())]);
    assert_eq!(assistant.key_file, places.config_dir.join("assistant.key"));
    assert_eq!(assistant.weather.as_ref().unwrap().place, "Example City");
    let agent = assistant.agent.as_ref().unwrap();
    assert_eq!(
        (agent.session_prefix.as_str(), agent.client_header.as_str(), agent.client_name.as_str()),
        ("swifty", "x-swifty-client", "Swifty Home"),
        "the mapping gives the agent version 2's identity: the old one was compiled in, and the site file pins it"
    );
    assert_eq!(
        migrate::required_pins(&legacy::Config::parse(&full_legacy(dir.path())).unwrap()),
        [migrate::Pin::ToshibaClientSuffix, migrate::Pin::AgentIdentity],
        "swifty migrate refuses this install unless the site file pins both"
    );
    assert_eq!((agent.owner_url.as_str(), agent.max_turn_secs), ("http://127.0.0.1:8801", 3600));
    let canary = dir.path().join("canary.json").display().to_string().replace('\\', "/");
    assert_eq!(new.health.cpu_canary_file, Some(PathBuf::from(canary)), "the canary is not the hub's file");
    new.validate().unwrap();
    let written = new.to_toml().unwrap();
    assert_eq!(&v2::Config::parse(&written).unwrap(), new, "the written file reads back the same:\n{written}");

    // What `swifty migrate` copies, and where to.
    let old = |file: &str| PathBuf::from(format!("{}/{file}", dir.path().display().to_string().replace('\\', "/")));
    let files: Vec<(FileKind, PathBuf, PathBuf)> =
        mapped.files.iter().map(|file| (file.kind, file.from.clone(), file.to.clone())).collect();
    assert_eq!(
        files,
        [
            (FileKind::Database, old("old-data/home.db"), places.data_dir.join("home.db")),
            (FileKind::AuditKey, old("old/audit.key"), places.config_dir.join("audit.key")),
            (FileKind::SpeechToken, old("old/speech.token"), places.config_dir.join("speech.token")),
            (FileKind::AssistantKey, old("old/assistant.key"), places.config_dir.join("assistant.key")),
            (FileKind::FurtherHostFiles, old("old"), places.config_dir.join("hosts")),
        ]
    );

    // No credential is in the instance configs, the new file or anything printed.
    let printed = format!("{mapped:?}");
    for (_, _, _, value) in CREDENTIALS {
        for line in value.lines() {
            assert!(!written.contains(line), "{line} in swifty.toml");
            assert!(!printed.contains(line), "{line} in the Debug output");
            for instance in &mapped.instances {
                assert!(!instance.config.to_string().contains(line), "{line} in {}", instance.id);
            }
        }
    }
}

/// The smallest version 1 file `swifty migrate` takes: a database and the origin
/// passkeys are bound to, with `top` among the top-level keys and `sections` after.
fn smallest(top: &str, sections: &str) -> String {
    format!(
        "database = \"home.db\"\n{top}\n[webauthn]\nrp_id = \"home.example.org\"\norigin = \"https://home.example.org\"\nrp_name = \"Glimdock Home\"\n{sections}\n"
    )
}

#[test]
fn the_standard_places_are_the_new_install_directories() {
    let text = smallest("", "").replace("home.db", "/var/lib/glimdock-home/home.db");
    let mapped = migrate::map_legacy(&legacy::Config::parse(&text).unwrap(), read).unwrap();
    assert_eq!(mapped.config.data_dir.as_deref(), Some(Path::new("/var/lib/swifty-home")));
    assert_eq!(mapped.config.database_path(), Path::new("/var/lib/swifty-home").join("home.db"));
    assert_eq!(
        mapped.files.iter().map(|file| (file.kind, file.to.clone())).collect::<Vec<_>>(),
        [(FileKind::Database, Path::new("/var/lib/swifty-home").join("home.db"))]
    );
    assert_eq!(Places::default().config_dir, Path::new("/etc/swifty-home"));
}

#[test]
fn the_smallest_legacy_file_keeps_what_its_defaults_meant() {
    let dir = tempfile::tempdir().unwrap();
    let mapped = map(&smallest("", ""), &places(dir.path()));
    assert!(mapped.instances.is_empty());
    assert!(mapped.secrets.is_empty());
    assert!(mapped.dropped.is_empty(), "the default console list has nothing to belong to, and that is no loss");
    let new = &mapped.config;
    assert_eq!(new.listen, "127.0.0.1:8790");
    assert_eq!(new.runs_on.as_deref(), Some("vm102"), "console_vm defaulted to 102");
    assert_eq!(new.webauthn.rp_name, "Glimdock Home");
    assert_eq!((new.session.absolute_secs, new.session.idle_secs), (28800, 1800));
    assert_eq!(new.network, v2::NetworkConfig::default());
    assert!(new.speech.is_none() && new.assistant.is_none() && new.audit.key_file.is_none());
    v2::Config::parse(&new.to_toml().unwrap()).unwrap();
}

#[test]
fn a_file_that_names_no_origin_is_not_guessed() {
    // Passkeys are bound to the origin, and the frozen parser's default is not the
    // one the old binary had, so a file that relied on it is refused.
    let old = legacy::Config::parse("database = \"home.db\"").unwrap();
    let error = migrate::map_legacy(&old, read).unwrap_err().to_string();
    assert!(error.contains("[webauthn]"), "{error}");
}

#[test]
fn a_proxmox_host_without_a_ca_file_trusts_the_public_roots() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("proxmox.token");
    std::fs::write(&token, "swifty@pve!home=fixture-value\n").unwrap();
    let proxmox = format!(
        "[devices.proxmox]\nurl = \"https://pve.example.org:8006\"\nnode = \"pve\"\ntoken_file = \"{}\"\n",
        token.display().to_string().replace('\\', "/")
    );
    let mapped = map(&smallest("", &proxmox), &places(dir.path()));
    let proxmox = mapped.instance("proxmox").unwrap();
    assert_eq!(
        proxmox.config,
        json!({
            "url": "https://pve.example.org:8006",
            "node": "pve",
            "public_ca": true,
            "console_targets": [{ "vm": "vm101" }, { "vm": "vm102" }, { "vm": "vm103" }],
        }),
        "version 1's default console list is written out, since the plugin's default is none"
    );
    assert_eq!(problems(&fixture("proxmox"), &proxmox.config), Vec::<String>::new());
    assert!(mapped.slot("proxmox", "token").is_some());
    assert!(mapped.slot("proxmox", "ca").is_none());
    assert!(!mapped.files.iter().any(|file| file.kind == FileKind::FurtherHostFiles), "no further hosts were possible");
}

#[test]
fn discovery_alone_makes_a_wiz_instance() {
    let dir = tempfile::tempdir().unwrap();
    let mapped = map(&smallest("", "[lights]\nwiz_broadcast = \"192.0.2.255:38899\""), &places(dir.path()));
    let wiz = mapped.instance("wiz").unwrap();
    assert_eq!(wiz.config, json!({ "broadcast": "192.0.2.255:38899" }));
    assert_eq!(problems(&fixture("wiz"), &wiz.config), Vec::<String>::new());
    assert_eq!(mapped.instances.len(), 1);
}

#[test]
fn what_has_no_place_or_cannot_be_read_is_reported_and_nothing_else_is_lost() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("plex.token");
    std::fs::write(&empty, " \n").unwrap();
    let at = dir.path().display().to_string().replace('\\', "/");
    let sections = format!(
        "[devices.hue]\nurl = \"http://192.0.2.30\"\nkey_file = \"{at}/never-paired.key\"\n\
         [devices.plex]\nurl = \"http://192.0.2.35:32400\"\ntoken_file = \"{at}/plex.token\"\n\
         [console]\ntargets = [\"vm101\"]\n\
         [machines]\nsecrets_dir = \"{at}\"\n\
         [agents]\npairing_path = \"/pair\"\n"
    );
    let mapped = map(&smallest("", &sections), &places(dir.path()));
    let what: Vec<&str> = mapped.dropped.iter().map(|dropped| dropped.what.split(' ').next().unwrap()).collect();
    assert_eq!(
        what,
        [
            "devices.hue.key_file",
            "devices.plex.token_file",
            "console.targets",
            "machines.secrets_dir",
            "agents.pairing_path"
        ]
    );
    for dropped in &mapped.dropped {
        assert!(!dropped.why.is_empty(), "{dropped:?}");
    }
    assert!(
        mapped.dropped[0].why.contains("app_key") && mapped.dropped[0].why.contains("hue"),
        "{:?}",
        mapped.dropped[0]
    );
    assert!(mapped.dropped[1].why.contains("empty"), "{:?}", mapped.dropped[1]);
    // The instances are still there, waiting for their credentials.
    assert_eq!(mapped.instance("hue").unwrap().config, json!({ "url": "http://192.0.2.30" }));
    assert!(mapped.instance("plex").is_some());
    assert!(mapped.secrets.is_empty());
    assert!(mapped.instance("proxmox").is_none() && mapped.instance("glimdock").is_none());
    assert!(!mapped.files.iter().any(|file| file.kind == FileKind::FurtherHostFiles));
}

#[test]
fn a_mapping_that_would_not_be_a_valid_version_2_file_is_refused() {
    // Version 1 took any text for `listen`; version 2 wants an address and a port.
    let old = legacy::Config::parse(&smallest("listen = \"localhost:8790\"", "")).unwrap();
    let error = migrate::map_legacy(&old, read).unwrap_err();
    assert!(error.to_string().contains("listen"), "{error}");
}

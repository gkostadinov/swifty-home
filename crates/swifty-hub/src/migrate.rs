//! The move from a glimdock-home install to Swifty Home.
//!
//! [`map_legacy`] turns a version 1 configuration into what version 2 keeps in
//! three places: `swifty.toml` (a [`v2::Config`]), plugin instances (the rows of
//! `plugin_instances`, each config the JSON its plugin's typed config accepts) and
//! secret slots (the contents of the old credential files). It reads files only
//! through the function it is given, writes nothing, and lists what it could not
//! carry over instead of failing on it.
//!
//! [`run`] is `swifty migrate`: the refusals, the
//! audit chain, the copies of the database and of the hub's own key files (listed in
//! [`Mapped::files`]), the site file laid over the converted instances, validating
//! each instance with its plugin, writing the rows and the slots, and the report.
//!
//! | Version 1 | Version 2 |
//! | --- | --- |
//! | `[devices] connect_timeout_ms`, `read_timeout_ms` | `[network]` |
//! | `[devices.wiz] bulbs`, `port`; `[lights] wiz_broadcast` | instance `wiz`: `{ bulbs: [{id, address}], port, broadcast }` |
//! | `[devices.hue] url`, `key_file` | instance `hue`: `{ url }`, slot `app_key` |
//! | `[devices.sonos] arc_url`, `sub_url` | instance `sonos`: `{ speakers: [{id: "sonos", url}, {id: "sub", url, read_only: true}] }` |
//! | `[devices.webos] url`, `cert_sha256`, `wol_mac`, `wol_broadcast`, `client_key_file` | instance `webos`: the same fields, slot `client_key` |
//! | `[devices.moonraker] url`, `api_key_file` | instance `moonraker`: `{ url }`, optional slot `api_key` |
//! | `[devices.plex] url`, `token_file` | instance `plex`: `{ url }`, slot `token` |
//! | `[devices.camera] host`, `port`, `credentials_file` | instance `camera`: `{ host, port }`, slot `login` |
//! | `[devices.proxmox] url`, `node`, `token_file`, `ca_file`; `[console] targets`; `[machines] secrets_dir` | instance `proxmox`: `{ url, node, public_ca, console_targets, host_files_dir }`, slots `token`, `ca` |
//! | `[machines] allowed_host_networks` | `[network] allowed_host_networks` |
//! | `[devices.collector] url`, `snapshot_path`, `token_file`; `[agents] pairing_path` | instance `glimdock`: `{ url, snapshot_path, pairing_path }`, slot `token` |
//! | `[[devices.tuya.plugs]] id`, `address`, `port`, `name`, `usb_dp`, `key_file` | instance `tuya`: `{ plugs: [{id, address, port, name, usb_dp}] }`, slots `local_key/<id>` |
//! | `[devices.petkit] …`, `credentials_file` | instance `petkit`: the same fields, slot `login` |
//! | `[devices.toshiba] …`, `units`, `credentials_file` | instance `toshiba`: the same fields, slot `login` |
//! | `console_vm = N` | `runs_on = "vmN"` |
//! | `[assistant.agent]` | the same, with version 2's identity keys |
//! | `database`, `[audit] key_file`, `[speech] token_file`, `[assistant] key_file` | copied into the new directories ([`Mapped::files`]) |
//! | `listen`, `[webauthn]`, `[session]`, `[timing]`, `[health]` | the same |
//!
//! A slot holds the old file's content trimmed of surrounding white space, which is
//! what the old service read from it.
//!
//! Two things the old service compiled in rather than read from its file, and that a
//! vendor or an agent already knows the hub by, are not in this code: the client id
//! suffix it registered with Toshiba's cloud, and the identity it gave the agent
//! (session prefix, header name and value). The owner's site file pins them
//! (`[[instance]] toshiba` with `client_suffix`, and `[hub.assistant.agent]`), and
//! [`run`] refuses an install that needs them when the site file does not say
//! ([`required_pins`]).

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use swifty_plugin_api::Secret;

use crate::config::legacy::{self, default_console_targets, default_webauthn};
use crate::config::shared::{AuditConfig, ConfigError, SpeechConfig};
use crate::config::v2::{self, NetworkConfig};
use crate::secrets::MAX_VALUE_BYTES;

mod command;
mod snapshot;

pub use command::{run, Copied, DeviceLine, InstanceLine, MigrateError, Report, Request, ACTION};

/// Something the old service compiled in, that a vendor or an agent knows the hub by
/// and that the site file must therefore pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// The suffix of the client id it registered with Toshiba's cloud.
    ToshibaClientSuffix,
    /// The session prefix, header name and header value it gave the agent.
    AgentIdentity,
}

/// What this old install needs pinned: the Toshiba suffix when it used Toshiba's
/// cloud, the agent's identity when it had an agent.
pub fn required_pins(legacy: &legacy::Config) -> Vec<Pin> {
    let mut pins = Vec::new();
    if legacy.devices.toshiba.is_some() {
        pins.push(Pin::ToshibaClientSuffix);
    }
    if legacy.assistant.as_ref().is_some_and(|assistant| assistant.agent.is_some()) {
        pins.push(Pin::AgentIdentity);
    }
    pins
}

/// Every instance a version 1 file can give, in the order they are mapped.
pub const INSTANCES: [&str; 12] =
    ["wiz", "hue", "sonos", "webos", "moonraker", "plex", "camera", "proxmox", "glimdock", "tuya", "petkit", "toshiba"];

/// The config version every first-party plugin stores today.
const PLUGIN_CONFIG_VERSION: u32 = 1;

/// Where the new install keeps what the old one kept elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    /// `swifty.toml`, the hub's key files, and the further Proxmox hosts' files
    /// (in `hosts/` under it).
    pub config_dir: PathBuf,
    /// The database and the secret store.
    pub data_dir: PathBuf,
}

impl Default for Places {
    fn default() -> Self {
        Self { config_dir: PathBuf::from("/etc/swifty-home"), data_dir: PathBuf::from(v2::DEFAULT_DATA_DIR) }
    }
}

impl Places {
    fn audit_key(&self) -> PathBuf {
        self.config_dir.join("audit.key")
    }

    fn speech_token(&self) -> PathBuf {
        self.config_dir.join("speech.token")
    }

    fn assistant_key(&self) -> PathBuf {
        self.config_dir.join("assistant.key")
    }

    fn host_files(&self) -> PathBuf {
        self.config_dir.join("hosts")
    }

    fn database(&self) -> PathBuf {
        self.data_dir.join(v2::DATABASE_FILE)
    }
}

/// What a version 1 install becomes.
#[derive(Debug)]
pub struct Mapped {
    /// `swifty.toml`. It already passed [`v2::Config::validate`].
    pub config: v2::Config,
    /// One per configured device section, in the order of [`INSTANCES`].
    pub instances: Vec<Instance>,
    /// What goes into the secret store.
    pub secrets: Vec<SlotValue>,
    /// The hub's own files to copy into the new directories. The config already
    /// names their new places.
    pub files: Vec<HubFile>,
    /// What has no place in version 2, or could not be read. Nothing here is fatal.
    pub dropped: Vec<Dropped>,
}

impl Mapped {
    pub fn instance(&self, id: &str) -> Option<&Instance> {
        self.instances.iter().find(|instance| instance.id == id)
    }

    /// The value for one slot, if the old file could be read.
    pub fn slot(&self, instance: &str, slot: &str) -> Option<&Secret> {
        self.secrets.iter().find(|value| value.instance == instance && value.slot == slot).map(|value| &value.value)
    }
}

/// A row of `plugin_instances`.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub id: String,
    pub plugin: String,
    pub config_version: u32,
    /// The plugin's typed config, as JSON. Never a secret.
    pub config: Value,
}

/// One secret slot's value. `Debug` never shows it.
#[derive(Debug, Clone)]
pub struct SlotValue {
    pub instance: String,
    pub slot: String,
    pub value: Secret,
}

/// Which of the hub's own files a [`HubFile`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Copied with SQLite's online backup, not as a file.
    Database,
    AuditKey,
    SpeechToken,
    AssistantKey,
    /// A directory. Only the files the database's further hosts name are copied.
    FurtherHostFiles,
}

/// A file (or directory) of the old install and where the new install has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubFile {
    pub kind: FileKind,
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Something of the old install that the new one does not have, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    /// The version 1 key, and the file for a credential.
    pub what: String,
    pub why: String,
}

/// [`map_legacy_to`] with the standard places, `/etc/swifty-home` and `/var/lib/swifty-home`.
pub fn map_legacy(
    legacy: &legacy::Config,
    read_file: impl FnMut(&Path) -> io::Result<String>,
) -> Result<Mapped, ConfigError> {
    map_legacy_to(legacy, &Places::default(), read_file)
}

/// Maps a version 1 configuration. `read_file` returns a credential file's
/// content; a file it cannot read leaves its slot empty and is listed in
/// [`Mapped::dropped`]. Fails only when the origin passkeys are bound to is not
/// in the file, or when the version 2 config it builds does not validate.
pub fn map_legacy_to(
    legacy: &legacy::Config,
    places: &Places,
    read_file: impl FnMut(&Path) -> io::Result<String>,
) -> Result<Mapped, ConfigError> {
    if legacy.webauthn == default_webauthn() {
        // Passkeys are bound to the origin for good, and this default is not the one
        // the old binary had. Writing it would lock everyone out.
        return Err(ConfigError::Invalid(
            "webauthn: the version 1 file has no [webauthn] section, so the origin its passkeys are bound to \
             is not known. Add the section the old service ran with, then migrate again"
                .to_string(),
        ));
    }
    let mut mapper = Mapper { read_file, instances: Vec::new(), secrets: Vec::new(), dropped: Vec::new() };
    let devices = &legacy.devices;
    mapper.wiz(legacy);
    if let Some(hue) = &devices.hue {
        mapper.instance("hue", json!({ "url": hue.url }));
        mapper.credential("hue", "app_key", "devices.hue.key_file", &hue.key_file);
    }
    if let Some(sonos) = &devices.sonos {
        let mut speakers = vec![json!({ "id": "sonos", "url": sonos.arc_url })];
        if let Some(sub) = &sonos.sub_url {
            speakers.push(json!({ "id": "sub", "url": sub, "read_only": true }));
        }
        mapper.instance("sonos", json!({ "speakers": speakers }));
    }
    if let Some(webos) = &devices.webos {
        let mut config = Map::new();
        config.insert("url".into(), json!(webos.url));
        put(&mut config, "cert_sha256", &webos.cert_sha256);
        put(&mut config, "wol_mac", &webos.wol_mac);
        put(&mut config, "wol_broadcast", &webos.wol_broadcast);
        mapper.instance("webos", Value::Object(config));
        mapper.credential("webos", "client_key", "devices.webos.client_key_file", &webos.client_key_file);
    }
    if let Some(moonraker) = &devices.moonraker {
        mapper.instance("moonraker", json!({ "url": moonraker.url }));
        if let Some(path) = &moonraker.api_key_file {
            mapper.credential("moonraker", "api_key", "devices.moonraker.api_key_file", path);
        }
    }
    if let Some(plex) = &devices.plex {
        mapper.instance("plex", json!({ "url": plex.url }));
        mapper.credential("plex", "token", "devices.plex.token_file", &plex.token_file);
    }
    if let Some(camera) = &devices.camera {
        mapper.instance("camera", json!({ "host": camera.host, "port": camera.port }));
        mapper.credential("camera", "login", "devices.camera.credentials_file", &camera.credentials_file);
    }
    mapper.proxmox(legacy, places);
    mapper.glimdock(legacy);
    if let Some(tuya) = &devices.tuya {
        let plugs: Vec<Value> = tuya
            .plugs
            .iter()
            .map(|plug| {
                let mut row = Map::new();
                row.insert("id".into(), json!(plug.id));
                row.insert("address".into(), json!(plug.address));
                row.insert("port".into(), json!(plug.port));
                put(&mut row, "name", &plug.name);
                put(&mut row, "usb_dp", &plug.usb_dp);
                Value::Object(row)
            })
            .collect();
        mapper.instance("tuya", json!({ "plugs": plugs }));
        for plug in &tuya.plugs {
            let what = format!("devices.tuya.plugs[{}].key_file", plug.id);
            mapper.credential("tuya", &format!("local_key/{}", plug.id), &what, &plug.key_file);
        }
    }
    if let Some(petkit) = &devices.petkit {
        let mut config = Map::new();
        config.insert("region".into(), json!(petkit.region));
        config.insert("timezone".into(), json!(petkit.timezone));
        config.insert("utc_offset_minutes".into(), json!(petkit.utc_offset_minutes));
        config.insert("commands".into(), json!(petkit.commands));
        put(&mut config, "feeder_id", &petkit.feeder_id);
        put(&mut config, "litter_id", &petkit.litter_id);
        config.insert("passport_url".into(), json!(petkit.passport_url));
        config.insert("poll_secs".into(), json!(petkit.poll_secs));
        mapper.instance("petkit", Value::Object(config));
        mapper.credential("petkit", "login", "devices.petkit.credentials_file", &petkit.credentials_file);
    }
    if let Some(toshiba) = &devices.toshiba {
        let units: Vec<Value> = toshiba
            .units
            .iter()
            .map(|unit| {
                let mut row = Map::new();
                row.insert("id".into(), json!(unit.id));
                row.insert("unique_id".into(), json!(unit.unique_id));
                put(&mut row, "name", &unit.name);
                Value::Object(row)
            })
            .collect();
        let config = json!({
            "api_url": toshiba.api_url,
            "hub_url": toshiba.hub_url,
            "timeout_ms": toshiba.timeout_ms,
            "units": units,
        });
        mapper.instance("toshiba", config);
        mapper.credential("toshiba", "login", "devices.toshiba.credentials_file", &toshiba.credentials_file);
    }

    let config = hub_config(legacy, places);
    config.validate()?;
    Ok(Mapped {
        config,
        instances: mapper.instances,
        secrets: mapper.secrets,
        files: hub_files(legacy, places),
        dropped: mapper.dropped,
    })
}

/// Adds `key` to a JSON object when the old file set it.
fn put<T: serde::Serialize>(object: &mut Map<String, Value>, key: &str, value: &Option<T>) {
    if let Some(value) = value {
        object.insert(key.to_string(), json!(value));
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

struct Mapper<F> {
    read_file: F,
    instances: Vec<Instance>,
    secrets: Vec<SlotValue>,
    dropped: Vec<Dropped>,
}

impl<F: FnMut(&Path) -> io::Result<String>> Mapper<F> {
    /// An instance whose id is its plugin's id, as every first-party default is.
    fn instance(&mut self, id: &str, config: Value) {
        self.instances.push(Instance {
            id: id.to_string(),
            plugin: id.to_string(),
            config_version: PLUGIN_CONFIG_VERSION,
            config,
        });
    }

    fn skip(&mut self, what: impl Into<String>, why: impl Into<String>) {
        self.dropped.push(Dropped { what: what.into(), why: why.into() });
    }

    /// The content of an old credential file, for one slot.
    fn credential(&mut self, instance: &str, slot: &str, key: &str, path: &Path) {
        let what = format!("{key} ({})", path.display());
        let again = format!("set the {slot} of the {instance} instance again");
        match (self.read_file)(path) {
            Ok(text) => {
                let value = text.trim();
                if value.is_empty() {
                    self.skip(what, format!("the file is empty; {again}"));
                } else if value.len() > MAX_VALUE_BYTES {
                    self.skip(what, format!("the file is too long for a secret slot; {again}"));
                } else {
                    let value: String = value.to_string();
                    self.secrets.push(SlotValue {
                        instance: instance.to_string(),
                        slot: slot.to_string(),
                        value: value.into(),
                    });
                }
            }
            Err(error) => self.skip(what, format!("cannot be read ({}); {again}", error.kind())),
        }
    }

    fn wiz(&mut self, legacy: &legacy::Config) {
        let broadcast = &legacy.lights.wiz_broadcast;
        let Some(wiz) = &legacy.devices.wiz else {
            if broadcast.is_some() {
                let mut config = Map::new();
                put(&mut config, "broadcast", broadcast);
                self.instance("wiz", Value::Object(config));
            }
            return;
        };
        let bulbs: Vec<Value> = wiz.bulbs.iter().map(|(id, address)| json!({ "id": id, "address": address })).collect();
        let mut config = Map::new();
        config.insert("bulbs".into(), Value::Array(bulbs));
        config.insert("port".into(), json!(wiz.port));
        put(&mut config, "broadcast", broadcast);
        self.instance("wiz", Value::Object(config));
    }

    fn proxmox(&mut self, legacy: &legacy::Config, places: &Places) {
        let Some(proxmox) = &legacy.devices.proxmox else {
            if legacy.console.targets != default_console_targets() {
                self.skip("console.targets", "there is no [devices.proxmox] section for the console list to belong to");
            }
            if let Some(dir) = &legacy.machines.secrets_dir {
                self.skip(
                    format!("machines.secrets_dir ({})", dir.display()),
                    "there is no [devices.proxmox] section for further hosts to belong to",
                );
            }
            return;
        };
        let consoles: Vec<Value> = legacy.console.targets.iter().map(|vm| json!({ "vm": vm })).collect();
        let mut config = Map::new();
        config.insert("url".into(), json!(proxmox.url));
        config.insert("node".into(), json!(proxmox.node));
        // Without a CA file the old service trusted the public roots.
        config.insert("public_ca".into(), json!(proxmox.ca_file.is_none()));
        config.insert("console_targets".into(), Value::Array(consoles));
        if legacy.machines.secrets_dir.is_some() {
            config.insert("host_files_dir".into(), json!(path_text(&places.host_files())));
        }
        self.instance("proxmox", Value::Object(config));
        self.credential("proxmox", "token", "devices.proxmox.token_file", &proxmox.token_file);
        if let Some(path) = &proxmox.ca_file {
            self.credential("proxmox", "ca", "devices.proxmox.ca_file", path);
        }
    }

    fn glimdock(&mut self, legacy: &legacy::Config) {
        let pairing_path = &legacy.agents.pairing_path;
        let Some(collector) = &legacy.devices.collector else {
            if let Some(path) = pairing_path {
                self.skip(
                    format!("agents.pairing_path ({path})"),
                    "there is no [devices.collector] section for agent pairing to belong to",
                );
            }
            return;
        };
        let mut config = Map::new();
        config.insert("url".into(), json!(collector.url));
        config.insert("snapshot_path".into(), json!(collector.snapshot_path));
        put(&mut config, "pairing_path", pairing_path);
        self.instance("glimdock", Value::Object(config));
        self.credential("glimdock", "token", "devices.collector.token_file", &collector.token_file);
    }
}

/// `swifty.toml` for the new install: the same behaviour, with the hub's own
/// files in their new places.
fn hub_config(legacy: &legacy::Config, places: &Places) -> v2::Config {
    // The agent gets version 2's identity here. The identity the old service gave it
    // was compiled in, so the site file pins it, and `swifty migrate` lays that over.
    let assistant = legacy.assistant.as_ref().map(|old| {
        let mut assistant = old.to_shared();
        assistant.key_file = places.assistant_key();
        assistant
    });
    v2::Config {
        config_version: v2::CONFIG_VERSION,
        live: legacy.live,
        listen: legacy.listen.clone(),
        data_dir: Some(places.data_dir.clone()),
        database: None,
        secrets_dir: None,
        runs_on: Some(format!("vm{}", legacy.console_vm)),
        webauthn: legacy.webauthn.clone(),
        session: legacy.session.clone(),
        timing: legacy.timing.clone(),
        network: NetworkConfig {
            connect_timeout_ms: legacy.devices.connect_timeout_ms,
            read_timeout_ms: legacy.devices.read_timeout_ms,
            allowed_host_networks: legacy.machines.allowed_host_networks.clone(),
        },
        audit: AuditConfig { key_file: legacy.audit.key_file.as_ref().map(|_| places.audit_key()) },
        speech: legacy
            .speech
            .as_ref()
            .map(|speech| SpeechConfig { token_file: places.speech_token(), ..speech.clone() }),
        assistant,
        health: legacy.health.clone(),
    }
}

/// The hub's own files, from where the old config named them to where the new one does.
fn hub_files(legacy: &legacy::Config, places: &Places) -> Vec<HubFile> {
    let mut files = vec![HubFile { kind: FileKind::Database, from: legacy.database.clone(), to: places.database() }];
    let mut add = |kind, from: Option<&PathBuf>, to: PathBuf| {
        if let Some(from) = from {
            files.push(HubFile { kind, from: from.clone(), to });
        }
    };
    add(FileKind::AuditKey, legacy.audit.key_file.as_ref(), places.audit_key());
    add(FileKind::SpeechToken, legacy.speech.as_ref().map(|speech| &speech.token_file), places.speech_token());
    add(FileKind::AssistantKey, legacy.assistant.as_ref().map(|assistant| &assistant.key_file), places.assistant_key());
    if legacy.devices.proxmox.is_some() {
        add(FileKind::FurtherHostFiles, legacy.machines.secrets_dir.as_ref(), places.host_files());
    }
    files
}

//! The version 1 configuration: glimdock-home's `config.toml`, frozen.
//!
//! This is the parser the hub ran on before plugins, kept as it was so that
//! `swifty migrate` reads the owner's file exactly as the old service did. The
//! sections that did not change live in [`super::shared`]; everything that
//! version 2 moved or dropped (`console_vm`, `[devices.*]`, `[console]`,
//! `[machines]`, `[lights]`, `[agents]`) is here. Until the hub runs on plugins,
//! it also still runs on this file.
//!
//! Secrets are never values in this file and never environment variables: a
//! secret is always a path to a separate file. Every section refuses unknown
//! keys, so a value pasted in as `token = "..."` stops the service from starting
//! instead of being silently ignored. The error names the line and the key and
//! never repeats the value, because it goes to the service log.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::shared::{
    self, check_device_url, check_host_networks, default_agent_turn_secs, default_assistant_secs,
    default_host_networks, default_listen, is_loopback, parse_error, AuditConfig, ConfigError, HealthConfig,
    SessionConfig, SpeechConfig, TimingConfig, WeatherConfig, WebauthnConfig, MAX_DEVICE_TIMEOUT_MS,
};
use crate::inventory::parse_machine;

/// The VMs version 1 knew without being told: `console_vm` had to be one of them.
const SEED_VMS: [u32; 4] = [100, 101, 102, 103];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Only `live = true` lets adapters reach a device. Anything else is dry-run.
    #[serde(default)]
    pub live: bool,
    #[serde(default = "default_listen")]
    pub listen: String,
    pub database: PathBuf,
    /// The Proxmox VM this console runs on.
    #[serde(default = "default_console_vm")]
    pub console_vm: u32,
    #[serde(default = "default_webauthn")]
    pub webauthn: WebauthnConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub timing: TimingConfig,
    #[serde(default)]
    pub audit: AuditConfig,
    pub speech: Option<SpeechConfig>,
    pub assistant: Option<AssistantConfig>,
    #[serde(default)]
    pub devices: DevicesConfig,
    #[serde(default)]
    pub console: ConsoleConfig,
    #[serde(default)]
    pub machines: MachinesConfig,
    #[serde(default)]
    pub lights: LightsConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub health: HealthConfig,
}

/// Which VMs may have a console session.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleConfig {
    #[serde(default = "default_console_targets")]
    pub targets: Vec<String>,
}

impl Default for ConsoleConfig {
    fn default() -> Self {
        Self { targets: default_console_targets() }
    }
}

/// The NAS, Ubuntu and the Mac. Windows has its GPU passed through and no console.
pub fn default_console_targets() -> Vec<String> {
    vec!["vm101".to_string(), "vm102".to_string(), "vm103".to_string()]
}

/// The limits on adding a further Proxmox host.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachinesConfig {
    /// A further host must have an IP address inside one of these.
    #[serde(default = "default_host_networks")]
    pub allowed_host_networks: Vec<String>,
    /// The only directory a further host's token and CA files may come from.
    /// Adding a host is refused until this is set.
    pub secrets_dir: Option<PathBuf>,
}

impl Default for MachinesConfig {
    fn default() -> Self {
        Self { allowed_host_networks: default_host_networks(), secrets_dir: None }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightsConfig {
    /// Where to broadcast the WiZ discovery datagram, as IP:port. Discovery is
    /// refused until this is set.
    pub wiz_broadcast: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    /// The path of the collector's own pairing endpoint.
    pub pairing_path: Option<String>,
}

fn default_console_vm() -> u32 {
    102
}

/// Version 1 had a default origin; version 2 has none. This default is not the one
/// the glimdock-home binary had, so `swifty migrate` refuses a file that relied on
/// it rather than bind passkeys to the wrong origin.
pub fn default_webauthn() -> WebauthnConfig {
    WebauthnConfig {
        rp_id: "home.example.org".to_string(),
        origin: "https://home.example.org".to_string(),
        rp_name: "Swifty Home".to_string(),
    }
}

/// The local model, and what it may look up.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantConfig {
    /// Its OpenAI-compatible addresses, ending in /v1. The next is tried only when one
    /// cannot be reached.
    pub urls: Vec<String>,
    pub model: String,
    /// File holding its API key.
    pub key_file: PathBuf,
    /// The most one answer may take. nginx gives up at 75 seconds.
    #[serde(default = "default_assistant_secs")]
    pub timeout_secs: u64,
    pub weather: Option<WeatherConfig>,
    pub agent: Option<AgentConfig>,
}

/// An external agent with an HTTP API, for everything that is not about the home. Loopback only.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// The owner's copy: every tool, no approvals.
    pub owner_url: String,
    /// Everyone else's copy: web search only.
    pub guest_url: String,
    /// The name of the model connection saved in both copies.
    pub connection: String,
    pub model: String,
    #[serde(default = "default_agent_turn_secs")]
    pub max_turn_secs: u64,
}

impl AssistantConfig {
    /// The same section in version 2's shape. Version 1 has no agent identity, so
    /// the agent gets version 2's defaults; `swifty migrate` pins the old values.
    pub fn to_shared(&self) -> shared::AssistantConfig {
        shared::AssistantConfig {
            urls: self.urls.clone(),
            model: self.model.clone(),
            key_file: self.key_file.clone(),
            timeout_secs: self.timeout_secs,
            weather: self.weather.clone(),
            agent: self.agent.as_ref().map(AgentConfig::to_shared),
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        self.to_shared().validate()
    }
}

impl AgentConfig {
    /// The same section in version 2's shape, with version 2's default identity.
    pub fn to_shared(&self) -> shared::AgentConfig {
        shared::AgentConfig {
            owner_url: self.owner_url.clone(),
            guest_url: self.guest_url.clone(),
            connection: self.connection.clone(),
            model: self.model.clone(),
            max_turn_secs: self.max_turn_secs,
            session_prefix: shared::DEFAULT_SESSION_PREFIX.to_string(),
            client_header: shared::DEFAULT_CLIENT_HEADER.to_string(),
            client_name: shared::DEFAULT_CLIENT_NAME.to_string(),
        }
    }
}

/// Where each device is. An adapter with no section here is disabled when live.
/// Every credential is a path to its own file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevicesConfig {
    #[serde(default = "default_device_connect_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_device_read_ms")]
    pub read_timeout_ms: u64,
    pub wiz: Option<WizConfig>,
    pub hue: Option<HueConfig>,
    pub sonos: Option<SonosConfig>,
    pub webos: Option<WebosConfig>,
    pub moonraker: Option<MoonrakerConfig>,
    pub plex: Option<PlexConfig>,
    pub camera: Option<CameraConfig>,
    pub proxmox: Option<ProxmoxConfig>,
    pub collector: Option<CollectorConfig>,
    pub tuya: Option<TuyaConfig>,
    pub petkit: Option<PetkitConfig>,
    pub toshiba: Option<ToshibaConfig>,
}

impl Default for DevicesConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: default_device_connect_ms(),
            read_timeout_ms: default_device_read_ms(),
            wiz: None,
            hue: None,
            sonos: None,
            webos: None,
            moonraker: None,
            plex: None,
            camera: None,
            proxmox: None,
            collector: None,
            tuya: None,
            petkit: None,
            toshiba: None,
        }
    }
}

fn default_device_connect_ms() -> u64 {
    1_500
}

fn default_device_read_ms() -> u64 {
    2_500
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WizConfig {
    /// Bulb target to its address: an IP, or an IP and port.
    pub bulbs: BTreeMap<String, String>,
    #[serde(default = "default_wiz_port")]
    pub port: u16,
}

fn default_wiz_port() -> u16 {
    38899
}

impl WizConfig {
    /// The address of one configured bulb.
    pub fn address(&self, target: &str) -> Option<SocketAddr> {
        let text = self.bulbs.get(target)?;
        text.parse().ok().or_else(|| text.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, self.port)))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HueConfig {
    pub url: String,
    /// The application key created with the bridge's link button.
    pub key_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SonosConfig {
    pub arc_url: String,
    /// The sub is read-only here.
    pub sub_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebosConfig {
    pub url: String,
    /// The SHA-256 of the TV's certificate, in hex. Required for wss, which is all some TVs offer.
    pub cert_sha256: Option<String>,
    /// The adapter stays disabled until this file exists. Pairing is done by
    /// hand and is never started by this service.
    pub client_key_file: PathBuf,
    /// The TV's MAC address. SSAP cannot wake a TV that is off, so without this
    /// turning it on is refused.
    pub wol_mac: Option<String>,
    pub wol_broadcast: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoonrakerConfig {
    pub url: String,
    pub api_key_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlexConfig {
    pub url: String,
    pub token_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraConfig {
    pub host: String,
    #[serde(default = "default_rtsp_port")]
    pub port: u16,
    /// Holds the camera's RTSP login. The status probe does not send it.
    pub credentials_file: PathBuf,
}

fn default_rtsp_port() -> u16 {
    554
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxmoxConfig {
    pub url: String,
    pub node: String,
    /// Holds `user@realm!tokenid=secret`.
    pub token_file: PathBuf,
    /// The private CA that signed the API certificate.
    pub ca_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectorConfig {
    pub url: String,
    /// The path of the collector's snapshot endpoint.
    pub snapshot_path: String,
    pub token_file: PathBuf,
}

/// Smart sockets that speak the Tuya local protocol.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TuyaConfig {
    pub plugs: Vec<PlugConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlugConfig {
    /// The Tuya device id, in lower case.
    pub id: String,
    /// An IPv4 address and nothing else.
    pub address: String,
    #[serde(default = "default_tuya_port")]
    pub port: u16,
    /// One line: the socket's local key, base64-encoded.
    pub key_file: PathBuf,
    /// Without one it is "Socket 1", "Socket 2" and so on, in this order.
    pub name: Option<String>,
    /// The data point of a USB port that switches on its own, if the socket has one.
    pub usb_dp: Option<u16>,
}

impl PlugConfig {
    pub fn target(&self) -> String {
        format!("plug-{}", self.id)
    }

    pub fn usb_target(&self) -> Option<String> {
        self.usb_dp.map(|_| format!("plug-{}-usb", self.id))
    }
}

fn default_tuya_port() -> u16 {
    6668
}

/// The Petkit D4 feeder and T4 litter box. Petkit has no local API, so these are
/// reached through Petkit's cloud, with an unofficial interface that its own app uses.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PetkitConfig {
    /// Your country as the Petkit app has it ("DE", "FR", "US"), or "china".
    pub region: String,
    /// One line, `account:md5`: the account name and the MD5 of its sign-in secret in
    /// hex, which is all the login sends. Use a second account that the devices are
    /// shared to: a Petkit account is signed in on one device at a time, so using
    /// the main one here signs the phone app out.
    pub credentials_file: PathBuf,
    /// An IANA name, sent to Petkit as the time zone of this account.
    #[serde(default = "default_petkit_timezone")]
    pub timezone: String,
    /// Minutes east of UTC for that zone, now. The date of a feed and the time shown
    /// for the last one use it. It does not follow daylight saving time by itself.
    #[serde(default)]
    pub utc_offset_minutes: i32,
    /// Feeding and cleaning are refused until this is true, so the readings can be
    /// checked against the real devices first.
    #[serde(default)]
    pub commands: bool,
    /// Only needed when the account has more than one feeder or litter box.
    pub feeder_id: Option<u64>,
    pub litter_id: Option<u64>,
    /// Where Petkit lists its regional servers. Only a test changes this.
    #[serde(default = "default_petkit_passport")]
    pub passport_url: String,
    /// How often a reading is fetched from Petkit when nothing was just commanded.
    #[serde(default = "default_petkit_poll_secs")]
    pub poll_secs: u64,
}

fn default_petkit_timezone() -> String {
    "UTC".to_string()
}

fn default_petkit_passport() -> String {
    "https://passport.petkt.com/".to_string()
}

fn default_petkit_poll_secs() -> u64 {
    60
}

pub const MIN_PETKIT_POLL_SECS: u64 = 15;
pub const MAX_PETKIT_POLL_SECS: u64 = 600;

fn check_petkit(petkit: &PetkitConfig) -> Result<(), ConfigError> {
    let invalid = |message: &str| Err(ConfigError::Invalid(format!("devices.petkit: {message}")));
    let region = &petkit.region;
    if region.is_empty()
        || region.len() > 32
        || !region.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-'))
    {
        return invalid("region must be your country as the Petkit app has it, such as DE, or china");
    }
    if petkit.credentials_file.as_os_str().is_empty() {
        return invalid("credentials_file is needed");
    }
    let zone = &petkit.timezone;
    if zone.is_empty()
        || zone.len() > 64
        || !zone.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
    {
        return invalid("timezone must be an IANA name such as Europe/Berlin");
    }
    if !(-840..=840).contains(&petkit.utc_offset_minutes) {
        return invalid("utc_offset_minutes must be between -840 and 840");
    }
    if !(MIN_PETKIT_POLL_SECS..=MAX_PETKIT_POLL_SECS).contains(&petkit.poll_secs) {
        return invalid(&format!("poll_secs must be between {MIN_PETKIT_POLL_SECS} and {MAX_PETKIT_POLL_SECS}"));
    }
    if petkit.feeder_id == Some(0) || petkit.litter_id == Some(0) {
        return invalid("feeder_id and litter_id cannot be 0");
    }
    let url = check_device_url("devices.petkit.passport_url", &petkit.passport_url, &["http", "https"])?;
    if url.scheme() != "https" && !is_loopback(&url) {
        return invalid("passport_url must use https, so the login never crosses the network in clear");
    }
    Ok(())
}

const MAX_PLUGS: usize = 32;
const MAX_PLUG_NAME: usize = 64;

/// A Tuya device id: lower-case letters and digits, of a sensible length.
pub fn is_plug_id(id: &str) -> bool {
    (10..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn check_tuya(tuya: &TuyaConfig) -> Result<(), ConfigError> {
    let invalid = |message: String| Err(ConfigError::Invalid(message));
    if tuya.plugs.is_empty() || tuya.plugs.len() > MAX_PLUGS {
        return invalid(format!("devices.tuya.plugs must list 1 to {MAX_PLUGS} sockets"));
    }
    let mut ids = std::collections::HashSet::new();
    let mut places = std::collections::HashSet::new();
    for plug in &tuya.plugs {
        if !is_plug_id(&plug.id) {
            return invalid("devices.tuya.plugs: an id must be 10 to 32 lower-case letters and digits".to_string());
        }
        let id = &plug.id;
        if !ids.insert(id) {
            return invalid(format!("devices.tuya.plugs: {id} is listed twice"));
        }
        let Ok(ip) = plug.address.parse::<Ipv4Addr>() else {
            return invalid(format!("devices.tuya.plugs: the address of {id} must be an IPv4 address"));
        };
        if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
            return invalid(format!("devices.tuya.plugs: the address of {id} cannot be a socket"));
        }
        if plug.port == 0 {
            return invalid(format!("devices.tuya.plugs: the port of {id} cannot be 0"));
        }
        if !places.insert((ip, plug.port)) {
            return invalid(format!("devices.tuya.plugs: the address of {id} is used twice"));
        }
        if plug.key_file.as_os_str().is_empty() {
            return invalid(format!("devices.tuya.plugs: {id} needs a key_file"));
        }
        if plug.usb_dp.is_some_and(|point| !(2..=255).contains(&point)) {
            return invalid(format!("devices.tuya.plugs: the usb_dp of {id} must be 2 to 255"));
        }
        if let Some(name) = &plug.name {
            let plain = !name.trim().is_empty() && name.trim() == name && !name.chars().any(char::is_control);
            if !plain || name.chars().count() > MAX_PLUG_NAME {
                return invalid(format!(
                    "devices.tuya.plugs: the name of {id} must be 1 to {MAX_PLUG_NAME} plain characters"
                ));
            }
        }
    }
    Ok(())
}

/// Toshiba air conditioners on Wi-Fi, reached through the Home AC Control cloud.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToshibaConfig {
    /// One line: base64 of the JSON that signs in to the account.
    pub credentials_file: PathBuf,
    #[serde(default = "default_toshiba_api_url")]
    pub api_url: String,
    /// The hub that takes commands. A token for any other hub is refused.
    #[serde(default = "default_toshiba_hub_url")]
    pub hub_url: String,
    /// How long one request to the cloud may take to answer.
    #[serde(default = "default_toshiba_timeout_ms")]
    pub timeout_ms: u64,
    pub units: Vec<AirconConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirconConfig {
    /// The `Id` the cloud lists for this unit.
    pub id: String,
    /// The `DeviceUniqueId` the cloud lists for this unit. Commands are addressed to it.
    pub unique_id: String,
    pub name: Option<String>,
}

impl AirconConfig {
    pub fn target(&self) -> String {
        format!("ac-{}", self.id.to_ascii_lowercase())
    }
}

fn default_toshiba_api_url() -> String {
    "https://mobileapi.toshibahomeaccontrols.com".to_string()
}

fn default_toshiba_hub_url() -> String {
    "https://toshibasmaciothubprod.azure-devices.net".to_string()
}

fn default_toshiba_timeout_ms() -> u64 {
    10_000
}

const MAX_AIRCONS: usize = 8;
const MAX_AIRCON_UNIQUE_ID: usize = 128;
const MAX_TOSHIBA_TIMEOUT_MS: u64 = 30_000;

/// A Toshiba unit id: letters, digits and hyphens, of a sensible length.
pub fn is_aircon_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn is_plain_unique_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_AIRCON_UNIQUE_ID
        && id.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\'))
}

/// A cloud's address: only https, unless it is this machine.
fn check_cloud_url(field: &str, text: &str) -> Result<(), ConfigError> {
    let url = check_device_url(field, text, &["http", "https"])?;
    if url.scheme() != "https" && !is_loopback(&url) {
        return Err(ConfigError::Invalid(format!("{field} must use https unless it is this machine")));
    }
    Ok(())
}

fn check_toshiba(toshiba: &ToshibaConfig) -> Result<(), ConfigError> {
    let invalid = |message: String| Err(ConfigError::Invalid(message));
    if toshiba.credentials_file.as_os_str().is_empty() {
        return invalid("devices.toshiba needs a credentials_file".to_string());
    }
    check_cloud_url("devices.toshiba.api_url", &toshiba.api_url)?;
    check_cloud_url("devices.toshiba.hub_url", &toshiba.hub_url)?;
    if toshiba.timeout_ms == 0 || toshiba.timeout_ms > MAX_TOSHIBA_TIMEOUT_MS {
        return invalid(format!("devices.toshiba.timeout_ms must be 1 to {MAX_TOSHIBA_TIMEOUT_MS}"));
    }
    if toshiba.units.is_empty() || toshiba.units.len() > MAX_AIRCONS {
        return invalid(format!("devices.toshiba.units must list 1 to {MAX_AIRCONS} air conditioners"));
    }
    let mut targets = std::collections::HashSet::new();
    for unit in &toshiba.units {
        let id = &unit.id;
        if !is_aircon_id(id) {
            return invalid("devices.toshiba.units: an id must be 8 to 64 letters, digits and hyphens".to_string());
        }
        if !targets.insert(unit.target()) {
            return invalid(format!("devices.toshiba.units: {id} is listed twice"));
        }
        if !is_plain_unique_id(&unit.unique_id) {
            return invalid(format!(
                "devices.toshiba.units: the unique_id of {id} must be 1 to {MAX_AIRCON_UNIQUE_ID} plain characters"
            ));
        }
        if let Some(name) = &unit.name {
            let plain = !name.trim().is_empty() && name.trim() == name && !name.chars().any(char::is_control);
            if !plain || name.chars().count() > MAX_PLUG_NAME {
                return invalid(format!(
                    "devices.toshiba.units: the name of {id} must be 1 to {MAX_PLUG_NAME} plain characters"
                ));
            }
        }
    }
    Ok(())
}

fn is_hostname(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 253
        && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

/// An absolute path on a device. A leading // is a scheme-relative URL: it would
/// name another host, and the device's token would go there.
fn is_plain_path(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.contains(['?', '#', '\\']) && !path.contains("..")
}

pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = text.split([':', '-']).collect();
    let parts: [&str; 6] = parts.try_into().ok()?;
    let mut mac = [0u8; 6];
    for (slot, part) in mac.iter_mut().zip(parts) {
        if part.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    Some(mac)
}

impl DevicesConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |message: String| Err(ConfigError::Invalid(message));
        for (name, value) in
            [("devices.connect_timeout_ms", self.connect_timeout_ms), ("devices.read_timeout_ms", self.read_timeout_ms)]
        {
            if value == 0 || value > MAX_DEVICE_TIMEOUT_MS {
                return invalid(format!("{name} must be between 1 and {MAX_DEVICE_TIMEOUT_MS}"));
            }
        }
        const HTTP: &[&str] = &["http", "https"];
        if let Some(wiz) = &self.wiz {
            for target in wiz.bulbs.keys() {
                if !matches!(target.as_str(), "bulb1" | "bulb2" | "bulb3" | "bulb4") {
                    return invalid(format!("devices.wiz.bulbs: {target} is not a bulb target"));
                }
                if wiz.address(target).is_none() {
                    return invalid(format!("devices.wiz.bulbs.{target} must be an IP address"));
                }
            }
        }
        if let Some(hue) = &self.hue {
            check_device_url("devices.hue.url", &hue.url, HTTP)?;
        }
        if let Some(sonos) = &self.sonos {
            check_device_url("devices.sonos.arc_url", &sonos.arc_url, &["http"])?;
            if let Some(sub) = &sonos.sub_url {
                check_device_url("devices.sonos.sub_url", sub, &["http"])?;
            }
        }
        if let Some(webos) = &self.webos {
            let url = check_device_url("devices.webos.url", &webos.url, &["ws", "wss"])?;
            match (url.scheme(), &webos.cert_sha256) {
                ("wss", Some(pin)) if pin.len() == 64 && pin.chars().all(|c| c.is_ascii_hexdigit()) => {}
                ("wss", _) => {
                    return invalid("devices.webos.cert_sha256 must be the certificate's SHA-256 in hex".to_string())
                }
                ("ws", Some(_)) => return invalid("devices.webos.cert_sha256 only applies to wss".to_string()),
                _ => {}
            }
            if let Some(mac) = &webos.wol_mac {
                if parse_mac(mac).is_none() {
                    return invalid("devices.webos.wol_mac is not a MAC address".to_string());
                }
            }
            if let Some(broadcast) = &webos.wol_broadcast {
                if broadcast.parse::<SocketAddr>().is_err() {
                    return invalid("devices.webos.wol_broadcast must be IP:port".to_string());
                }
            }
        }
        if let Some(moonraker) = &self.moonraker {
            check_device_url("devices.moonraker.url", &moonraker.url, HTTP)?;
        }
        if let Some(plex) = &self.plex {
            check_device_url("devices.plex.url", &plex.url, HTTP)?;
        }
        if let Some(camera) = &self.camera {
            if !is_hostname(&camera.host) {
                return invalid("devices.camera.host must be a plain host name or IP".to_string());
            }
        }
        if let Some(proxmox) = &self.proxmox {
            let url = check_device_url("devices.proxmox.url", &proxmox.url, HTTP)?;
            if url.scheme() != "https" && !is_loopback(&url) {
                return invalid(
                    "devices.proxmox.url must use https, so its token never crosses the network in clear".to_string(),
                );
            }
            let node = &proxmox.node;
            if node.is_empty() || node.len() > 63 || !node.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return invalid("devices.proxmox.node must be a plain node name".to_string());
            }
        }
        if let Some(collector) = &self.collector {
            check_device_url("devices.collector.url", &collector.url, HTTP)?;
            if !is_plain_path(&collector.snapshot_path) {
                return invalid("devices.collector.snapshot_path must be a plain absolute path".to_string());
            }
        }
        if let Some(tuya) = &self.tuya {
            check_tuya(tuya)?;
        }
        if let Some(petkit) = &self.petkit {
            check_petkit(petkit)?;
        }
        if let Some(toshiba) = &self.toshiba {
            check_toshiba(toshiba)?;
        }
        Ok(())
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text =
            std::fs::read_to_string(path).map_err(|source| ConfigError::Read { path: path.to_path_buf(), source })?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(text).map_err(|error| parse_error(&error, text))?;
        config.validate()?;
        Ok(config)
    }

    /// The console list, the limits on further hosts, and the discovery address.
    fn validate_home(&self) -> Result<(), ConfigError> {
        let invalid = |message: String| Err(ConfigError::Invalid(message));
        if let Some(bad) = self.console.targets.iter().find(|target| parse_machine(target).is_none()) {
            return invalid(format!("console.targets: {bad} is not a VM target"));
        }
        check_host_networks("machines.allowed_host_networks", &self.machines.allowed_host_networks)?;
        if self.machines.secrets_dir.as_ref().is_some_and(|dir| !dir.is_absolute()) {
            return invalid("machines.secrets_dir must be an absolute path".into());
        }
        if let Some(address) = &self.lights.wiz_broadcast {
            if !address.parse::<SocketAddr>().is_ok_and(|a| a.is_ipv4()) {
                return invalid("lights.wiz_broadcast must be IPv4 address:port".into());
            }
        }
        if let Some(path) = &self.agents.pairing_path {
            if !is_plain_path(path) {
                return invalid("agents.pairing_path must be a plain absolute path".into());
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |message: String| Err(ConfigError::Invalid(message));
        if !SEED_VMS.contains(&self.console_vm) {
            return invalid(format!("console_vm must be one of {SEED_VMS:?}"));
        }
        self.timing.validate(self.live)?;
        self.validate_home()?;
        self.webauthn.validate()?;
        self.session.validate()?;
        if let Some(speech) = &self.speech {
            speech.validate()?;
        }
        if let Some(assistant) = &self.assistant {
            assistant.validate()?;
        }
        self.devices.validate()
    }

    pub fn session_absolute_ms(&self) -> i64 {
        self.session.absolute_secs as i64 * 1000
    }

    pub fn session_idle_ms(&self) -> i64 {
        self.session.idle_secs as i64 * 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"database = "home.db""#;

    #[test]
    fn dry_run_is_the_default() {
        let config = Config::parse(MINIMAL).unwrap();
        assert!(!config.live);
        assert_eq!(config.webauthn.rp_id, "home.example.org");
        assert_eq!(config.webauthn.origin, "https://home.example.org");
        assert_eq!(config.session.absolute_secs, 8 * 3600);
        assert_eq!(config.console_vm, 102);
    }

    #[test]
    fn live_must_be_a_real_boolean() {
        assert!(Config::parse("live = true\ndatabase = \"x\"").unwrap().live);
        assert!(Config::parse("live = \"true\"\ndatabase = \"x\"").is_err());
        assert!(Config::parse("live = 1\ndatabase = \"x\"").is_err());
    }

    #[test]
    fn inline_secrets_are_refused() {
        let text = r#"
            database = "x"
            [speech]
            url = "http://127.0.0.1:1"
            token_file = "t"
            token = "inline-secret"
        "#;
        assert!(Config::parse(text).is_err());
        assert!(Config::parse("database = \"x\"\napi_key = \"inline\"").is_err());
    }

    #[test]
    fn a_pasted_secret_is_never_repeated_in_the_error() {
        const PASTED: &str = "swifty@pve!home=PASTED-PROXMOX-SECRET";
        // An unknown key, the case the refusal exists for.
        let text = format!(
            "database = \"x\"\n[devices.proxmox]\nurl = \"https://p:8006\"\nnode = \"n\"\ntoken_file = \"t\"\ntoken = \"{PASTED}\"\n"
        );
        let message = Config::parse(&text).unwrap_err().to_string();
        assert!(!message.contains(PASTED), "{message}");
        assert!(!message.contains("PASTED"), "{message}");
        assert!(message.contains("line 6"), "{message}");
        assert!(message.contains("unknown field `token`"), "{message}");

        // Pasted into a key that exists but takes another type, where serde
        // quotes the value in its own message.
        for text in [
            format!("database = \"x\"\nlive = \"{PASTED}\"\n"),
            format!("database = \"x\"\nconsole_vm = \"{PASTED}\"\n"),
            format!("database = 12345\nlive = \"{PASTED}\"\n"),
            format!("database = \"x\"\n[speech]\nurl = \"{PASTED}\"\ntoken_file = 987654321\n"),
            format!("database = \"x\"\n{PASTED}\n"),
        ] {
            let message = Config::parse(&text).unwrap_err().to_string();
            assert!(!message.contains("PASTED"), "{message}");
            assert!(!message.contains("987654321"), "{message}");
            assert!(message.starts_with("invalid configuration:"), "{message}");
        }
    }

    #[test]
    fn console_vm_must_be_on_the_allow_list() {
        let with = |vm: &str| Config::parse(&format!("database = \"x\"\nconsole_vm = {vm}"));
        for vm in [100, 101, 102, 103] {
            assert_eq!(with(&vm.to_string()).unwrap().console_vm, vm);
        }
        for vm in ["0", "99", "104", "999", "1020", "4294967295"] {
            let error = with(vm).unwrap_err().to_string();
            assert!(error.contains("console_vm"), "{error}");
        }
    }

    #[test]
    fn timing_is_range_checked() {
        let with = |body: &str| {
            Config::parse(&format!(
                "database = \"x\"\n[timing]\npoll_ms = 250\nsync_wait_ms = 4000\nduplicate_window_ms = 10000\n{body}"
            ))
        };
        assert!(with("").is_ok());
        assert!(with("confirm_timeout_ms = 1").is_ok(), "allowed while nothing is live");

        let raw = |poll: i64, sync: i64, window: i64| {
            Config::parse(&format!(
                "database = \"x\"\n[timing]\npoll_ms = {poll}\nsync_wait_ms = {sync}\nduplicate_window_ms = {window}"
            ))
        };
        assert!(raw(250, 4000, 10_000).is_ok());
        for (poll, sync, window) in [
            (0, 4000, 10_000),
            (49, 4000, 10_000),
            (10_001, 4000, 10_000),
            (250, 0, 10_000),
            (250, 60_001, 10_000),
            (250, 4000, 0),
            (250, 4000, -1),
            (250, 4000, 9_999),
            (250, 4000, 3_600_001),
        ] {
            assert!(raw(poll, sync, window).is_err(), "{poll} {sync} {window}");
        }

        let live = "live = true\ndatabase = \"x\"\n[timing]\npoll_ms = 250\nsync_wait_ms = 4000\nduplicate_window_ms = 10000\nconfirm_timeout_ms = 1";
        assert!(Config::parse(live).unwrap_err().to_string().contains("confirm_timeout_ms"));
    }

    #[test]
    fn a_scheme_relative_snapshot_path_is_refused() {
        let collector = |path: &str| {
            devices(&format!("[devices.collector]\nurl = \"http://c\"\nsnapshot_path = \"{path}\"\ntoken_file = \"t\""))
        };
        assert!(collector("/api/snapshot").is_ok());
        assert!(collector("//127.0.0.1:52409/stolen").is_err());
        assert!(collector("//evil.example/stolen").is_err());
    }

    #[test]
    fn the_speech_url_follows_the_device_url_rules() {
        let speech = |url: &str, extra: &str| {
            Config::parse(&format!("database = \"x\"\n[speech]\nurl = \"{url}\"\ntoken_file = \"t\"\n{extra}"))
        };
        assert!(speech("http://127.0.0.1:8791", "").is_ok());
        assert!(speech("http://localhost:8791", "").is_ok());
        assert!(speech("https://speech.example:8791", "").is_ok());
        for url in [
            "http://192.0.2.9:8791",
            "http://user:inline-secret@192.0.2.9:8791",
            "https://user:inline-secret@192.0.2.9:8791",
            "http://127.0.0.1:8791/some/path",
            "http://127.0.0.1:8791/?key=inline-secret",
            "ftp://127.0.0.1:8791",
        ] {
            assert!(speech(url, "").is_err(), "{url}");
        }
        assert!(speech("http://127.0.0.1:1", "connect_timeout_secs = 30\ntimeout_secs = 120").is_ok());
        for extra in
            ["connect_timeout_secs = 0", "connect_timeout_secs = 31", "timeout_secs = 0", "timeout_secs = 86400"]
        {
            assert!(speech("http://127.0.0.1:1", extra).is_err(), "{extra}");
        }
    }

    #[test]
    fn the_console_list_defaults_to_the_nas_ubuntu_and_the_mac() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.console.targets, ["vm101", "vm102", "vm103"]);
        assert!(Config::parse("database = \"x\"\n[console]\ntargets = [\"vm100/../shell\"]").is_err());
        assert!(Config::parse("database = \"x\"\n[console]\ntargets = [\"host\"]").is_err());
    }

    #[test]
    fn host_networks_are_checked_and_cannot_be_everything() {
        let with = |list: &str| Config::parse(&format!("database = \"x\"\n[machines]\nallowed_host_networks = {list}"));
        assert!(with("[\"198.51.100.0/24\"]").is_ok());
        for bad in [
            "[\"0.0.0.0/0\"]",
            "[\"::/0\"]",
            "[\"128.0.0.0/1\"]",
            "[\"10.0.0.0\"]",
            "[\"10.0.0.0/33\"]",
            "[\"nope/8\"]",
        ] {
            assert!(with(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn discovery_and_pairing_settings_are_checked() {
        assert!(Config::parse("database = \"x\"\n[lights]\nwiz_broadcast = \"192.0.2.255:38899\"").is_ok());
        assert!(Config::parse("database = \"x\"\n[lights]\nwiz_broadcast = \"everywhere\"").is_err());
        assert!(Config::parse("database = \"x\"\n[agents]\npairing_path = \"/pair\"").is_ok());
        assert!(Config::parse("database = \"x\"\n[agents]\npairing_path = \"//evil.example/pair\"").is_err());
    }

    #[test]
    fn eight_hour_session_cap_is_enforced() {
        let text = "database = \"x\"\n[session]\nabsolute_secs = 28801\nidle_secs = 60";
        assert!(Config::parse(text).is_err());
    }

    #[test]
    fn origin_rules() {
        let with = |origin: &str, rp: &str| {
            Config::parse(&format!(
                "database = \"x\"\n[webauthn]\nrp_id = \"{rp}\"\norigin = \"{origin}\"\nrp_name = \"n\""
            ))
        };
        assert!(with("https://home.example.org", "home.example.org").is_ok());
        assert!(with("http://home.example.org", "home.example.org").is_err());
        assert!(with("https://home.example.org/", "home.example.org").is_err());
        assert!(with("https://evil.example", "home.example.org").is_err());
        assert!(with("http://localhost:8790", "localhost").is_ok());
    }

    fn devices(section: &str) -> Result<Config, ConfigError> {
        Config::parse(&format!("database = \"x\"\n{section}"))
    }

    #[test]
    fn device_sections_are_optional_and_have_timeouts() {
        let config = Config::parse(MINIMAL).unwrap();
        assert!(config.devices.wiz.is_none());
        assert_eq!(config.devices.connect_timeout_ms, 1_500);
        assert_eq!(config.devices.read_timeout_ms, 2_500);
        assert!(devices("[devices]\nconnect_timeout_ms = 0").is_err());
        assert!(devices("[devices]\nread_timeout_ms = 30001").is_err());
    }

    #[test]
    fn device_credentials_cannot_be_inline() {
        for section in [
            "[devices.proxmox]\nurl = \"https://p:8006\"\nnode = \"n\"\ntoken_file = \"t\"\ntoken = \"inline\"",
            "[devices.hue]\nurl = \"http://h\"\nkey_file = \"k\"\nkey = \"inline\"",
            "[devices.plex]\nurl = \"http://p\"\ntoken_file = \"k\"\ntoken = \"inline\"",
            "[devices.webos]\nurl = \"ws://t:3000\"\nclient_key_file = \"k\"\nclient_key = \"inline\"",
        ] {
            assert!(devices(section).is_err(), "{section}");
        }
    }

    #[test]
    fn a_toshiba_unit_has_to_be_described_exactly() {
        let toshiba = |first: &str, units: &str| devices(&format!("[devices.toshiba]\n{first}\n{units}"));
        let unit = "[[devices.toshiba.units]]\nid = \"0a1b2c3d-1111-2222-3333-444455556666\"\nunique_id = \"AB-12:cd\"";
        let login = "credentials_file = \"/etc/x/toshiba.login\"";
        assert!(toshiba(login, unit).is_ok());
        let found = toshiba(login, unit).unwrap();
        let settings = found.devices.toshiba.unwrap();
        assert_eq!(settings.api_url, "https://mobileapi.toshibahomeaccontrols.com");
        assert_eq!(settings.hub_url, "https://toshibasmaciothubprod.azure-devices.net");
        assert_eq!(settings.units[0].target(), "ac-0a1b2c3d-1111-2222-3333-444455556666");
        assert!(
            toshiba(&format!("{login}\ntimeout_ms = 30000\nhub_url = \"https://h.azure-devices.net\""), unit).is_ok()
        );
        assert!(
            toshiba(&format!("{login}\napi_url = \"http://127.0.0.1:9\""), unit).is_ok(),
            "this machine, for a test"
        );
        for bad in [
            format!("{login}\napi_url = \"http://192.0.2.1\""),
            format!("{login}\nhub_url = \"http://hub.test\""),
            format!("{login}\napi_url = \"https://h.test/api\""),
            format!("{login}\ntimeout_ms = 0"),
            format!("{login}\ntimeout_ms = 30001"),
            format!("{login}\nsecret = \"inline\""),
            String::new(),
        ] {
            assert!(toshiba(&bad, unit).is_err(), "{bad}");
        }
        let with = |body: &str| toshiba(login, &format!("[[devices.toshiba.units]]\n{body}"));
        for bad in [
            "id = \"short\"\nunique_id = \"x\"",
            "id = \"has spaces here\"\nunique_id = \"x\"",
            "id = \"0a1b2c3d\"\nunique_id = \"\"",
            "id = \"0a1b2c3d\"\nunique_id = \"has space\"",
            "id = \"0a1b2c3d\"\nunique_id = \"quo\\\"te\"",
            "id = \"0a1b2c3d\"\nunique_id = \"x\"\nname = \"\"",
            "id = \"0a1b2c3d\"\nunique_id = \"x\"\naddress = \"192.0.2.1\"",
        ] {
            assert!(with(bad).is_err(), "{bad}");
        }
        assert!(with("id = \"0a1b2c3d\"\nunique_id = \"x\"\nname = \"Lounge\"").is_ok());
        let twice = format!(
            "{unit}\n[[devices.toshiba.units]]\nid = \"0A1B2C3D-1111-2222-3333-444455556666\"\nunique_id = \"y\""
        );
        assert!(toshiba(login, &twice).is_err(), "the same id in another case");
        assert!(toshiba(login, "units = []").is_err(), "nothing listed");
    }

    #[test]
    fn device_urls_are_only_an_authority() {
        let plug = |body: &str| devices(&format!("[[devices.tuya.plugs]]\n{body}"));
        let good = "id = \"bf0123456789abcdef\"\naddress = \"192.0.2.40\"\nkey_file = \"/k\"";
        assert!(plug(good).is_ok());
        assert!(plug(&format!("{good}\nname = \"Desk\"\nport = 7000")).is_ok());
        for bad in [
            "id = \"BF0123456789ABCDEF\"\naddress = \"192.0.2.40\"\nkey_file = \"/k\"",
            "id = \"short\"\naddress = \"192.0.2.40\"\nkey_file = \"/k\"",
            "id = \"bf0123456789abcde/\"\naddress = \"192.0.2.40\"\nkey_file = \"/k\"",
            "id = \"bf0123456789abcdef\"\naddress = \"socket.example\"\nkey_file = \"/k\"",
            "id = \"bf0123456789abcdef\"\naddress = \"192.0.2.40:6668\"\nkey_file = \"/k\"",
            "id = \"bf0123456789abcdef\"\naddress = \"::1\"\nkey_file = \"/k\"",
            "id = \"bf0123456789abcdef\"\naddress = \"0.0.0.0\"\nkey_file = \"/k\"",
            "id = \"bf0123456789abcdef\"\naddress = \"224.0.0.1\"\nkey_file = \"/k\"",
            "id = \"bf0123456789abcdef\"\naddress = \"255.255.255.255\"\nkey_file = \"/k\"",
            "id = \"bf0123456789abcdef\"\naddress = \"192.0.2.40\"\nkey_file = \"\"",
            "id = \"bf0123456789abcdef\"\naddress = \"192.0.2.40\"",
            "id = \"bf0123456789abcdef\"\naddress = \"192.0.2.40\"\nkey_file = \"/k\"\nport = 0",
            "id = \"bf0123456789abcdef\"\naddress = \"192.0.2.40\"\nkey_file = \"/k\"\nname = \" x \"",
            "id = \"bf0123456789abcdef\"\naddress = \"192.0.2.40\"\nkey_file = \"/k\"\nname = \"\"",
            "id = \"bf0123456789abcdef\"\naddress = \"192.0.2.40\"\nkey_file = \"/k\"\nkey = \"inline\"",
        ] {
            assert!(plug(bad).is_err(), "{bad}");
        }
        let twice = |second_id: &str, second_ip: &str| {
            let second = format!("id = \"{second_id}\"\naddress = \"{second_ip}\"\nkey_file = \"/j\"");
            devices(&format!("[[devices.tuya.plugs]]\n{good}\n[[devices.tuya.plugs]]\n{second}"))
        };
        assert!(twice("bf0123456789abcde2", "192.0.2.41").is_ok());
        assert!(twice("bf0123456789abcdef", "192.0.2.41").is_err(), "the same id");
        assert!(twice("bf0123456789abcde2", "192.0.2.40").is_err(), "the same address");
        assert!(devices("[devices.tuya]\nplugs = []").is_err(), "nothing listed");
        let hue = |url: &str| devices(&format!("[devices.hue]\nurl = \"{url}\"\nkey_file = \"k\""));
        assert!(hue("http://192.0.2.1").is_ok());
        assert!(hue("http://192.0.2.1/api").is_err());
        assert!(hue("http://user:x@192.0.2.1").is_err());
        assert!(hue("http://192.0.2.1?a=b").is_err());
        assert!(hue("ftp://192.0.2.1").is_err());
    }

    #[test]
    fn the_proxmox_token_never_crosses_the_network_in_clear() {
        let proxmox =
            |url: &str| devices(&format!("[devices.proxmox]\nurl = \"{url}\"\nnode = \"pve\"\ntoken_file = \"t\""));
        assert!(proxmox("https://192.0.2.1:8006").is_ok());
        assert!(proxmox("http://192.0.2.1:8006").is_err());
        assert!(proxmox("http://127.0.0.1:8006").is_ok());
        assert!(devices("[devices.proxmox]\nurl = \"https://p:8006\"\nnode = \"a/b\"\ntoken_file = \"t\"").is_err());
    }

    #[test]
    fn the_petkit_section_is_checked_and_its_login_is_never_inline() {
        let petkit = |body: &str| devices(&format!("[devices.petkit]\n{body}"));
        let good = "region = \"FR\"\ncredentials_file = \"/c\"";
        assert!(petkit(good).is_ok());
        assert!(
            petkit(&format!("{good}\ntimezone = \"Europe/Paris\"\nutc_offset_minutes = 120\ncommands = true")).is_ok()
        );
        assert!(petkit(&format!("{good}\nfeeder_id = 12345\nlitter_id = 67890\npoll_secs = 120")).is_ok());
        for bad in [
            "region = \"\"\ncredentials_file = \"/c\"",
            "region = \"B/G\"\ncredentials_file = \"/c\"",
            "region = \"FR\"\ncredentials_file = \"\"",
            "region = \"FR\"",
            "credentials_file = \"/c\"",
            "region = \"FR\"\ncredentials_file = \"/c\"\ntimezone = \"Europe Paris\"",
            "region = \"FR\"\ncredentials_file = \"/c\"\nutc_offset_minutes = 900",
            "region = \"FR\"\ncredentials_file = \"/c\"\npoll_secs = 5",
            "region = \"FR\"\ncredentials_file = \"/c\"\npoll_secs = 601",
            "region = \"FR\"\ncredentials_file = \"/c\"\nfeeder_id = 0",
            "region = \"FR\"\ncredentials_file = \"/c\"\npassport_url = \"http://passport.example\"",
            "region = \"FR\"\ncredentials_file = \"/c\"\npassport_url = \"https://passport.example/x\"",
            "region = \"FR\"\ncredentials_file = \"/c\"\nmd5 = \"inline\"",
            "region = \"FR\"\ncredentials_file = \"/c\"\nusername = \"inline\"",
        ] {
            assert!(petkit(bad).is_err(), "{bad}");
        }
        assert!(petkit(&format!("{good}\npassport_url = \"http://127.0.0.1:9\"")).is_ok(), "loopback is for tests");
    }

    #[test]
    fn bulb_addresses_are_checked() {
        let wiz = |body: &str| devices(&format!("[devices.wiz.bulbs]\n{body}"));
        let ok = wiz("bulb1 = \"192.0.2.1\"\nbulb2 = \"192.0.2.2:40000\"").unwrap();
        let section = ok.devices.wiz.unwrap();
        assert_eq!(section.address("bulb1").unwrap().to_string(), "192.0.2.1:38899");
        assert_eq!(section.address("bulb2").unwrap().to_string(), "192.0.2.2:40000");
        assert_eq!(section.address("bulb3"), None);
        assert!(wiz("bulb5 = \"192.0.2.1\"").is_err());
        assert!(wiz("bulb1 = \"not an address\"").is_err());
    }

    #[test]
    fn collector_and_tv_settings_are_checked() {
        let collector = |path: &str| {
            devices(&format!("[devices.collector]\nurl = \"http://c\"\nsnapshot_path = \"{path}\"\ntoken_file = \"t\""))
        };
        assert!(collector("/snapshot").is_ok());
        assert!(collector("snapshot").is_err());
        assert!(collector("/a?x=1").is_err());
        assert!(collector("/a/../b").is_err());
        let tv =
            |extra: &str| devices(&format!("[devices.webos]\nurl = \"ws://t:3000\"\nclient_key_file = \"k\"\n{extra}"));
        assert!(tv("wol_mac = \"aa:bb:cc:dd:ee:ff\"").is_ok());
        assert!(tv("wol_mac = \"aa:bb:cc\"").is_err());
        assert!(tv("wol_broadcast = \"nowhere\"").is_err());
        assert_eq!(parse_mac("AA-bb-CC-dd-EE-FF"), Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]));
        assert!(devices("[devices.webos]\nurl = \"http://t:3000\"\nclient_key_file = \"k\"").is_err());
    }
}

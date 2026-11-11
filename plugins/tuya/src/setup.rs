//! The guided setup. It replaces the deploy step that fetched keys with a Python
//! helper and the hand-written `[[devices.tuya.plugs]]`.
//!
//! ```text
//! Choose: look the keys up in the cloud, or "I have the keys"
//!   cloud:  Form (data center, access id, access secret) -> lookup -> Choose (sockets)
//!           -> Form (each chosen socket's address) -> read each -> Done
//!   manual: Form (id, address, local key; "add another") ... -> Choose (sockets) -> read each -> Done
//! ```
//!
//! The cloud path needs a live hub and uses the cloud only during setup. The manual
//! path is a form and works in dry-run too, where nothing is contacted. When the hub
//! is live, every socket with a newly entered key is read once before the result is
//! handed over, and a socket that does not answer stops the setup with a reason.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;
use swifty_plugin_api::{
    Choice, Field, FieldType, Scalar, Secret, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep,
};
use tokio::sync::Mutex;

use crate::cloud::{region, Cloud, CloudDevice, MAX_ACCESS_ID, REGIONS};
use crate::config::{
    check, check_address, is_plain_name, is_plug_id, key_slot, PlugConfig, TuyaConfig, DEFAULT_PORT, MAX_PLUGS,
    MAX_PLUG_NAME,
};
use crate::driver::{plug_address, query};
use crate::protocol::{cipher_from_stored, is_on, stored_key, MAIN_SWITCH};

const LIVE_NEEDED: &str = "Looking the keys up in Tuya's cloud needs a live hub (live = true). \
                           In dry-run, choose \"I have the keys\" instead.";
const DOES_NOT_FIT: &str = "That answer does not fit this step.";

/// One socket the setup knows about.
struct Socket {
    plug: PlugConfig,
    /// A key entered or looked up in this setup, in the form the slot stores.
    new_key: Option<Secret>,
    /// Whether its slot already holds a key.
    key_stored: bool,
}

impl Socket {
    fn has_key(&self) -> bool {
        self.new_key.is_some() || self.key_stored
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    Path,
    CloudForm,
    CloudChoose,
    CloudPlaces { chosen: Vec<String> },
    ManualForm,
    ManualChoose,
}

/// The setup flow for one instance of the plugin.
pub struct TuyaSetup {
    /// Replaces every region's host. Only a test cloud on loopback sets it.
    cloud_base: Option<String>,
    stage: Stage,
    /// The sockets so far: the stored ones first, then those added here.
    sockets: Vec<Socket>,
    /// What the cloud lookup found.
    found: Vec<CloudDevice>,
}

impl Default for TuyaSetup {
    fn default() -> Self {
        Self::new()
    }
}

impl TuyaSetup {
    /// The flow as the hub runs it.
    pub fn new() -> Self {
        Self { cloud_base: None, stage: Stage::Path, sockets: Vec::new(), found: Vec::new() }
    }

    /// The same flow, with every data center replaced by the cloud at `base`
    /// (`http://127.0.0.1:<port>`). For tests against a mock cloud.
    pub fn with_cloud(base: &str) -> Self {
        Self { cloud_base: Some(base.to_string()), ..Self::new() }
    }

    fn path_step(&self) -> SetupStep {
        SetupStep::Choose {
            title: "Add Tuya sockets".to_string(),
            text: "Each socket has a local key that Tuya keeps in its cloud. Swifty Home can look the keys up \
                   in your Tuya cloud project once, or you can enter them yourself. Either way the sockets are \
                   switched on your own network afterwards."
                .to_string(),
            options: vec![
                Choice {
                    id: "cloud".to_string(),
                    label: "Look the keys up in my Tuya cloud project".to_string(),
                    detail: "Needs a cloud project on the Tuya developer platform, linked to your Tuya app \
                             account, and its access id and secret. The secret is used once and not stored."
                        .to_string(),
                    selected: true,
                },
                Choice {
                    id: "manual".to_string(),
                    label: "I have the keys".to_string(),
                    detail: "Enter each socket's device id, address and local key.".to_string(),
                    selected: false,
                },
            ],
            multiple: false,
            min: 1,
        }
    }

    fn cloud_form(&self) -> SetupStep {
        let regions = REGIONS.iter().map(|region| format!("{} ({})", region.code, region.label)).collect::<Vec<_>>();
        SetupStep::Form {
            title: "Your Tuya cloud project".to_string(),
            text: "On the Tuya developer platform, open your cloud project. Its overview shows the access id \
                   and the access secret, and the data center it was created in."
                .to_string(),
            fields: vec![
                field(
                    "region",
                    "Data center",
                    &format!("Where the project lives: {}.", regions.join(", ")),
                    FieldType::Choice { options: REGIONS.iter().map(|region| region.code.to_string()).collect() },
                    true,
                    Some(Scalar::Text(REGIONS[0].code.to_string())),
                ),
                field(
                    "access_id",
                    "Access id",
                    "The project's access id, also called client id.",
                    FieldType::Text { max_len: MAX_ACCESS_ID },
                    true,
                    None,
                ),
                field(
                    "access_secret",
                    "Access secret",
                    "The project's access secret. It signs the requests and is not stored.",
                    FieldType::Secret,
                    true,
                    None,
                ),
            ],
        }
    }

    fn cloud_choose(&self, skipped: usize) -> SetupStep {
        let options = self
            .found
            .iter()
            .map(|device| {
                let known = self.sockets.iter().any(|socket| socket.plug.id == device.id);
                let online = if device.online { "online" } else { "offline" };
                let short: String = device.id.chars().take(6).collect();
                Choice {
                    id: device.id.clone(),
                    label: if device.name.is_empty() { format!("Device {short}") } else { device.name.clone() },
                    detail: format!(
                        "{}, {online}, id {short}...",
                        if device.kind.is_empty() { "unknown type" } else { device.kind.as_str() }
                    ),
                    selected: known || device.is_socket(),
                }
            })
            .collect();
        let mut text = "Tick the sockets to add. Sockets and power strips are ticked already.".to_string();
        if skipped > 0 {
            text.push_str(&format!(" {skipped} other device(s) had no local key and are not listed."));
        }
        SetupStep::Choose { title: "Sockets in your project".to_string(), text, options, multiple: true, min: 1 }
    }

    fn places_form(&self, chosen: &[String]) -> SetupStep {
        let mut fields = Vec::new();
        for id in chosen {
            let Some(device) = self.found.iter().find(|device| &device.id == id) else { continue };
            let stored = self.sockets.iter().find(|socket| &socket.plug.id == id).map(|socket| &socket.plug);
            let label = if device.name.is_empty() { id.clone() } else { device.name.clone() };
            fields.push(field(
                &format!("address_{id}"),
                &format!("Address of {label}"),
                "Its IPv4 address on your network. Your router's list of devices shows it. \
                 A fixed address in the router keeps it from changing.",
                FieldType::Host,
                true,
                stored.map(|plug| Scalar::Text(plug.address.clone())),
            ));
            fields.push(port_field(&format!("port_{id}"), stored.map_or(DEFAULT_PORT, |plug| plug.port)));
            let name = stored.and_then(|plug| plug.name.clone()).or_else(|| Some(device.name.clone()));
            fields.push(field(
                &format!("name_{id}"),
                &format!("Name of {label}"),
                "How it is shown. Leave empty for \"Socket 1\", \"Socket 2\" and so on.",
                FieldType::Text { max_len: MAX_PLUG_NAME },
                false,
                name.filter(|name| is_plain_name(name)).map(Scalar::Text),
            ));
            fields.push(usb_field(&format!("usb_dp_{id}"), stored.and_then(|plug| plug.usb_dp)));
        }
        SetupStep::Form {
            title: "Where the sockets are".to_string(),
            text: "The sockets are switched on your network, not through the cloud, so Swifty Home needs each \
                   one's address."
                .to_string(),
            fields,
        }
    }

    fn manual_form(&self) -> SetupStep {
        let any_stored = self.sockets.iter().any(|socket| socket.key_stored);
        let key_help = if any_stored {
            "The socket's local key: 16 characters as Tuya shows it, or its base64 form. Leave empty to keep the \
             key already stored for a socket you are changing."
        } else {
            "The socket's local key: 16 characters as Tuya shows it, or its base64 form."
        };
        SetupStep::Form {
            title: "A socket".to_string(),
            text: "Enter one socket. Tick \"Add another\" to enter the next one after it.".to_string(),
            fields: vec![
                field(
                    "id",
                    "Device id",
                    "The Tuya device id, 10 to 32 letters and digits. The Tuya app shows it under the device's \
                     information.",
                    FieldType::Text { max_len: 32 },
                    true,
                    None,
                ),
                field("address", "Address", "Its IPv4 address on your network.", FieldType::Host, true, None),
                port_field("port", DEFAULT_PORT),
                field("local_key", "Local key", key_help, FieldType::Secret, !any_stored, None),
                field(
                    "name",
                    "Name",
                    "How it is shown. Leave empty for \"Socket 1\", \"Socket 2\" and so on.",
                    FieldType::Text { max_len: MAX_PLUG_NAME },
                    false,
                    None,
                ),
                usb_field("usb_dp", None),
                field(
                    "another",
                    "Add another",
                    "Enter one more socket after this one.",
                    FieldType::Bool,
                    false,
                    Some(Scalar::Bool(false)),
                ),
            ],
        }
    }

    fn sockets_choose(&self) -> SetupStep {
        let options = self
            .sockets
            .iter()
            .map(|socket| Choice {
                id: socket.plug.id.clone(),
                label: socket.plug.name.clone().unwrap_or_else(|| socket.plug.target()),
                detail: format!(
                    "{}:{}{}",
                    socket.plug.address,
                    socket.plug.port,
                    if socket.has_key() { "" } else { ", no local key yet" }
                ),
                selected: true,
            })
            .collect();
        SetupStep::Choose {
            title: "Sockets to keep".to_string(),
            text: "Untick a socket to leave it out.".to_string(),
            options,
            multiple: true,
            min: 1,
        }
    }

    /// Adds a socket, or replaces the one with the same id. A replacement keeps the
    /// stored key unless a new one was given.
    fn upsert(&mut self, plug: PlugConfig, new_key: Option<Secret>) {
        match self.sockets.iter_mut().find(|socket| socket.plug.id == plug.id) {
            Some(socket) => {
                socket.plug = plug;
                if new_key.is_some() {
                    socket.new_key = new_key;
                }
            }
            None => self.sockets.push(Socket { plug, new_key, key_stored: false }),
        }
    }

    fn manual_socket(&mut self, values: &BTreeMap<String, Scalar>) -> Result<bool, String> {
        let id = text(values, "id").unwrap_or_default().to_ascii_lowercase();
        if !is_plug_id(&id) {
            return Err("The device id must be 10 to 32 letters and digits.".to_string());
        }
        let address = text(values, "address").unwrap_or_default();
        check_address(&address).map_err(|why| format!("The address {why}."))?;
        let port = port_of(values, "port", DEFAULT_PORT)?;
        let name = name_of(values, "name")?;
        let usb_dp = usb_of(values, "usb_dp")?;
        let new_key = match key_text(values, "local_key") {
            Some(entered) => Some(
                stored_key(&entered)
                    .ok_or("The local key must be 16 characters as Tuya shows it, or their base64 form.")?,
            ),
            None => None,
        };
        let existing = self.sockets.iter().find(|socket| socket.plug.id == id);
        if new_key.is_none() && !existing.is_some_and(Socket::has_key) {
            return Err(format!("plug-{id} needs its local key."));
        }
        if existing.is_none() && self.sockets.len() >= MAX_PLUGS {
            return Err(format!("One instance holds at most {MAX_PLUGS} sockets."));
        }
        self.upsert(PlugConfig { id, address, port, name, usb_dp }, new_key.map(Into::into));
        Ok(matches!(values.get("another"), Some(Scalar::Bool(true))))
    }

    fn cloud_places(&mut self, chosen: &[String], values: &BTreeMap<String, Scalar>) -> Result<(), String> {
        let mut placed = Vec::new();
        for id in chosen {
            let device = self.found.iter().find(|device| &device.id == id).ok_or(DOES_NOT_FIT)?;
            let address = text(values, &format!("address_{id}")).unwrap_or_default();
            let label = if device.name.is_empty() { format!("plug-{id}") } else { device.name.clone() };
            check_address(&address).map_err(|why| format!("The address of {label} {why}."))?;
            let stored_port = self
                .sockets
                .iter()
                .find(|socket| &socket.plug.id == id)
                .map_or(DEFAULT_PORT, |socket| socket.plug.port);
            let port = port_of(values, &format!("port_{id}"), stored_port)?;
            let name = name_of(values, &format!("name_{id}"))?;
            let usb_dp = usb_of(values, &format!("usb_dp_{id}"))?;
            placed.push((PlugConfig { id: id.clone(), address, port, name, usb_dp }, device.key.clone()));
        }
        // A listed socket that was not ticked is left out; one the cloud does not list is kept.
        let listed: Vec<String> = self.found.iter().map(|device| device.id.clone()).collect();
        self.sockets.retain(|socket| !listed.contains(&socket.plug.id));
        for (plug, key) in placed {
            self.upsert(plug, Some(key));
        }
        Ok(())
    }

    /// Checks the result, reads each socket with a new key when the hub is live, and finishes.
    async fn finish(&mut self, ctx: &SetupContext, keep: &[String]) -> SetupStep {
        let chosen: Vec<&Socket> = self.sockets.iter().filter(|socket| keep.contains(&socket.plug.id)).collect();
        if chosen.is_empty() || chosen.len() != keep.len() {
            return failed(DOES_NOT_FIT);
        }
        if let Some(socket) = chosen.iter().find(|socket| !socket.has_key()) {
            return failed(&format!("{} has no local key. Enter it with \"I have the keys\".", socket.plug.target()));
        }
        let config = TuyaConfig { plugs: chosen.iter().map(|socket| socket.plug.clone()).collect() };
        if let Err(problem) = check(&config) {
            return failed(&format!("{}: {}", problem.field, problem.message));
        }
        let mut read = 0;
        if let Some(grant) = ctx.grant {
            for socket in chosen.iter().filter(|socket| socket.new_key.is_some()) {
                let target = socket.plug.target();
                let Some(cipher) = socket.new_key.as_ref().and_then(|key| cipher_from_stored(key.expose())) else {
                    return failed(&format!("The local key for {target} is not a key of 16 bytes."));
                };
                let one_at_a_time = Mutex::new(());
                let answer =
                    query(grant, ctx.timeouts, plug_address(&socket.plug), &one_at_a_time, &socket.plug.id, &cipher)
                        .await
                        .and_then(|json| is_on(&json, MAIN_SWITCH));
                if let Err(error) = answer {
                    return failed(&format!(
                        "{target} at {}:{} did not answer as expected: {error}. Check its address and key, and \
                         close the Tuya app, which can hold the socket's only connection.",
                        socket.plug.address, socket.plug.port
                    ));
                }
                read += 1;
            }
        }
        let secrets = chosen
            .iter()
            .filter_map(|socket| socket.new_key.clone().map(|key| (key_slot(&socket.plug.id), key)))
            .collect();
        let count = config.plugs.len();
        let mut summary = format!("Saved {count} socket{}.", if count == 1 { "" } else { "s" });
        if read > 0 {
            summary.push_str(&format!(" {read} answered."));
        }
        let config = serde_json::to_value(&config).unwrap_or(Value::Null);
        SetupStep::Done(SetupResult { config, secrets, adopt: Vec::new(), summary })
    }

    async fn look_up(&mut self, ctx: &SetupContext, values: &BTreeMap<String, Scalar>) -> SetupStep {
        let Some(grant) = ctx.grant else { return failed(LIVE_NEEDED) };
        let Some(region) = text(values, "region").and_then(|code| region(&code)) else {
            return failed("Choose one of the data centers.");
        };
        let access_id = text(values, "access_id").unwrap_or_default();
        if access_id.is_empty()
            || access_id.len() > MAX_ACCESS_ID
            || !access_id.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return failed("The access id is letters and digits, as the project's overview shows it.");
        }
        let Some(access_secret) = text(values, "access_secret") else {
            return failed("The access secret is needed.");
        };
        let base = self.cloud_base.clone().unwrap_or_else(|| format!("https://{}", region.host));
        let listing = match Cloud::sign_in(grant, &base, ctx.timeouts, &access_id, Secret::new(access_secret)).await {
            Ok(cloud) => cloud.devices().await,
            Err(reason) => Err(reason),
        };
        let listing = match listing {
            Ok(listing) => listing,
            Err(reason) => return failed(&reason),
        };
        if listing.usable.is_empty() {
            return failed(
                "The project lists no device with a local key. Check that your Tuya app account is linked to \
                 the cloud project and that the data center is right.",
            );
        }
        let skipped = listing.skipped;
        self.found = listing.usable;
        self.stage = Stage::CloudChoose;
        self.cloud_choose(skipped)
    }
}

#[async_trait]
impl SetupFlow for TuyaSetup {
    async fn begin(&mut self, ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        self.stage = Stage::Path;
        self.found.clear();
        self.sockets = existing
            .and_then(|config| crate::config::parse(config).ok())
            .map(|config| {
                config
                    .plugs
                    .into_iter()
                    .map(|plug| {
                        let key_stored = ctx.secrets_present.contains(&plug.key_slot());
                        Socket { plug, new_key: None, key_stored }
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.path_step()
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        if matches!(input, SetupInput::Back) {
            self.stage = Stage::Path;
            return self.path_step();
        }
        match (self.stage.clone(), input) {
            (Stage::Path, SetupInput::Chosen { ids }) => match ids.as_slice() {
                [id] if id == "cloud" => {
                    if ctx.grant.is_none() {
                        return failed(LIVE_NEEDED);
                    }
                    self.stage = Stage::CloudForm;
                    self.cloud_form()
                }
                [id] if id == "manual" => {
                    self.stage = Stage::ManualForm;
                    self.manual_form()
                }
                _ => failed(DOES_NOT_FIT),
            },
            (Stage::CloudForm, SetupInput::Values { values }) => self.look_up(ctx, &values).await,
            (Stage::CloudChoose, SetupInput::Chosen { ids }) => {
                if ids.is_empty() || !ids.iter().all(|id| self.found.iter().any(|device| &device.id == id)) {
                    return failed(DOES_NOT_FIT);
                }
                self.stage = Stage::CloudPlaces { chosen: ids.clone() };
                self.places_form(&ids)
            }
            (Stage::CloudPlaces { chosen }, SetupInput::Values { values }) => {
                if let Err(reason) = self.cloud_places(&chosen, &values) {
                    return failed(&reason);
                }
                let keep: Vec<String> = self.sockets.iter().map(|socket| socket.plug.id.clone()).collect();
                self.finish(ctx, &keep).await
            }
            (Stage::ManualForm, SetupInput::Values { values }) => match self.manual_socket(&values) {
                Ok(true) => self.manual_form(),
                Ok(false) => {
                    self.stage = Stage::ManualChoose;
                    self.sockets_choose()
                }
                Err(reason) => failed(&reason),
            },
            (Stage::ManualChoose, SetupInput::Chosen { ids }) => self.finish(ctx, &ids).await,
            _ => failed(DOES_NOT_FIT),
        }
    }
}

/// A failure the person can correct: the step before is offered again.
fn failed(reason: &str) -> SetupStep {
    SetupStep::Failed { reason: reason.to_string(), retry: true }
}

fn field(key: &str, label: &str, help: &str, kind: FieldType, required: bool, value: Option<Scalar>) -> Field {
    Field { key: key.to_string(), label: label.to_string(), help: help.to_string(), kind, required, value }
}

fn usb_field(key: &str, value: Option<u16>) -> Field {
    field(
        key,
        "USB data point",
        "Only for a socket with a USB port that switches on its own: the port's data point, usually 7. \
         Leave empty otherwise.",
        FieldType::Int { min: 2, max: 255 },
        false,
        value.map(|point| Scalar::Int(i64::from(point))),
    )
}

/// A text value, trimmed, when it is not empty.
fn text(values: &BTreeMap<String, Scalar>, key: &str) -> Option<String> {
    match values.get(key)? {
        Scalar::Text(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        Scalar::Int(number) => Some(number.to_string()),
        Scalar::Bool(_) => None,
    }
}

/// A key as it was typed or pasted: only a line end is dropped, because a key
/// can hold any printable character.
fn key_text(values: &BTreeMap<String, Scalar>, key: &str) -> Option<String> {
    match values.get(key)? {
        Scalar::Text(text) => Some(text.trim_matches(['\r', '\n']).to_string()).filter(|text| !text.is_empty()),
        _ => None,
    }
}

/// A whole number, given as a number or as text.
fn int(values: &BTreeMap<String, Scalar>, key: &str) -> Option<i64> {
    match values.get(key)? {
        Scalar::Int(number) => Some(*number),
        Scalar::Text(text) => text.trim().parse().ok(),
        Scalar::Bool(_) => None,
    }
}

fn name_of(values: &BTreeMap<String, Scalar>, key: &str) -> Result<Option<String>, String> {
    match text(values, key) {
        None => Ok(None),
        Some(name) if is_plain_name(&name) => Ok(Some(name)),
        Some(_) => Err(format!("A name is 1 to {MAX_PLUG_NAME} plain characters.")),
    }
}

fn port_of(values: &BTreeMap<String, Scalar>, key: &str, otherwise: u16) -> Result<u16, String> {
    match (int(values, key), text(values, key)) {
        (Some(port), _) if (1..=65_535).contains(&port) => Ok(port as u16),
        (None, None) => Ok(otherwise),
        _ => Err("The port must be 1 to 65535.".to_string()),
    }
}

fn port_field(key: &str, value: u16) -> Field {
    field(
        key,
        "Port",
        "The local protocol's port. Leave it at 6668 unless you know better.",
        FieldType::Port,
        false,
        Some(Scalar::Int(i64::from(value))),
    )
}

fn usb_of(values: &BTreeMap<String, Scalar>, key: &str) -> Result<Option<u16>, String> {
    match (int(values, key), text(values, key)) {
        (Some(point), _) if (2..=255).contains(&point) => Ok(Some(point as u16)),
        (None, None) => Ok(None),
        _ => Err("The USB data point must be 2 to 255.".to_string()),
    }
}

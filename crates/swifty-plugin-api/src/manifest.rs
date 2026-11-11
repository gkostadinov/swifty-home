//! What a plugin says about itself before it runs, and how its config is described.

use serde::Serialize;

use crate::{DeviceClass, Examples, Expect, ParamDef, Tier};

/// The version of this contract. A plugin built for another version is not loaded.
pub const API_VERSION: u32 = 1;

/// Everything the hub and a person can know about a plugin before it runs.
/// A `static` in the plugin crate. Rendered by the app, `swifty plugin info`
/// and the generated reference, and checked by the conformance suite.
#[derive(Debug, Clone, Serialize)]
pub struct Manifest {
    /// `[a-z][a-z0-9-]{1,31}`. Also the default instance id and the prefix of custom action ids.
    pub id: &'static str,
    /// The name people see.
    pub name: &'static str,
    /// The crate version: `env!("CARGO_PKG_VERSION")`.
    pub version: &'static str,
    /// The plugin API it was written for. Must equal `API_VERSION`.
    pub api: u32,
    /// One sentence on what it connects to.
    pub summary: &'static str,
    /// Whether it stays on the local network or goes through a vendor's servers.
    pub reach: Reach,
    /// Everything it opens a socket to. Shown before setup. Declared, not enforced.
    pub network: &'static [NetUse],
    /// The classes of the devices it offers.
    pub classes: &'static [DeviceClass],
    /// Bumped when the stored config changes shape. See `Plugin::migrate`.
    pub config_version: u32,
    /// The settings of an instance.
    pub config: &'static [ConfigField],
    /// The credentials an instance stores.
    pub secrets: &'static [SecretSlot],
    /// The actions it adds to the catalog.
    pub actions: &'static [CustomAction],
    /// What the recogniser mishears, as words: ("whiz", "wiz"). Words only, never grammar.
    pub vocabulary: &'static [(&'static str, &'static str)],
    /// Brand words that name a class: ("sonos", DeviceClass::Speaker).
    pub nouns: &'static [(&'static str, DeviceClass)],
    /// Whether a second instance makes sense (two bridges, two clusters).
    pub multi_instance: bool,
}

/// Where a plugin's traffic goes.
#[derive(Debug, Clone, Copy, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "reach", rename_all = "snake_case")]
pub enum Reach {
    /// Talks only to devices on the local network.
    Local,
    /// Goes through a vendor's servers. `official` is false when the vendor
    /// does not document the API for third parties. The app says so.
    Cloud {
        /// The vendor's name.
        vendor: &'static str,
        /// Whether the vendor documents the API for third parties.
        official: bool,
    },
}

/// One kind of connection a plugin opens.
#[derive(Debug, Clone, Copy, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "use", rename_all = "snake_case")]
pub enum NetUse {
    /// Plain HTTP to a device.
    Http {
        /// The port.
        port: u16,
    },
    /// HTTPS to a device.
    Https {
        /// The port.
        port: u16,
    },
    /// A websocket to a device.
    Ws {
        /// The port.
        port: u16,
    },
    /// A TCP connection to a device.
    Tcp {
        /// The port.
        port: u16,
    },
    /// UDP datagrams to a device.
    Udp {
        /// The port.
        port: u16,
    },
    /// UDP broadcasts on the local network.
    UdpBroadcast {
        /// The port.
        port: u16,
    },
    /// Wake-on-LAN packets.
    WakeOnLan,
    /// HTTPS to a vendor's server.
    CloudHost {
        /// The server's host name.
        host: &'static str,
    },
}

/// One setting of an instance, as a form field and a line of documentation.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ConfigField {
    /// The key in the instance config.
    pub key: &'static str,
    /// The field's label.
    pub label: &'static str,
    /// One sentence for the person filling it in.
    pub help: &'static str,
    /// What it may hold.
    pub kind: FieldKind,
    /// Whether it must be given.
    pub required: bool,
    /// The value it has when it is not given, as text.
    pub default: Option<&'static str>,
}

/// What a config field may hold.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    /// Text.
    Text {
        /// The most characters it may have.
        max_len: usize,
    },
    /// IP address or host name.
    Host,
    /// A URL.
    Url {
        /// The schemes it may have.
        schemes: &'static [&'static str],
    },
    /// A host and a port.
    HostPort,
    /// A port.
    Port,
    /// A whole number in a range.
    Int {
        /// The lowest value.
        min: i64,
        /// The highest value.
        max: i64,
    },
    /// `true` or `false`.
    Bool,
    /// One of these words.
    Choice {
        /// The words it may be.
        options: &'static [&'static str],
    },
    /// A MAC address.
    Mac,
    /// A list of rows, each with these fields (Tuya sockets, Toshiba units, WiZ bulbs).
    Rows {
        /// The fields of each row.
        fields: &'static [ConfigField],
    },
}

/// A credential this plugin stores. The value never appears in config.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SecretSlot {
    /// "app_key"; for rows: "local_key/{id}".
    pub slot: &'static str,
    /// The field's label.
    pub label: &'static str,
    /// One sentence on where to find it.
    pub help: &'static str,
    /// Whether the instance cannot work without it.
    pub required: bool,
}

/// An action a plugin adds to the catalog, beyond the standard ones.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CustomAction {
    /// `"<plugin id>.<verb>"`.
    pub id: &'static str,
    /// A short name for it.
    pub label: &'static str,
    /// What will happen.
    pub description: &'static str,
    /// What follows from it.
    pub consequences: &'static [&'static str],
    /// The class of device it acts on.
    pub class: DeviceClass,
    /// A request. The hub applies the floor.
    pub tier: Tier,
    /// Its parameters.
    pub params: &'static [ParamDef],
    /// False means a repeat would do the thing twice, so it is never retried.
    pub idempotent: bool,
    /// How long the hub waits for the device to show the change.
    pub confirm_timeout_ms: u64,
    /// How the hub tells it worked.
    pub expect: Expect,
}

impl Examples for Reach {
    fn examples() -> Vec<Self> {
        vec![Reach::Local, Reach::Cloud { vendor: "Acme", official: false }]
    }
}

impl Examples for NetUse {
    fn examples() -> Vec<Self> {
        vec![
            NetUse::Http { port: 80 },
            NetUse::Https { port: 443 },
            NetUse::Ws { port: 3000 },
            NetUse::Tcp { port: 6668 },
            NetUse::Udp { port: 38899 },
            NetUse::UdpBroadcast { port: 38899 },
            NetUse::WakeOnLan,
            NetUse::CloudHost { host: "api.example.org" },
        ]
    }
}

/// The fields of one row in the [`FieldKind::Rows`] sample.
const SAMPLE_ROW: &[ConfigField] = &[
    ConfigField {
        key: "id",
        label: "Id",
        help: "The device's id: lower-case letters, digits, - and _.",
        kind: FieldKind::Text { max_len: 64 },
        required: true,
        default: None,
    },
    ConfigField {
        key: "address",
        label: "Address",
        help: "Its IP address, such as 192.0.2.20.",
        kind: FieldKind::Host,
        required: true,
        default: None,
    },
];

impl Examples for FieldKind {
    fn examples() -> Vec<Self> {
        vec![
            FieldKind::Text { max_len: 64 },
            FieldKind::Host,
            FieldKind::Url { schemes: &["http", "https"] },
            FieldKind::HostPort,
            FieldKind::Port,
            FieldKind::Int { min: 1, max: 3600 },
            FieldKind::Bool,
            FieldKind::Choice { options: &["eu", "us"] },
            FieldKind::Mac,
            FieldKind::Rows { fields: SAMPLE_ROW },
        ]
    }
}

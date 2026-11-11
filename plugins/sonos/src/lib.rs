//! Sonos speakers, over UPnP SOAP on port 1400 on the local network.
//!
//! Each speaker is a row of the instance config. A row marked `read_only` (a
//! subwoofer, which has no AVTransport service) is only ever read: it offers no
//! capability, so no catalog action admits it, and the driver refuses any
//! command for it before anything is sent.

#![forbid(unsafe_code)]

mod config;
mod driver;
mod protocol;
mod setup;

use std::sync::Arc;

use serde_json::json;
use swifty_plugin_api::{
    Capability, ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext,
    Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, SetupFlow, API_VERSION,
};

pub use config::{SonosConfig, SpeakerConfig};
pub use driver::SonosDriver;
pub use setup::SonosSetup;

/// The port every Sonos speaker answers UPnP on.
pub const PORT: u16 = 1400;

pub(crate) const READ_ONLY: &str = "This speaker is read-only here.";
pub(crate) const CANNOT: &str = "This speaker cannot do that.";

pub(crate) fn not_a_speaker(device: &str) -> String {
    format!("{device} is not a speaker of this instance.")
}

const SPEAKER_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "id",
        label: "Id",
        help: "A short id for the speaker, such as sonos or sub. It never changes once the speaker is added.",
        kind: FieldKind::Text { max_len: 64 },
        required: true,
        default: None,
    },
    ConfigField {
        key: "url",
        label: "Address",
        help: "http://<IP address>:1400. Use the IP address: a Sonos speaker refuses a request addressed by name.",
        kind: FieldKind::Url { schemes: &["http"] },
        required: true,
        default: None,
    },
    ConfigField {
        key: "read_only",
        label: "Read only",
        help: "Only read this speaker and never command it. For a subwoofer, which has no playback of its own.",
        kind: FieldKind::Bool,
        required: false,
        default: Some("false"),
    },
];

const FIELDS: &[ConfigField] = &[ConfigField {
    key: "speakers",
    label: "Speakers",
    help: "One row per speaker.",
    kind: FieldKind::Rows { fields: SPEAKER_FIELDS },
    required: true,
    default: None,
}];

/// What this plugin is, before it runs.
pub static MANIFEST: Manifest = Manifest {
    id: "sonos",
    name: "Sonos",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "Sonos speakers on the local network: volume, mute, night mode, play and pause. \
              A subwoofer can be added read-only.",
    reach: Reach::Local,
    network: &[NetUse::Http { port: PORT }],
    classes: &[DeviceClass::Speaker],
    config_version: 1,
    config: FIELDS,
    secrets: &[],
    actions: &[],
    vocabulary: &[],
    nouns: &[("sonos", DeviceClass::Speaker), ("arc", DeviceClass::Speaker)],
    multi_instance: false,
};

/// The plugin.
pub struct SonosPlugin;

/// The plugin, as the hub's compiled-in list names it.
pub static PLUGIN: SonosPlugin = SonosPlugin;

/// What a speaker that may be commanded can do.
fn capabilities(read_only: bool) -> Vec<Capability> {
    if read_only {
        Vec::new()
    } else {
        vec![Capability::Volume, Capability::Mute, Capability::NightMode, Capability::Transport]
    }
}

/// Why this cannot go to this speaker, if it cannot. Pure.
pub fn refusal(config: &SonosConfig, device: &str, op: &Op) -> Option<String> {
    let Some(speaker) = config.speaker(device) else { return Some(not_a_speaker(device)) };
    if speaker.read_only {
        return Some(READ_ONLY.to_string());
    }
    match op {
        Op::Volume { .. } | Op::VolumeCap { .. } | Op::Mute { .. } | Op::NightMode { .. } | Op::Play | Op::Pause => {
            None
        }
        _ => Some(CANNOT.to_string()),
    }
}

impl Plugin for SonosPlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &serde_json::Value) -> Result<(), ConfigProblem> {
        SonosConfig::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        match SonosConfig::parse(instance.config) {
            Ok(_) => Readiness::Ready,
            Err(problem) => Readiness::Disabled(problem.to_string()),
        }
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        let Ok(config) = SonosConfig::parse(instance.config) else { return Vec::new() };
        config
            .speakers
            .iter()
            .map(|speaker| DeviceDecl {
                local_id: speaker.id.clone(),
                target_hint: Some(speaker.id.clone()),
                class: DeviceClass::Speaker,
                name: if speaker.read_only { "Sonos subwoofer" } else { "Sonos speaker" }.to_string(),
                capabilities: capabilities(speaker.read_only),
                attrs: json!({ "url": speaker.url, "read_only": speaker.read_only }),
            })
            .collect()
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        match SonosConfig::parse(instance.config) {
            Ok(config) => refusal(&config, device, op),
            Err(problem) => Some(problem.to_string()),
        }
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = SonosConfig::parse(&ctx.config).map_err(|problem| PluginError(problem.to_string()))?;
        let driver = SonosDriver::new(ctx.grant, &config, ctx.timeouts).map_err(PluginError)?;
        Ok(Arc::new(driver))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(SonosSetup::default()))
    }
}

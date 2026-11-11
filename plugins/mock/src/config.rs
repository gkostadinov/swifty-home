//! The instance config: a list of devices, each with a class and, if it should
//! differ from the class's usual set, its capabilities.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::{CapKind, Capability, ConfigProblem, DeviceClass, MIN_LIGHT_DIMMING};

use crate::{CUSTOM_ACTION, MANIFEST};

/// The inputs a mock TV offers unless its row says otherwise.
pub const TV_INPUTS: &[&str] = &["HDMI 1", "HDMI 2", "HDMI 3", "HDMI 4", "Live TV"];
/// The remote keys a mock TV offers unless its row says otherwise.
pub const TV_KEYS: &[&str] = &[
    "up",
    "down",
    "left",
    "right",
    "ok",
    "back",
    "home",
    "exit",
    "volume_up",
    "volume_down",
    "mute",
    "channel_up",
    "channel_down",
    "play",
    "pause",
];
/// The modes a mock air conditioner offers unless its row says otherwise.
pub const AC_MODES: &[&str] = &["auto", "cool", "heat", "dry", "fan"];
/// The fan speeds a mock air conditioner offers unless its row says otherwise.
pub const AC_FANS: &[&str] = &["auto", "quiet", "low", "medium_low", "medium", "medium_high", "high"];
/// The target temperatures a mock air conditioner accepts unless its row says otherwise.
pub const AC_TEMPERATURE: (u8, u8) = (17, 30);
/// The portions a mock feeder dispenses unless its row says otherwise, in grams.
pub const FEED_GRAMS: &[u8] = &[10, 20, 30, 40, 50];

/// The most devices one instance may declare.
pub const MAX_DEVICES: usize = 256;
const MAX_NAME: usize = 64;
const MAX_REASON: usize = 200;
const MAX_OPTION: usize = 32;
const MAX_OPTIONS: usize = 64;

/// One instance of the mock plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MockConfig {
    /// The devices this instance offers. May be empty.
    pub devices: Vec<DeviceRow>,
    /// When set, the instance is disabled with this sentence as the reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<String>,
    /// When true, the instance needs a value in the `access_code` slot before it is ready.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_code: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One device of an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRow {
    /// The local id: `[a-z0-9][a-z0-9_-]{0,63}`.
    pub id: String,
    /// What kind of thing it is.
    pub class: DeviceClass,
    /// Its name. Left out, a generic name for its class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The target the hub is asked to give it. Left out, the hub picks one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// What it can do. Left out, the usual set for its class; an empty list
    /// makes it read-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<CapabilityRow>>,
}

impl DeviceRow {
    /// A row with the class's name and capabilities.
    pub fn new(id: &str, class: DeviceClass) -> Self {
        Self { id: id.to_string(), class, name: None, target: None, capabilities: None }
    }

    /// The same row with this name.
    pub fn named(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// The same row, asking the hub for its own id as the target.
    pub fn hinted(mut self) -> Self {
        self.target = Some(self.id.clone());
        self
    }

    /// The same row with exactly these capabilities.
    pub fn with(mut self, capabilities: &[Capability]) -> Self {
        self.capabilities = Some(capabilities.iter().map(CapabilityRow::from).collect());
        self
    }

    /// The name it is shown with.
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| default_name(self.class).to_string())
    }

    /// What it can do. Assumes the row passed [`check`].
    pub fn capability_list(&self) -> Vec<Capability> {
        match &self.capabilities {
            None => default_capabilities(self.class),
            Some(rows) => rows.iter().filter_map(|row| row.to_capability().ok()).collect(),
        }
    }
}

/// One capability, in a form a person can fill in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRow {
    /// Which capability.
    pub cap: CapKind,
    /// The lowest brightness in percent, or the lowest target temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<u8>,
    /// The highest target temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u8>,
    /// The accepted values, separated by commas: inputs, keys, modes, fan
    /// speeds, or portions in grams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<String>,
    /// The custom action, for `custom`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl CapabilityRow {
    fn bare(cap: CapKind) -> Self {
        Self { cap, min: None, max: None, options: None, action: None }
    }

    /// The capability this row describes, or what is wrong with it.
    pub fn to_capability(&self) -> Result<Capability, String> {
        let bare = |capability: Capability| {
            if self.min.is_some() || self.max.is_some() || self.options.is_some() || self.action.is_some() {
                Err("this capability takes no min, max, options or action".to_string())
            } else {
                Ok(capability)
            }
        };
        let words = || -> Result<Vec<String>, String> {
            if self.min.is_some() || self.max.is_some() || self.action.is_some() {
                return Err("this capability takes only options".to_string());
            }
            split_options(self.options.as_deref())
        };
        match self.cap {
            CapKind::Power => bare(Capability::Power),
            CapKind::Color => bare(Capability::Color),
            CapKind::Volume => bare(Capability::Volume),
            CapKind::Mute => bare(Capability::Mute),
            CapKind::NightMode => bare(Capability::NightMode),
            CapKind::Transport => bare(Capability::Transport),
            CapKind::Clean => bare(Capability::Clean),
            CapKind::PrintJob => bare(Capability::PrintJob),
            CapKind::EmergencyStop => bare(Capability::EmergencyStop),
            CapKind::Lifecycle => bare(Capability::Lifecycle),
            CapKind::NodePower => bare(Capability::NodePower),
            CapKind::Tunnel => bare(Capability::Tunnel),
            CapKind::Metrics => bare(Capability::Metrics),
            CapKind::History => bare(Capability::History),
            CapKind::Console => bare(Capability::Console),
            CapKind::AgentPairing => bare(Capability::AgentPairing),
            CapKind::Brightness => {
                if self.max.is_some() || self.options.is_some() || self.action.is_some() {
                    return Err("brightness takes only min".to_string());
                }
                let min = self.min.unwrap_or(MIN_LIGHT_DIMMING);
                if !(1..=100).contains(&min) {
                    return Err("min must be from 1 to 100".to_string());
                }
                Ok(Capability::Brightness { min })
            }
            CapKind::Temperature => {
                if self.options.is_some() || self.action.is_some() {
                    return Err("temperature takes only min and max".to_string());
                }
                let (min, max) = (self.min.unwrap_or(AC_TEMPERATURE.0), self.max.unwrap_or(AC_TEMPERATURE.1));
                if min > max {
                    return Err("min is above max".to_string());
                }
                Ok(Capability::Temperature { min, max })
            }
            CapKind::Inputs => Ok(Capability::Inputs { inputs: words()? }),
            CapKind::Keys => Ok(Capability::Keys { keys: words()? }),
            CapKind::Mode => Ok(Capability::Mode { modes: words()? }),
            CapKind::Fan => Ok(Capability::Fan { speeds: words()? }),
            CapKind::Dispense => {
                let grams = words()?
                    .iter()
                    .map(|word| word.parse::<u8>().ok().filter(|grams| *grams > 0))
                    .collect::<Option<Vec<u8>>>()
                    .ok_or_else(|| "each portion must be a whole number of grams from 1 to 255".to_string())?;
                Ok(Capability::Dispense { grams })
            }
            CapKind::Custom => {
                if self.min.is_some() || self.max.is_some() || self.options.is_some() {
                    return Err("custom takes only action".to_string());
                }
                let action = self.action.as_deref().ok_or_else(|| "custom needs an action".to_string())?;
                if !MANIFEST.actions.iter().any(|declared| declared.id == action) {
                    return Err(format!("the mock plugin declares no action {action}"));
                }
                Ok(Capability::Custom { action: action.to_string() })
            }
            _ => Err("the mock plugin does not know this capability".to_string()),
        }
    }
}

impl From<&Capability> for CapabilityRow {
    fn from(capability: &Capability) -> Self {
        let joined = |words: &[String]| Some(words.join(", "));
        let mut row = Self::bare(capability.kind());
        match capability {
            Capability::Brightness { min } => row.min = Some(*min),
            Capability::Temperature { min, max } => (row.min, row.max) = (Some(*min), Some(*max)),
            Capability::Inputs { inputs } => row.options = joined(inputs),
            Capability::Keys { keys } => row.options = joined(keys),
            Capability::Mode { modes } => row.options = joined(modes),
            Capability::Fan { speeds } => row.options = joined(speeds),
            Capability::Dispense { grams } => {
                row.options = Some(grams.iter().map(u8::to_string).collect::<Vec<_>>().join(", "))
            }
            Capability::Custom { action } => row.action = Some(action.clone()),
            _ => {}
        }
        row
    }
}

fn split_options(options: Option<&str>) -> Result<Vec<String>, String> {
    let options = options.ok_or_else(|| "this capability needs options".to_string())?;
    let words: Vec<String> = options.split(',').map(|word| word.trim().to_string()).collect();
    if words.iter().any(|word| word.is_empty() || word.chars().count() > MAX_OPTION || has_control(word)) {
        return Err(format!("each option must be 1 to {MAX_OPTION} characters, separated by commas"));
    }
    if words.len() > MAX_OPTIONS {
        return Err(format!("at most {MAX_OPTIONS} options"));
    }
    if words.iter().collect::<BTreeSet<_>>().len() != words.len() {
        return Err("an option is listed twice".to_string());
    }
    Ok(words)
}

/// A generic name for a device of this class.
pub fn default_name(class: DeviceClass) -> &'static str {
    use DeviceClass::*;
    match class {
        Light => "Mock light",
        Bridge => "Mock bridge",
        Plug => "Mock plug",
        Aircon => "Mock air conditioner",
        Speaker => "Mock speaker",
        Tv => "Mock TV",
        Feeder => "Mock feeder",
        Litter => "Mock litter box",
        Printer => "Mock printer",
        Vm => "Mock VM",
        Host => "Mock host",
        Router => "Mock router",
        Camera => "Mock camera",
        Service => "Mock service",
        Telemetry => "Mock sensor",
        _ => "Mock gadget",
    }
}

fn owned(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

/// The usual capabilities of a device of this class: the catalog's whole range.
pub fn default_capabilities(class: DeviceClass) -> Vec<Capability> {
    use Capability as C;
    use DeviceClass::*;
    match class {
        Light => vec![C::Power, C::Brightness { min: MIN_LIGHT_DIMMING }, C::Color],
        Plug => vec![C::Power],
        Aircon => vec![
            C::Power,
            C::Mode { modes: owned(AC_MODES) },
            C::Temperature { min: AC_TEMPERATURE.0, max: AC_TEMPERATURE.1 },
            C::Fan { speeds: owned(AC_FANS) },
        ],
        Speaker => vec![C::Volume, C::Mute, C::NightMode, C::Transport],
        Tv => vec![C::Power, C::Inputs { inputs: owned(TV_INPUTS) }, C::Keys { keys: owned(TV_KEYS) }],
        Feeder => vec![C::Dispense { grams: FEED_GRAMS.to_vec() }],
        Litter => vec![C::Clean],
        Printer => vec![C::PrintJob, C::EmergencyStop],
        Vm => vec![C::Lifecycle, C::Console, C::History],
        Host => vec![C::NodePower, C::History],
        Router => vec![C::Tunnel],
        Telemetry => vec![C::Metrics],
        Bridge | Camera | Service => Vec::new(),
        _ => vec![C::Custom { action: CUSTOM_ACTION.to_string() }],
    }
}

/// `[a-z0-9][a-z0-9_-]{0,63}`: a local id, and the shape of a target.
pub fn is_local_id(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && text.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn has_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

fn problem(field: impl Into<String>, message: impl Into<String>) -> ConfigProblem {
    ConfigProblem { field: field.into(), message: message.into() }
}

/// The name in the first pair of backticks of a serde message, such as
/// "missing field `id`".
fn named_field(message: &str) -> Option<&str> {
    let (_, rest) = message.split_once('`')?;
    rest.split_once('`').map(|(name, _)| name)
}

/// Parses a config document and checks it. Pure.
pub fn parse(config: &Value) -> Result<MockConfig, ConfigProblem> {
    let parsed: MockConfig = serde_json::from_value(config.clone()).map_err(|error| {
        let message = error.to_string();
        let field = match named_field(&message) {
            Some(name) if message.contains("field") => name.to_string(),
            _ => "config".to_string(),
        };
        problem(field, message)
    })?;
    check(&parsed)?;
    Ok(parsed)
}

/// The rules a parsed config must meet beyond its shape.
pub fn check(config: &MockConfig) -> Result<(), ConfigProblem> {
    if config.devices.len() > MAX_DEVICES {
        return Err(problem("devices", format!("at most {MAX_DEVICES} devices")));
    }
    let mut ids = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for (n, row) in config.devices.iter().enumerate() {
        let at = |key: &str| format!("devices[{n}].{key}");
        if !is_local_id(&row.id) {
            return Err(problem(at("id"), "must match [a-z0-9][a-z0-9_-]{0,63}"));
        }
        if !ids.insert(row.id.as_str()) {
            return Err(problem(at("id"), format!("{} is listed twice", row.id)));
        }
        if let Some(name) = &row.name {
            if name.trim().is_empty() || name.chars().count() > MAX_NAME || has_control(name) {
                return Err(problem(at("name"), format!("must be 1 to {MAX_NAME} printable characters")));
            }
        }
        if let Some(target) = &row.target {
            if !is_local_id(target) {
                return Err(problem(at("target"), "must match [a-z0-9][a-z0-9_-]{0,63}"));
            }
            if !targets.insert(target.as_str()) {
                return Err(problem(at("target"), format!("{target} is asked for twice")));
            }
        }
        let mut kinds = HashSet::new();
        for (m, capability) in row.capabilities.iter().flatten().enumerate() {
            let field = format!("devices[{n}].capabilities[{m}]");
            capability.to_capability().map_err(|message| problem(field.clone(), message))?;
            if !kinds.insert(capability.cap) {
                return Err(problem(field, "this capability is listed twice"));
            }
        }
    }
    if let Some(reason) = &config.disabled {
        if reason.trim().is_empty() || reason.chars().count() > MAX_REASON || has_control(reason) {
            return Err(problem("disabled", format!("must be a sentence of 1 to {MAX_REASON} printable characters")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_device_row_needs_only_an_id_and_a_class() {
        let config = parse(&json!({ "devices": [{ "id": "lamp", "class": "light" }] })).unwrap();
        let row = &config.devices[0];
        assert_eq!(row.display_name(), "Mock light");
        assert_eq!(row.capability_list(), default_capabilities(DeviceClass::Light));
        assert!(config.disabled.is_none() && !config.require_code);
    }

    #[test]
    fn capabilities_can_be_narrowed_or_emptied() {
        let config = parse(&json!({ "devices": [
            { "id": "tv", "class": "tv", "capabilities": [
                { "cap": "power" },
                { "cap": "inputs", "options": "HDMI 1, HDMI 2" },
            ]},
            { "id": "sub", "class": "speaker", "capabilities": [] },
            { "id": "dim", "class": "light", "capabilities": [{ "cap": "brightness", "min": 25 }] },
            { "id": "ac", "class": "aircon", "capabilities": [{ "cap": "temperature", "min": 18, "max": 26 }] },
            { "id": "bowl", "class": "feeder", "capabilities": [{ "cap": "dispense", "options": "5, 15" }] },
            { "id": "gizmo", "class": "other", "capabilities": [{ "cap": "custom", "action": "mock.set_level" }] },
        ]}))
        .unwrap();
        let lists: Vec<Vec<Capability>> = config.devices.iter().map(DeviceRow::capability_list).collect();
        assert_eq!(lists[0], vec![Capability::Power, Capability::Inputs { inputs: owned(&["HDMI 1", "HDMI 2"]) }]);
        assert!(lists[1].is_empty());
        assert_eq!(lists[2], vec![Capability::Brightness { min: 25 }]);
        assert_eq!(lists[3], vec![Capability::Temperature { min: 18, max: 26 }]);
        assert_eq!(lists[4], vec![Capability::Dispense { grams: vec![5, 15] }]);
        assert_eq!(lists[5], vec![Capability::Custom { action: "mock.set_level".to_string() }]);
    }

    #[test]
    fn every_default_capability_survives_the_row_form() {
        for class in crate::CLASSES {
            for capability in default_capabilities(*class) {
                let row = CapabilityRow::from(&capability);
                assert_eq!(row.to_capability(), Ok(capability.clone()), "{class:?}");
                let json = serde_json::to_value(&row).unwrap();
                assert_eq!(serde_json::from_value::<CapabilityRow>(json).unwrap(), row);
            }
        }
    }

    #[test]
    fn bad_configs_are_refused_with_the_field_named() {
        let cases = [
            (json!({}), "devices"),
            (json!({ "devices": [], "colour": "red" }), "colour"),
            (json!({ "devices": [{ "class": "light" }] }), "id"),
            (json!({ "devices": [{ "id": "lamp" }] }), "class"),
            (json!({ "devices": [{ "id": "lamp", "class": "light", "size": 3 }] }), "size"),
            (json!({ "devices": [{ "id": "Lamp", "class": "light" }] }), "devices[0].id"),
            (json!({ "devices": [{ "id": "a", "class": "light" }, { "id": "a", "class": "plug" }] }), "devices[1].id"),
            (json!({ "devices": [{ "id": "a", "class": "light", "name": "" }] }), "devices[0].name"),
            (json!({ "devices": [{ "id": "a", "class": "light", "name": "a\nb" }] }), "devices[0].name"),
            (json!({ "devices": [{ "id": "a", "class": "light", "target": "A B" }] }), "devices[0].target"),
            (
                json!({ "devices": [
                    { "id": "a", "class": "light", "target": "t" },
                    { "id": "b", "class": "light", "target": "t" },
                ]}),
                "devices[1].target",
            ),
            (
                json!({ "devices": [{ "id": "a", "class": "light", "capabilities": [{ "cap": "power", "min": 3 }] }] }),
                "devices[0].capabilities[0]",
            ),
            (
                json!({ "devices": [{ "id": "a", "class": "light", "capabilities": [{ "cap": "power" }, { "cap": "power" }] }] }),
                "devices[0].capabilities[1]",
            ),
            (
                json!({ "devices": [{ "id": "a", "class": "tv", "capabilities": [{ "cap": "inputs" }] }] }),
                "devices[0].capabilities[0]",
            ),
            (
                json!({ "devices": [{ "id": "a", "class": "tv", "capabilities": [{ "cap": "inputs", "options": "x,,y" }] }] }),
                "devices[0].capabilities[0]",
            ),
            (
                json!({ "devices": [{ "id": "a", "class": "tv", "capabilities": [{ "cap": "keys", "options": "x, x" }] }] }),
                "devices[0].capabilities[0]",
            ),
            (
                json!({ "devices": [{ "id": "a", "class": "aircon", "capabilities": [{ "cap": "temperature", "min": 30, "max": 20 }] }] }),
                "devices[0].capabilities[0]",
            ),
            (
                json!({ "devices": [{ "id": "a", "class": "light", "capabilities": [{ "cap": "brightness", "min": 0 }] }] }),
                "devices[0].capabilities[0]",
            ),
            (
                json!({ "devices": [{ "id": "a", "class": "feeder", "capabilities": [{ "cap": "dispense", "options": "10, lots" }] }] }),
                "devices[0].capabilities[0]",
            ),
            (
                json!({ "devices": [{ "id": "a", "class": "other", "capabilities": [{ "cap": "custom", "action": "mock.explode" }] }] }),
                "devices[0].capabilities[0]",
            ),
            (json!({ "devices": [], "disabled": " " }), "disabled"),
            (json!({ "devices": [], "require_code": "yes" }), "config"),
        ];
        for (config, field) in cases {
            let refused = parse(&config).expect_err(&config.to_string());
            assert_eq!(refused.field, field, "{config}: {refused}");
        }
    }

    #[test]
    fn too_many_devices_are_refused() {
        let devices: Vec<Value> =
            (0..=MAX_DEVICES).map(|n| json!({ "id": format!("d{n}"), "class": "plug" })).collect();
        assert_eq!(parse(&json!({ "devices": devices })).unwrap_err().field, "devices");
    }

    #[test]
    fn local_ids_have_one_grammar() {
        for good in ["a", "bulb1", "plug-bf01", "vm_1", "0"] {
            assert!(is_local_id(good), "{good}");
        }
        for bad in ["", "A", "-a", "_a", "a b", "a.b", "a/b", &"a".repeat(65)] {
            assert!(!is_local_id(bad), "{bad}");
        }
    }
}

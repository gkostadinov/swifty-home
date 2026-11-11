//! A stand-in device on loopback that speaks the template's protocol, and the
//! driver and setup context built against it.
//!
//! It keeps one set of fields per device, stores what it is sent, and answers a
//! read with the fields. When you change `protocol.rs`, change `answer` here to
//! match, the way your real device behaves.
//!
//! It is rigged for the conformance suite: while the suite says so, the stand-in
//! spoils its replies (check 12) and reports a long hostile name in any text it
//! sends (check 10). That is what lets `conformance.rs` run every check.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};
use swifty_plugin_api::net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    DeviceRecord, Directory, Driver, InstanceView, LiveContext, Op, Plugin, Scalar, Secrets, SetupContext, SetupInput,
};
use swifty_plugin_testkit::conformance::Switches;
{{#local}}
use swifty_plugin_testkit::devices::MockUdp;
{{/local}}
{{#cloud}}
use swifty_plugin_testkit::devices::MockHttp;
{{/cloud}}

use {{lib}}::PLUGIN;

{{#cloud}}
/// The access token the stand-in service accepts. Not a real one.
pub const TOKEN: &str = "test-token";

{{/cloud}}
/// Every device of the stand-in, with its class word, one per class.
pub const DEVICES: &[(&str, &str)] = &[
    // device, class
    {{device_table}}
];

/// Each device's fields.
pub type Devices = BTreeMap<String, Map<String, Value>>;
/// Changes that reached the stand-in: the device and the fields.
pub type Changes = Vec<(String, Map<String, Value>)>;

/// The stand-in's fields at the start, by device.
fn initial() -> Devices {
    let devices = [
        // device, its fields
        {{initial_states}}
    ];
    devices.into_iter().map(|(id, fields)| (id.to_string(), fields.as_object().cloned().unwrap_or_default())).collect()
}

/// Operations to run, in order, on each device that can do them.
pub fn sample_ops() -> Vec<(String, Op)> {
    vec![
        // device, operation
        {{sample_ops}}
    ]
}

/// What the stand-in does beyond storing the fields it is sent.
fn apply(state: &mut Map<String, Value>, set: &Map<String, Value>) {
    for (key, value) in set {
        state.insert(key.clone(), value.clone());
    }
    {{mock_rules}}
}

/// The stand-in device.
pub struct Stand {
{{#local}}
    pub device: MockUdp,
    /// What the conformance suite turns on: spoiled replies, a hostile name.
    pub switches: Switches,
{{/local}}
{{#cloud}}
    pub device: MockHttp,
{{/cloud}}
    /// Each device's fields now.
    pub states: Arc<Mutex<Devices>>,
    /// Every change that reached it: the device and the fields, oldest first.
    pub sets: Arc<Mutex<Changes>>,
}

/// The fields as the stand-in reports them. Any text in them is what the suite says to report (a long
/// hostile name, to see that the plugin cleans it), cut to fit one datagram.
fn reported(state: &Map<String, Value>, switches: &Switches) -> Map<String, Value> {
    let report = |value: &Value| match value {
        Value::String(text) => Value::String(switches.device_text(text).chars().take(600).collect()),
        other => other.clone(),
    };
    state.iter().map(|(key, value)| (key.clone(), report(value))).collect()
}

/// The stand-in's answer to one request.
fn answer(states: &Mutex<Devices>, sets: &Mutex<Changes>, switches: &Switches, body: &[u8]) -> Value {
    let Ok(request) = serde_json::from_slice::<Value>(body) else {
        return json!({ "ok": false, "error": "not JSON" });
    };
    let Some(id) = request.get("id").and_then(Value::as_str) else {
        return json!({ "ok": false, "error": "no id" });
    };
    let mut states = states.lock().unwrap();
    let Some(state) = states.get_mut(id) else {
        return json!({ "ok": false, "error": "no such device" });
    };
    if let Some(set) = request.get("set").and_then(Value::as_object) {
        sets.lock().unwrap().push((id.to_string(), set.clone()));
        apply(state, set);
        return json!({ "ok": true });
    }
    json!({ "ok": true, "state": reported(state, switches) })
}

impl Stand {
    pub async fn start() -> Self {
        let states = Arc::new(Mutex::new(initial()));
        let sets = Arc::new(Mutex::new(Vec::new()));
        let (for_device, sets_for_device) = (states.clone(), sets.clone());
{{#local}}
        let switches = Switches::default();
        let rigged = switches.clone();
        let device = MockUdp::start(move |datagram| {
            let reply = answer(&for_device, &sets_for_device, &rigged, datagram);
            Some(rigged.garble(reply.to_string().into_bytes()))
        })
        .await;
{{/local}}
{{#cloud}}
        // `start_rigged` hands the handler the suite's switches and spoils bodies itself.
        let device = MockHttp::start_rigged(move |seen, switches| {
            if seen.method != "POST" || !seen.path.starts_with("/v1/devices/") {
                return (404, "{}".to_string());
            }
            if seen.header("authorization") != format!("Bearer {TOKEN}") {
                return (401, "{}".to_string());
            }
            (200, answer(&for_device, &sets_for_device, switches, seen.body.as_bytes()).to_string())
        })
        .await;
{{/cloud}}
{{#local}}
        Self { device, switches, states, sets }
{{/local}}
{{#cloud}}
        Self { device, states, sets }
{{/cloud}}
    }

    /// One config row per device, pointing at the stand-in.
    pub fn row(&self, id: &str, class: &str) -> Value {
{{#local}}
        json!({ "id": id, "address": "127.0.0.1", "port": self.device.addr.port(), "class": class })
{{/local}}
{{#cloud}}
        json!({ "id": id, "class": class })
{{/cloud}}
    }

    /// The instance config for every device of the stand-in.
    pub fn config(&self) -> Value {
        self.config_with(&[])
    }

    /// The same, with more rows.
    pub fn config_with(&self, extra: &[Value]) -> Value {
        let mut rows: Vec<Value> = DEVICES.iter().map(|(id, class)| self.row(id, class)).collect();
        rows.extend(extra.iter().cloned());
{{#local}}
        json!({ "devices": rows })
{{/local}}
{{#cloud}}
        json!({ "api_url": self.device.url, "devices": rows })
{{/cloud}}
    }

    /// The live driver for this config, built by the plugin itself.
    pub fn driver_for(&self, config: Value) -> Arc<dyn Driver> {
        PLUGIN
            .connect(LiveContext {
                instance_id: "{{id}}".to_string(),
                config,
                secrets: secrets(),
                grant: grant(),
                timeouts: timeouts(),
                directory: Arc::new(NoDevices),
            })
            .unwrap()
    }

    pub fn driver(&self) -> Arc<dyn Driver> {
        self.driver_for(self.config())
    }

    /// Every connection or datagram that reached the stand-in.
    pub fn traffic(&self) -> usize {
        self.device.traffic()
    }

    /// The inputs that set the first device up through the setup flow.
    pub fn setup_script(&self) -> Vec<SetupInput> {
        let (id, class) = DEVICES[0];
{{#local}}
        let device = [
            ("id", text(id)),
            ("class", text(class)),
            ("name", text("Desk")),
            ("address", text("127.0.0.1")),
            ("port", Scalar::Int(i64::from(self.device.addr.port()))),
        ];
{{/local}}
{{#cloud}}
        let device = [("id", text(id)), ("class", text(class)), ("name", text("Desk"))];
{{/cloud}}
{{#local}}
        vec![values(&device)]
{{/local}}
{{#cloud}}
        let account = [("api_url", text(&self.device.url)), ("token", text(TOKEN))];
        vec![values(&account), values(&device)]
{{/cloud}}
    }
}

/// The secrets the instance holds.
pub fn secrets() -> Secrets {
{{#local}}
    Secrets::default()
{{/local}}
{{#cloud}}
    Secrets::of([("token", TOKEN)])
{{/cloud}}
}

/// The slots that hold a value.
pub fn present() -> BTreeSet<String> {
{{#local}}
    BTreeSet::new()
{{/local}}
{{#cloud}}
    ["token".to_string()].into()
{{/cloud}}
}

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// No adopted devices: this plugin's devices all come from its config.
pub struct NoDevices;

impl Directory for NoDevices {
    fn get(&self, _local_id: &str) -> Option<DeviceRecord> {
        None
    }
    fn all(&self) -> Vec<DeviceRecord> {
        Vec::new()
    }
}

/// What the pure `refusal` says.
pub fn refusal(config: &Value, device: &str, op: &Op) -> Option<String> {
    let present = present();
    let view = InstanceView { id: "{{id}}", config, secrets_present: &present, adopted: &[] };
    PLUGIN.refusal(&view, device, op)
}

pub fn context(live: bool) -> SetupContext {
    SetupContext {
        instance_id: "{{id}}".to_string(),
        grant: LiveGrant::when_live(live),
        timeouts: timeouts(),
        secrets_present: BTreeSet::new(),
    }
}

pub fn text(value: &str) -> Scalar {
    Scalar::Text(value.to_string())
}

pub fn values(pairs: &[(&str, Scalar)]) -> SetupInput {
    SetupInput::Values { values: pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect() }
}

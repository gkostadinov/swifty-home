//! What the tests share: a live instance built through the plugin's own API, and
//! a TV on a plain websocket that can be paired.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use swifty_plugin_api::net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    CommandAck, DeviceError, DeviceRecord, Directory, Driver, InstanceView, LiveContext, Op, Plugin, PluginError,
    Secrets, State,
};
use swifty_plugin_testkit::devices::{MockTv, MockWs, TV_KEY};
use swifty_plugin_webos::{CLIENT_KEY, PLUGIN};

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// No adopted devices: the TV comes from the config.
pub struct NoneAdopted;

impl Directory for NoneAdopted {
    fn get(&self, _local_id: &str) -> Option<DeviceRecord> {
        None
    }

    fn all(&self) -> Vec<DeviceRecord> {
        Vec::new()
    }
}

/// The config for a mock TV, with Wake-on-LAN to `wol` when given.
pub fn tv_config(tv: &MockTv, wol: Option<String>) -> Value {
    let mut config = json!({ "url": tv.url });
    if let Some(wol) = wol {
        config["wol_mac"] = json!("aa:bb:cc:dd:ee:ff");
        config["wol_broadcast"] = json!(wol);
    }
    config
}

/// One live instance, called as the hub would: the driver, and the plugin's pure refusal.
pub struct Instance {
    pub config: Value,
    pub driver: Arc<dyn Driver>,
}

pub fn try_connect(config: Value, key: Option<&str>) -> Result<Instance, PluginError> {
    let secrets = match key {
        Some(key) => Secrets::of([(CLIENT_KEY, key)]),
        None => Secrets::default(),
    };
    let ctx = LiveContext {
        instance_id: "webos".to_string(),
        config: config.clone(),
        secrets,
        grant: grant(),
        timeouts: timeouts(),
        directory: Arc::new(NoneAdopted),
    };
    let driver = PLUGIN.connect(ctx)?;
    Ok(Instance { config, driver })
}

impl Instance {
    /// With the key the mock TVs accept.
    pub fn connect(config: Value) -> Self {
        try_connect(config, Some(TV_KEY)).unwrap()
    }

    pub fn refusal(&self, device: &str, op: &Op) -> Option<String> {
        let present = BTreeSet::from([CLIENT_KEY.to_string()]);
        let view = InstanceView { id: "webos", config: &self.config, secrets_present: &present, adopted: &[] };
        PLUGIN.refusal(&view, device, op)
    }

    pub fn poll_refusal(&self) -> Option<String> {
        self.driver.poll_refusal()
    }

    pub async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        self.driver.command(device, op).await
    }

    pub async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        self.driver.read_state(device).await
    }
}

/// What a [`PairingTv`] does when it is asked to pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// The person accepts at once: the TV answers with its key.
    Accepted,
    /// The prompt is up and nobody accepts it.
    Ignored,
    /// The person declines.
    Declined,
    /// The TV accepts but sends a key that cannot be stored.
    BadKey,
}

/// A TV on a plain websocket that can be paired, unlike [`MockTv`], which only
/// knows a key it was given. It hands out [`TV_KEY`], accepts it afterwards, and
/// answers the two read calls.
pub struct PairingTv {
    pub ws: MockWs,
    pub prompts: Arc<AtomicUsize>,
    pub prompt: Arc<Mutex<Prompt>>,
}

impl PairingTv {
    pub async fn start(prompt: Prompt) -> Self {
        let prompts = Arc::new(AtomicUsize::new(0));
        let prompt = Arc::new(Mutex::new(prompt));
        let (raised, mode) = (prompts.clone(), prompt.clone());
        let ws = MockWs::start(move |frame| {
            let message: Value = serde_json::from_str(frame).ok()?;
            let id = message["id"].clone();
            if message["type"] == "register" {
                let presented = message["payload"].get("client-key").and_then(Value::as_str);
                if presented == Some(TV_KEY) {
                    return Some(json!({ "type": "registered", "id": id, "payload": { "client-key": TV_KEY } }).to_string());
                }
                raised.fetch_add(1, Ordering::SeqCst);
                let reply = match (*mode.lock().unwrap(), presented) {
                    (Prompt::Accepted, None) => {
                        json!({ "type": "registered", "id": id, "payload": { "client-key": TV_KEY } })
                    }
                    (Prompt::BadKey, None) => json!({ "type": "registered", "id": id, "payload": { "client-key": "" } }),
                    (Prompt::Declined, None) => json!({ "type": "error", "id": id, "error": "403 denied by the user" }),
                    _ => json!({ "type": "response", "id": id, "payload": { "pairingType": "PROMPT", "returnValue": true } }),
                };
                return Some(reply.to_string());
            }
            let payload = match message["uri"].as_str().unwrap_or("") {
                "ssap://com.webos.service.tvpower/power/getPowerState" => json!({ "returnValue": true, "state": "Active" }),
                "ssap://com.webos.applicationManager/getForegroundAppInfo" => {
                    json!({ "returnValue": true, "appId": "com.webos.app.hdmi2" })
                }
                "ssap://system.launcher/launch" => json!({ "returnValue": true }),
                _ => json!({ "returnValue": false, "errorText": "404 no such service or method" }),
            };
            Some(json!({ "type": "response", "id": id, "payload": payload }).to_string())
        })
        .await;
        Self { ws, prompts, prompt }
    }

    /// Every message the TV received, as JSON.
    pub fn received(&self) -> Vec<Value> {
        self.ws.frames.lock().unwrap().iter().filter_map(|frame| serde_json::from_str(frame).ok()).collect()
    }

    pub fn registrations(&self) -> Vec<Value> {
        self.received().into_iter().filter(|message| message["type"] == "register").collect()
    }
}

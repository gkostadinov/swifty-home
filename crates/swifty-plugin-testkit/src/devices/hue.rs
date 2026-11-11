//! A Philips Hue bridge with lights.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::http::Seen;
use crate::conformance::Switches;

pub const HUE_KEY: &str = "hue-app-key-0123456789";

pub fn hue_handler(seen: &Seen) -> (u16, String) {
    if seen.path == format!("/api/{HUE_KEY}/groups/0") {
        (200, json!({ "name": "Group 0", "lights": ["1"], "state": { "all_on": false, "any_on": true } }).to_string())
    } else {
        (200, json!([{ "error": { "type": 1, "address": "/", "description": "unauthorized user" } }]).to_string())
    }
}

/// A light on the mock bridge.
#[derive(Debug, Clone)]
pub struct HueLight {
    pub name: String,
    pub on: bool,
    pub bri: u64,
    /// `None` for a light with no colour.
    pub hue: Option<u64>,
    pub sat: u64,
    pub colormode: String,
    pub reachable: bool,
}

impl HueLight {
    pub fn colour(name: &str) -> Self {
        Self {
            name: name.to_string(),
            on: false,
            bri: 127,
            hue: Some(8_000),
            sat: 100,
            colormode: "ct".to_string(),
            reachable: true,
        }
    }

    pub fn white(name: &str) -> Self {
        Self {
            name: name.to_string(),
            on: true,
            bri: 127,
            hue: None,
            sat: 0,
            colormode: String::new(),
            reachable: true,
        }
    }

    fn json(&self, name: &str) -> Value {
        let mut state = json!({ "on": self.on, "bri": self.bri, "alert": "none", "mode": "homeautomation", "reachable": self.reachable });
        if let Some(hue) = self.hue {
            state["hue"] = json!(hue);
            state["sat"] = json!(self.sat);
            state["ct"] = json!(366);
            state["xy"] = json!([0.45, 0.41]);
            state["effect"] = json!("none");
            state["colormode"] = json!(self.colormode);
        }
        let kind = if self.hue.is_some() { "Extended color light" } else { "Dimmable light" };
        json!({ "name": name, "type": kind, "modelid": "LST002", "state": state })
    }
}

#[derive(Debug, Default)]
pub struct HueBridge {
    pub lights: BTreeMap<String, HueLight>,
    /// Every change is answered with this error, whose text repeats the key.
    pub error: Option<u32>,
    /// Set: changes are accepted and do nothing.
    pub ignores: bool,
}

pub type SharedBridge = Arc<Mutex<HueBridge>>;

fn bridge_error(code: u32, address: &str) -> String {
    json!([{ "error": { "type": code, "address": format!("/api/{HUE_KEY}{address}"), "description": format!("no for {HUE_KEY}") } }]).to_string()
}

/// A bridge with lights. Anything else is answered as [`hue_handler`] does.
pub fn hue_bridge_handler(bridge: SharedBridge) -> impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static {
    let rigged = hue_bridge_rigged(bridge);
    move |seen| rigged(seen, &Switches::default())
}

/// [`hue_bridge_handler`], reporting the conformance suite's text as every light's name.
pub fn hue_bridge_rigged(bridge: SharedBridge) -> impl Fn(&Seen, &Switches) -> (u16, String) + Send + Sync + 'static {
    move |seen, switches| {
        let Some(rest) = seen.path.strip_prefix(&format!("/api/{HUE_KEY}/lights")) else { return hue_handler(seen) };
        let mut bridge = bridge.lock().unwrap();
        if rest.is_empty() {
            let all: serde_json::Map<String, Value> = bridge
                .lights
                .iter()
                .map(|(id, light)| (id.clone(), light.json(&switches.device_text(&light.name))))
                .collect();
            return (200, Value::Object(all).to_string());
        }
        let parts: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
        let (error, ignores) = (bridge.error, bridge.ignores);
        let Some(light) = bridge.lights.get_mut(parts[0]) else { return (200, bridge_error(3, rest)) };
        match (seen.method.as_str(), &parts[1..]) {
            ("GET", []) => {
                let name = switches.device_text(&light.name);
                (200, light.json(&name).to_string())
            }
            ("PUT", ["state"]) => {
                if let Some(code) = error {
                    return (200, bridge_error(code, rest));
                }
                let body: Value = serde_json::from_str(&seen.body).unwrap();
                if body.get("hue").is_some() && light.hue.is_none() {
                    return (200, bridge_error(6, rest));
                }
                let mut replies = Vec::new();
                for (name, value) in body.as_object().unwrap() {
                    if !ignores {
                        match name.as_str() {
                            "on" => light.on = value.as_bool().unwrap(),
                            "bri" => light.bri = value.as_u64().unwrap(),
                            "hue" => {
                                light.hue = value.as_u64();
                                light.colormode = "hs".to_string();
                            }
                            "sat" => light.sat = value.as_u64().unwrap(),
                            other => panic!("the bridge was sent {other}"),
                        }
                    }
                    replies.push(json!({ "success": { format!("/lights/{}/state/{name}", parts[0]): value } }));
                }
                (200, Value::Array(replies).to_string())
            }
            _ => (200, bridge_error(4, rest)),
        }
    }
}

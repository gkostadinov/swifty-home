//! The live side: HTTP to the bridge, with the application key in the path.
//! Every request goes through `swifty-net`, to the configured bridge and nowhere
//! else, once.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use swifty_net::{Http, LiveGrant, Timeouts};
use swifty_plugin_api::{
    Capability, CommandAck, DeviceClass, DeviceDecl, DeviceError, DeviceRecord, Directory, Driver, Health,
    HealthStatus, Op, Secret, State,
};

use crate::protocol::{self, bridge_number, Pairing, BRIDGE_ID, NO_COLOUR};

/// What a light on the bridge can do. Brightness goes down to 1 percent; 0 is off.
pub fn light_capabilities(colour: bool) -> Vec<Capability> {
    let mut capabilities = vec![Capability::Power, Capability::Brightness { min: 1 }];
    if colour {
        capabilities.push(Capability::Color);
    }
    capabilities
}

/// Whether an adopted light's record says it can show colour. A record that
/// does not say is asked about at the bridge before a colour is sent.
pub fn record_colour(record: &DeviceRecord) -> Option<bool> {
    record.attrs.get("colour").and_then(Value::as_bool)
}

/// Why this cannot go to this device, if it cannot. `record` is the adopted
/// record of `device`, if there is one. Pure.
pub fn refusal(device: &str, op: &Op, record: Option<&DeviceRecord>) -> Option<String> {
    if device == BRIDGE_ID {
        return Some("The Hue Bridge itself takes no commands. Its lights do.".to_string());
    }
    if !op.is_light_op() {
        return Some("A Hue light cannot do that.".to_string());
    }
    let Some(record) = record.filter(|_| protocol::is_light_id(device)) else {
        return Some(format!("{device} is not a known light."));
    };
    let wants_colour = matches!(op, Op::LightHue { .. } | Op::LightSet { percent: 1.., .. });
    (wants_colour && record_colour(record) == Some(false)).then(|| NO_COLOUR.to_string())
}

/// A light the bridge listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundLight {
    /// The bridge's number for it.
    pub number: String,
    /// Its name on the bridge, made safe.
    pub name: String,
    /// Whether it can show colour.
    pub colour: bool,
}

impl FoundLight {
    /// Its local id: `hue-<number>`.
    pub fn local_id(&self) -> String {
        format!("hue-{}", self.number)
    }

    /// The device it would be once adopted.
    pub fn decl(&self) -> DeviceDecl {
        let id = self.local_id();
        DeviceDecl {
            local_id: id.clone(),
            target_hint: Some(id),
            class: DeviceClass::Light,
            name: self.name.clone(),
            capabilities: light_capabilities(self.colour),
            attrs: json!({ "light": self.number, "colour": self.colour }),
        }
    }
}

/// Why pairing did not give a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    /// The bridge only agrees for 30 seconds after its link button is pressed.
    LinkButtonNotPressed,
    /// Anything else.
    Failed(DeviceError),
}

/// Asks the bridge for an application key. The key is handed back as a
/// [`Secret`] and is never shown, logged or put anywhere else here.
pub async fn pair(grant: LiveGrant, url: &str, timeouts: Timeouts) -> Result<Secret, PairError> {
    let http =
        Http::new(grant, url, timeouts, None).map_err(|reason| PairError::Failed(DeviceError::Disabled(reason)))?;
    let target = http.url("/api").map_err(PairError::Failed)?;
    let reply = http.send(http.post(target).json(&protocol::pairing_request())).await.map_err(PairError::Failed)?;
    if !reply.status.is_success() {
        return Err(PairError::Failed(protocol::refused_status(&reply)));
    }
    match protocol::pairing_answer(&reply.json().map_err(PairError::Failed)?) {
        Pairing::Key(key) => Ok(key.into()),
        Pairing::ButtonNotPressed => Err(PairError::LinkButtonNotPressed),
        Pairing::Refused(error) => Err(PairError::Failed(error)),
    }
}

/// The driver of one bridge.
pub struct HueDriver {
    http: Http,
    key: Secret,
    directory: Arc<dyn Directory>,
}

impl HueDriver {
    /// Fails for a bridge URL the HTTP client will not take, or a key that could
    /// change the path it is put in.
    pub fn new(
        grant: LiveGrant,
        url: &str,
        key: Secret,
        timeouts: Timeouts,
        directory: Arc<dyn Directory>,
    ) -> Result<Self, String> {
        if !protocol::is_usable_key(key.expose()) {
            return Err("the Hue application key holds invalid text".to_string());
        }
        Ok(Self { http: Http::new(grant, url, timeouts, None)?, key, directory })
    }

    /// A path under this key. It only ever goes into a URL on the bridge.
    fn path(&self, rest: &str) -> String {
        format!("/api/{}{rest}", self.key.expose())
    }

    async fn fetch(&self, rest: &str) -> Result<Value, DeviceError> {
        let url = self.http.url(&self.path(rest))?;
        protocol::checked(&self.http.send(self.http.get(url)).await?)
    }

    /// The bridge's number for a light this instance has adopted.
    fn known_number<'a>(&self, device: &'a str) -> Result<&'a str, DeviceError> {
        bridge_number(device)
            .filter(|_| self.directory.get(device).is_some())
            .ok_or_else(|| DeviceError::Disabled(format!("{device} is not a known light.")))
    }

    /// Group 0 is small and answers with an error for a key the bridge does not
    /// know. The bridge's public config would answer either way.
    async fn bridge_state(&self) -> Result<State, DeviceError> {
        let url = self.http.url(&self.path("/groups/0"))?;
        let reply = self.http.send(self.http.get(url)).await?;
        if !reply.status.is_success() {
            return Err(protocol::refused_status(&reply));
        }
        if reply.json()?.pointer("/0/error").is_some() {
            return Err(DeviceError::Rejected("the bridge did not accept the application key".to_string()));
        }
        Ok(State::Bridge { reachable: true })
    }

    /// One light's state, by the bridge's number. The caller checks that it may be read.
    pub(crate) async fn light_state(&self, number: &str) -> Result<State, DeviceError> {
        protocol::bulb_state(&self.fetch(&format!("/lights/{number}")).await?)
    }

    /// Every light on the bridge, in the bridge's order of numbers.
    pub async fn list_lights(&self) -> Result<Vec<FoundLight>, DeviceError> {
        let body = self.fetch("/lights").await?;
        let lights = body.as_object().ok_or_else(protocol::unreadable)?;
        let mut found: Vec<FoundLight> = lights
            .iter()
            .filter(|(number, _)| protocol::is_light_id(&format!("hue-{number}")))
            .take(protocol::MAX_LIGHTS)
            .map(|(number, light)| FoundLight {
                number: number.clone(),
                name: protocol::light_name(light, number),
                colour: protocol::has_colour(light),
            })
            .collect();
        found.sort_by_key(|light| (light.number.len(), light.number.clone()));
        Ok(found)
    }
}

#[async_trait]
impl Driver for HueDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        if let Some(reason) = refusal(device, op, self.directory.get(device).as_ref()) {
            return Err(DeviceError::Disabled(reason));
        }
        let number = self.known_number(device)?;
        let body =
            protocol::wire_state(op).ok_or_else(|| DeviceError::Disabled("A Hue light cannot do that.".to_string()))?;
        if body.get("hue").is_some() {
            let light = self.fetch(&format!("/lights/{number}")).await?;
            if !protocol::has_colour(&light) {
                return Err(DeviceError::Disabled(NO_COLOUR.to_string()));
            }
        }
        let url = self.http.url(&self.path(&format!("/lights/{number}/state")))?;
        let reply = self.http.send(self.http.put(url).json(&body)).await?;
        protocol::all_succeeded(&protocol::checked(&reply)?)?;
        Ok(CommandAck)
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        if device == BRIDGE_ID {
            return self.bridge_state().await;
        }
        let number = self.known_number(device)?;
        self.light_state(number).await
    }

    /// The bridge's lights. The window does not apply: the bridge answers at once.
    async fn discover(&self, _window: Duration) -> Result<Vec<DeviceDecl>, DeviceError> {
        Ok(self.list_lights().await?.iter().map(FoundLight::decl).collect())
    }

    async fn health(&self) -> Health {
        match self.bridge_state().await {
            Ok(_) => Health { status: HealthStatus::Ok, detail: None },
            Err(error) => Health { status: HealthStatus::Down, detail: Some(error.to_string()) },
        }
    }
}

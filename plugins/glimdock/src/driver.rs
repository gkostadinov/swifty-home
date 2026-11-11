//! The live driver: reads the node's snapshot and asks it to pair an agent.
//! Nothing else can be sent to a node.

use async_trait::async_trait;
use http::header::{HeaderValue, ACCEPT, AUTHORIZATION};
use serde_json::Value;
use swifty_net::{Http, LiveGrant, Reply, Timeouts};
use swifty_plugin_api::{AgentPairing, CommandAck, DeviceError, Driver, Op, Secret, State};

use crate::config::GlimdockConfig;
use crate::protocol::{cleaned, pairing_request, with_flat_readings, MAX_SNAPSHOT};
use crate::{DEVICE, READ_ONLY};

/// A live connection to one Glimdock node.
pub struct GlimdockDriver {
    http: Http,
    snapshot_path: String,
    pairing_path: Option<String>,
    authorization: HeaderValue,
}

impl GlimdockDriver {
    /// Builds the client. It does not check `config`, so a test can prove the
    /// driver refuses a bad path by itself; `connect` checks it first.
    pub fn new(grant: LiveGrant, config: &GlimdockConfig, token: &Secret, timeouts: Timeouts) -> Result<Self, String> {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
            .map_err(|_| "the node's access token holds characters a header cannot carry".to_string())?;
        authorization.set_sensitive(true);
        Ok(Self {
            http: Http::new(grant, &config.url, timeouts, None)?.with_max_body(MAX_SNAPSHOT),
            snapshot_path: config.snapshot_path.clone(),
            pairing_path: config.pairing_path.clone(),
            authorization,
        })
    }
}

/// What the node's answer means: JSON on success, a refusal otherwise.
fn answer(reply: &Reply) -> Result<Value, DeviceError> {
    match reply.status.as_u16() {
        200..=299 => reply.json(),
        401 | 403 => Err(DeviceError::Rejected("the Glimdock node did not accept the token".to_string())),
        status => Err(DeviceError::Rejected(format!("the Glimdock node refused with HTTP {status}"))),
    }
}

fn unknown_device() -> DeviceError {
    DeviceError::Disabled("A Glimdock node has one device, and this is not it.".to_string())
}

#[async_trait]
impl Driver for GlimdockDriver {
    async fn command(&self, _device: &str, _op: &Op) -> Result<CommandAck, DeviceError> {
        Err(DeviceError::Disabled(READ_ONLY.to_string()))
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        if device != DEVICE {
            return Err(unknown_device());
        }
        let request = self
            .http
            .get(self.http.url(&self.snapshot_path)?)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(ACCEPT, "application/json");
        let reply = self.http.send(request).await?;
        Ok(State::Telemetry { data: with_flat_readings(cleaned(answer(&reply)?)) })
    }

    fn agents(&self) -> Option<&dyn AgentPairing> {
        Some(self)
    }
}

#[async_trait]
impl AgentPairing for GlimdockDriver {
    /// Asks the node to start its own agent pairing. What comes back is for
    /// the person installing the agent, and an agent only ever reports readings.
    async fn pair_agent(&self, name: &str) -> Result<Value, DeviceError> {
        let path = self.pairing_path.as_deref().ok_or_else(|| {
            DeviceError::Disabled("pairing_path is not configured for this Glimdock node.".to_string())
        })?;
        let request = self
            .http
            .post(self.http.url(path)?)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(ACCEPT, "application/json")
            .json(&pairing_request(name));
        let reply = self.http.send(request).await?;
        answer(&reply)
    }
}

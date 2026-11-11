//! The live driver. A hub builds it only when it is live, and it reaches the
//! devices through `swifty_plugin_api::net` alone, whose helpers need the grant
//! the hub hands over and apply the timeouts. It never retries: the hub decides
//! whether an operation may be sent twice.

use std::collections::HashMap;

use async_trait::async_trait;
{{#local}}
use swifty_plugin_api::net::{self, LiveGrant, SocketAddr, Timeouts};
{{/local}}
{{#cloud}}
use swifty_plugin_api::net::{self, LiveGrant, Timeouts};
{{/cloud}}
{{#local}}
use swifty_plugin_api::{CommandAck, DeviceClass, DeviceError, Driver, Op, State};
{{/local}}
{{#cloud}}
use swifty_plugin_api::{CommandAck, DeviceClass, DeviceError, Driver, Op, PluginError, Secret, State};
{{/cloud}}

use crate::config::Config;
use crate::protocol::{self, Fields};

{{#local}}
/// One device: what it is and where it listens.
struct Device {
    class: DeviceClass,
    address: Option<SocketAddr>,
}

/// A live connection to one instance's devices.
pub struct {{Type}}Driver {
    grant: LiveGrant,
    timeouts: Timeouts,
    devices: HashMap<String, Device>,
}

impl {{Type}}Driver {
    /// Builds the driver from a config that passed `config::parse`.
    pub fn new(grant: LiveGrant, timeouts: Timeouts, config: &Config) -> Self {
        let devices = config
            .devices
            .iter()
            .map(|row| (row.id.clone(), Device { class: row.class(), address: row.socket() }))
            .collect();
        Self { grant, timeouts, devices }
    }

    fn device(&self, id: &str) -> Result<&Device, DeviceError> {
        self.devices.get(id).ok_or_else(|| DeviceError::Disabled(format!("{id} is not a device of this instance.")))
    }

    /// Sends one request and reads its one reply.
    async fn exchange(&self, device: &Device, request: Vec<u8>) -> Result<Fields, DeviceError> {
        let address =
            device.address.ok_or_else(|| DeviceError::Disabled("The device's address is not valid.".to_string()))?;
        let reply = net::udp_exchange(self.grant, address, &request, self.timeouts.read).await?;
        protocol::parse_reply(&reply)
    }
}
{{/local}}
{{#cloud}}
/// A live connection to one instance's devices, through the vendor's API.
pub struct {{Type}}Driver {
    http: net::Http,
    token: Secret,
    devices: HashMap<String, DeviceClass>,
}

impl {{Type}}Driver {
    /// Builds the driver from a config that passed `config::parse`.
    pub fn new(grant: LiveGrant, timeouts: Timeouts, config: &Config, token: Secret) -> Result<Self, PluginError> {
        // A base without a final slash would lose its last segment when a path is added.
        let base = if config.api_url.ends_with('/') { config.api_url.clone() } else { format!("{}/", config.api_url) };
        let http = net::Http::new(grant, &base, timeouts, None).map_err(PluginError)?;
        let devices = config.devices.iter().map(|row| (row.id.clone(), row.class())).collect();
        Ok(Self { http, token, devices })
    }

    fn class(&self, id: &str) -> Result<DeviceClass, DeviceError> {
        self.devices
            .get(id)
            .copied()
            .ok_or_else(|| DeviceError::Disabled(format!("{id} is not a device of this instance.")))
    }

    /// Sends one request about one device and reads its one reply.
    async fn exchange(&self, id: &str, request: Vec<u8>) -> Result<Fields, DeviceError> {
        let url = self.http.url(&format!("v1/devices/{id}"))?;
        let request = self
            .http
            .post(url)
            .header("authorization", format!("Bearer {}", self.token.expose()))
            .header("content-type", "application/json")
            .body(request);
        let reply = self.http.send(request).await?;
        match reply.status.as_u16() {
            200 => protocol::parse_reply(&reply.body),
            401 | 403 => Err(DeviceError::Rejected("the access token was refused".to_string())),
            429 => Err(DeviceError::Rejected("the service asks to slow down".to_string())),
            status => Err(DeviceError::Rejected(format!("the service answered {status}"))),
        }
    }
}
{{/cloud}}

#[async_trait]
impl Driver for {{Type}}Driver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
{{#local}}
        let known = self.device(device)?;
        let set = protocol::encode(known.class, op).map_err(|why| DeviceError::Disabled(why.to_string()))?;
        self.exchange(known, protocol::set_request(device, &set)).await?;
{{/local}}
{{#cloud}}
        let class = self.class(device)?;
        let set = protocol::encode(class, op).map_err(|why| DeviceError::Disabled(why.to_string()))?;
        self.exchange(device, protocol::set_request(device, &set)).await?;
{{/cloud}}
        Ok(CommandAck)
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
{{#local}}
        let known = self.device(device)?;
        let fields = self.exchange(known, protocol::read_request(device)).await?;
        protocol::decode(known.class, &fields)
{{/local}}
{{#cloud}}
        let class = self.class(device)?;
        let fields = self.exchange(device, protocol::read_request(device)).await?;
        protocol::decode(class, &fields)
{{/cloud}}
    }
}

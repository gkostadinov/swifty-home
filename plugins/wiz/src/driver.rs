//! The live side: one datagram per command, one per reading, and a broadcast to
//! find bulbs. Every socket is opened by `swifty-net` with the hub's grant.

use std::sync::Arc;
use std::time::Duration;
use swifty_net::SocketAddr;

use async_trait::async_trait;
use serde_json::{json, Value};
use swifty_net::{self as net, LiveGrant};
use swifty_plugin_api::{
    Capability, CommandAck, DeviceClass, DeviceDecl, DeviceError, DeviceRecord, Directory, Driver, Op, State,
    MIN_LIGHT_DIMMING,
};

use crate::config::{parse_address, WizConfig};
use crate::protocol::{self, discovered_id, is_discovered_id};

/// The longest a discovery listens for answers, whatever window the hub asks for.
pub const MAX_DISCOVERY_WINDOW: Duration = Duration::from_secs(10);

/// What a light of this plugin can do.
pub fn light_capabilities() -> Vec<Capability> {
    vec![Capability::Power, Capability::Brightness { min: MIN_LIGHT_DIMMING }, Capability::Color]
}

/// A bulb that answered a discovery broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundBulb {
    /// Twelve lower-case hex digits.
    pub mac: String,
    /// Where its answer came from.
    pub address: SocketAddr,
}

impl FoundBulb {
    /// The device it would be once adopted. The address is kept in `attrs`, so
    /// the hub can store it and the driver can find the bulb again.
    pub fn decl(&self) -> DeviceDecl {
        let id = discovered_id(&self.mac);
        DeviceDecl {
            local_id: id.clone(),
            target_hint: Some(id),
            class: DeviceClass::Light,
            name: format!("WiZ light {}", &self.mac[8..]),
            capabilities: light_capabilities(),
            attrs: json!({ "mac": self.mac, "address": self.address.to_string() }),
        }
    }
}

/// Where an adopted bulb is: the address its record keeps. Only a discovered id
/// counts, so a record cannot point a configured id somewhere else.
pub fn adopted_address(record: &DeviceRecord) -> Option<SocketAddr> {
    if !is_discovered_id(&record.local_id) {
        return None;
    }
    record.attrs.get("address").and_then(Value::as_str).and_then(|text| parse_address(text, protocol::DEFAULT_PORT))
}

/// Asks every bulb on the network to say who it is. Nothing here changes a bulb.
pub async fn discover(
    grant: LiveGrant,
    broadcast: SocketAddr,
    window: Duration,
) -> Result<Vec<FoundBulb>, DeviceError> {
    let message = protocol::discovery_message().to_string();
    let replies = net::udp_discover(grant, broadcast, message.as_bytes(), window).await?;
    let mut found: Vec<FoundBulb> = Vec::new();
    for (address, reply) in replies {
        if !protocol::answered_the_broadcast(broadcast, address) {
            continue;
        }
        let Some(mac) = protocol::discovered_mac(&reply) else { continue };
        if !found.iter().any(|bulb| bulb.mac == mac) {
            found.push(FoundBulb { mac, address });
        }
    }
    found.sort_by(|a, b| a.mac.cmp(&b.mac));
    Ok(found)
}

/// One request and its reply, sent once.
pub(crate) async fn exchange(
    grant: LiveGrant,
    peer: SocketAddr,
    message: &Value,
    wait: Duration,
) -> Result<Value, DeviceError> {
    let method = message.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
    let reply = net::udp_exchange(grant, peer, message.to_string().as_bytes(), wait).await?;
    protocol::result_of(&reply, &method)
}

/// Reads one bulb's state at an address.
pub(crate) async fn read_at(grant: LiveGrant, peer: SocketAddr, wait: Duration) -> Result<State, DeviceError> {
    protocol::pilot_state(&exchange(grant, peer, &protocol::get_pilot(), wait).await?)
}

/// The driver of one WiZ instance.
pub struct WizDriver {
    grant: LiveGrant,
    config: WizConfig,
    wait: Duration,
    directory: Arc<dyn Directory>,
}

impl WizDriver {
    /// `wait` is the read timeout, for each reply.
    pub fn new(grant: LiveGrant, config: WizConfig, wait: Duration, directory: Arc<dyn Directory>) -> Self {
        Self { grant, config, wait, directory }
    }

    fn address(&self, device: &str) -> Result<SocketAddr, DeviceError> {
        let address = if is_discovered_id(device) {
            self.directory.get(device).as_ref().and_then(adopted_address)
        } else {
            self.config.address(device)
        };
        address.ok_or_else(|| DeviceError::Disabled(format!("{device} has no address in this instance.")))
    }
}

#[async_trait]
impl Driver for WizDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        let message =
            protocol::set_pilot(op).ok_or_else(|| DeviceError::Disabled("A WiZ bulb cannot do that.".to_string()))?;
        let peer = self.address(device)?;
        protocol::accepted(&exchange(self.grant, peer, &message, self.wait).await?)?;
        Ok(CommandAck)
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        let peer = self.address(device)?;
        read_at(self.grant, peer, self.wait).await
    }

    async fn discover(&self, window: Duration) -> Result<Vec<DeviceDecl>, DeviceError> {
        let broadcast = self.config.broadcast().ok_or_else(|| {
            DeviceError::Disabled("No broadcast address is configured, so discovery has nowhere to send.".to_string())
        })?;
        let found = discover(self.grant, broadcast, window.min(MAX_DISCOVERY_WINDOW)).await?;
        Ok(found.iter().map(FoundBulb::decl).collect())
    }
}

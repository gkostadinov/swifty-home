//! The live driver: the SSAP websocket, with the stored client key.
//!
//! The driver can only ever present that key. Pairing belongs to the setup flow
//! alone. If the TV does not accept the key it answers by offering a pairing
//! prompt, so the driver hangs up at once and stops connecting until the
//! instance is loaded again, rather than leaving a prompt on the screen or
//! raising another one.
//!
//! Connections are opened one at a time, so a second attempt waits for the first
//! to learn whether the key was accepted. A background poll never makes the first
//! connection: a stale key can then only raise a prompt because someone asked for
//! something on the TV, and at most once.

// Address types only: every socket is opened by `swifty-net`.
use std::sync::atomic::{AtomicBool, Ordering};
use swifty_net::{IpAddr, SocketAddr};

use async_trait::async_trait;
use serde_json::{json, Value};
use swifty_net::{self as net, LiveGrant, Timeouts, Ws};
use swifty_plugin_api::{CommandAck, DeviceError, Driver, Health, HealthStatus, Op, Secret, State};
use tokio::sync::Mutex;
use url::Url;

use crate::config::{parse_mac, WebosConfig};
use crate::protocol::{self, REGISTER_ID};

const NOT_THIS_TV: &str = "the TV offered an input socket somewhere else, so nothing was sent";
/// Why every call fails once the TV has refused the stored key.
pub const KEY_REFUSED: &str = "The TV did not accept the client key. Run the TV's setup again to pair it. \
                               Nothing here starts pairing outside the setup.";
const NOT_CONNECTED_YET: &str = "Connects the first time you use it.";

struct Wake {
    packet: Vec<u8>,
    broadcast: SocketAddr,
}

/// One TV.
pub struct WebosDriver {
    grant: LiveGrant,
    url: Url,
    key: Secret,
    timeouts: Timeouts,
    pin: Option<String>,
    wake: Option<Wake>,
    key_refused: AtomicBool,
    key_accepted: AtomicBool,
    /// Held from connecting until the TV has answered the registration.
    opening: Mutex<()>,
    /// Held for a whole key press, so presses reach the TV in the order they came.
    pressing: Mutex<()>,
}

impl WebosDriver {
    /// `Err` when the config cannot be used. Opens nothing yet.
    pub fn new(grant: LiveGrant, config: &WebosConfig, key: Secret, timeouts: Timeouts) -> Result<Self, String> {
        let url = config.url()?;
        let wake = match &config.wol_mac {
            None => None,
            Some(mac) => {
                let mac = parse_mac(mac).ok_or("wol_mac is not a MAC address")?;
                let broadcast = config.wol_broadcast.as_deref().unwrap_or(protocol::DEFAULT_BROADCAST);
                let broadcast = broadcast.parse().map_err(|_| "wol_broadcast must be IP:port".to_string())?;
                Some(Wake { packet: protocol::magic_packet(mac), broadcast })
            }
        };
        Ok(Self {
            grant,
            url,
            key,
            timeouts,
            pin: config.cert_sha256.clone(),
            wake,
            key_refused: AtomicBool::new(false),
            key_accepted: AtomicBool::new(false),
            opening: Mutex::new(()),
            pressing: Mutex::new(()),
        })
    }

    /// Connects and presents the stored key. Anything but acceptance ends the
    /// connection and stops further attempts. One at a time: each connection a
    /// stale key reaches would raise its own prompt on the screen.
    async fn open(&self) -> Result<Ws, DeviceError> {
        let _one_at_a_time = self.opening.lock().await;
        if self.key_refused.load(Ordering::SeqCst) {
            return Err(DeviceError::Disabled(KEY_REFUSED.to_string()));
        }
        let mut ws = net::ws_connect(self.grant, &self.url, self.pin.as_deref(), self.timeouts).await?;
        ws.send(protocol::register_with_key(self.key.expose())).await?;
        for _ in 0..protocol::MAX_SKIPPED {
            let message = protocol::parse(&ws.recv().await?)?;
            if protocol::id_of(&message) != Some(REGISTER_ID) {
                continue;
            }
            if protocol::type_of(&message) == Some("registered") {
                self.key_accepted.store(true, Ordering::SeqCst);
                return Ok(ws);
            }
            self.key_refused.store(true, Ordering::SeqCst);
            ws.close().await;
            return Err(DeviceError::Disabled(KEY_REFUSED.to_string()));
        }
        ws.close().await;
        Err(protocol::unreadable())
    }

    async fn run(&self, id: &str, uri: &str, payload: Value) -> Result<(), DeviceError> {
        let mut ws = self.open().await?;
        let result = call(&mut ws, id, uri, payload).await;
        ws.close().await;
        result.map(|_| ())
    }

    /// Whether this names the TV: the host it is configured with, or an address that
    /// host stands for right now.
    async fn names_this_tv(&self, reported: &Url) -> Result<bool, DeviceError> {
        if reported.host() == self.url.host() {
            return Ok(true);
        }
        let bare = |host: &'_ str| host.trim_matches(['[', ']']).to_string();
        let Some(address) = reported.host_str().and_then(|host| bare(host).parse::<IpAddr>().ok()) else {
            return Ok(false);
        };
        let (Some(host), Some(port)) = (self.url.host_str(), self.url.port_or_known_default()) else {
            return Ok(false);
        };
        let known = net::resolve(self.grant, &bare(host), port, self.timeouts.connect).await?;
        Ok(known.iter().any(|ip| ip.to_canonical() == address.to_canonical()))
    }

    /// Where to send a key press, given what the TV said. Only the path is used: the
    /// scheme, host, port and certificate pin always come from the configuration, so a
    /// reply cannot move this anywhere else or take TLS away.
    async fn pointer_url(&self, reported: &str) -> Result<Url, DeviceError> {
        let elsewhere = || DeviceError::Rejected(NOT_THIS_TV.to_string());
        let reported = Url::parse(reported).map_err(|_| elsewhere())?;
        let plain = reported.scheme() == "ws";
        if !plain && reported.scheme() != "wss" {
            return Err(elsewhere());
        }
        let same_port = reported.port_or_known_default() == self.url.port_or_known_default()
            || (plain && reported.port() == Some(protocol::PLAIN_PORT));
        if !same_port || !self.names_this_tv(&reported).await? {
            return Err(elsewhere());
        }
        let mut url = self.url.clone();
        url.set_path(reported.path());
        Ok(url)
    }

    /// Sends one button press. The SSAP connection stays open until it has gone, in
    /// case the socket only lasts as long as the session that asked for it.
    async fn press(&self, button: &str) -> Result<(), DeviceError> {
        let _in_order = self.pressing.lock().await;
        let mut ssap = self.open().await?;
        let result = self.send_button(&mut ssap, button).await;
        ssap.close().await;
        result
    }

    async fn send_button(&self, ssap: &mut Ws, button: &str) -> Result<(), DeviceError> {
        let reply = call(ssap, "pointer", protocol::POINTER_SOCKET_URI, json!({})).await?;
        let reported = reply.get("socketPath").and_then(Value::as_str).ok_or_else(protocol::unreadable)?;
        let url = self.pointer_url(reported).await?;
        let mut socket = net::ws_connect(self.grant, &url, self.pin.as_deref(), self.timeouts).await?;
        let sent = socket.send(protocol::button_frame(button)).await;
        socket.close().await;
        sent
    }
}

/// Sends one request and waits for its reply, skipping a few unrelated messages.
async fn call(ws: &mut Ws, id: &str, uri: &str, payload: Value) -> Result<Value, DeviceError> {
    ws.send(protocol::request(id, uri, payload)).await?;
    for _ in 0..protocol::MAX_SKIPPED {
        let message = protocol::parse(&ws.recv().await?)?;
        if protocol::id_of(&message) != Some(id) {
            continue;
        }
        return protocol::reply(&message);
    }
    Err(protocol::unreadable())
}

fn check_device(device: &str) -> Result<(), DeviceError> {
    if device == crate::TV_ID {
        Ok(())
    } else {
        Err(DeviceError::Disabled(crate::not_this_tv(device)))
    }
}

#[async_trait]
impl Driver for WebosDriver {
    fn poll_refusal(&self) -> Option<String> {
        if self.key_refused.load(Ordering::SeqCst) {
            return Some(KEY_REFUSED.to_string());
        }
        (!self.key_accepted.load(Ordering::SeqCst)).then(|| NOT_CONNECTED_YET.to_string())
    }

    async fn health(&self) -> Health {
        if self.key_refused.load(Ordering::SeqCst) {
            Health { status: HealthStatus::Down, detail: Some(KEY_REFUSED.to_string()) }
        } else {
            Health::unknown()
        }
    }

    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        check_device(device)?;
        match op {
            Op::TvPower { on: true } => {
                let wake =
                    self.wake.as_ref().ok_or_else(|| DeviceError::Disabled("wol_mac is not set.".to_string()))?;
                net::udp_broadcast(self.grant, wake.broadcast, &wake.packet).await?;
            }
            Op::TvPower { on: false } => {
                self.run("power", "ssap://system/turnOff", json!({})).await?;
            }
            Op::TvInput { input } => {
                let app_id = protocol::input_app(input)
                    .ok_or_else(|| DeviceError::Disabled(format!("{input} is not a known input.")))?;
                self.run("input", "ssap://system.launcher/launch", json!({ "id": app_id })).await?;
            }
            Op::TvKey { key } => {
                let button = protocol::button_name(key)
                    .ok_or_else(|| DeviceError::Disabled(format!("{key} is not a button on the remote.")))?;
                self.press(button).await?;
            }
            _ => return Err(DeviceError::Disabled(crate::CANNOT.to_string())),
        }
        Ok(CommandAck)
    }

    /// A TV that does not answer is an error, not a reading of off. An off TV and
    /// a hung or unplugged one look the same from here, and only one of them
    /// would make "the TV is off" true.
    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        check_device(device)?;
        let off = State::Tv { on: false, input: String::new() };
        let mut ws = self.open().await?;
        let power = call(&mut ws, "power", "ssap://com.webos.service.tvpower/power/getPowerState", json!({})).await;
        let on = match &power {
            Ok(reply) => protocol::is_on(reply),
            // Firmware without this call only answers at all when it is on.
            Err(DeviceError::Rejected(_)) => true,
            Err(_) => {
                ws.close().await;
                return Err(power.err().unwrap_or(DeviceError::Timeout));
            }
        };
        if !on {
            ws.close().await;
            return Ok(off);
        }
        let app = call(&mut ws, "app", "ssap://com.webos.applicationManager/getForegroundAppInfo", json!({})).await;
        ws.close().await;
        let input = app?.get("appId").and_then(Value::as_str).map(protocol::input_name).unwrap_or_default();
        Ok(State::Tv { on: true, input })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn an_address_counts_if_the_configured_name_stands_for_it() {
        let config = WebosConfig {
            cert_sha256: None,
            url: "ws://localhost:3100".to_string(),
            wol_mac: None,
            wol_broadcast: None,
        };
        let timeouts = Timeouts { connect: Duration::from_secs(3), read: Duration::from_secs(3) };
        let grant = LiveGrant::when_live(true).unwrap();
        let driver = WebosDriver::new(grant, &config, "key".to_string().into(), timeouts).unwrap();
        let url = driver.pointer_url("ws://127.0.0.1:3100/resources/a/netinput.pointer.sock").await.unwrap();
        assert_eq!(
            url.as_str(),
            "ws://localhost:3100/resources/a/netinput.pointer.sock",
            "only the path comes from the TV"
        );
        assert!(driver.pointer_url("ws://127.0.0.2:3100/resources/a").await.is_err());
        assert!(driver.pointer_url("ws://localhost:3101/resources/a").await.is_err());
    }
}

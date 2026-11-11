//! The live driver: today's adapter body, reading each key from its secret slot.
//!
//! A socket allows very few connections, so each exchange connects, does one thing
//! and closes, one at a time per socket. A key is only ever held as its cipher.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use swifty_net::{IpAddr, SocketAddr};

use aes::Aes128;
use async_trait::async_trait;
use swifty_plugin_api::net::{self, LiveGrant, TcpLink, Timeouts};
use swifty_plugin_api::{CommandAck, DeviceError, Driver, Op, Secrets, State};
use tokio::sync::Mutex;

use crate::config::{PlugConfig, TuyaConfig};
use crate::protocol::{
    bad_frame, body_length, build_frame, cipher_from_stored, control_payload, is_on, parse_frame, query_payload,
    reply_json, unreadable, Frame, CONTROL, DP_QUERY, HEADER_LEN, MAIN_SWITCH, MAX_FRAMES,
};

/// One poll asks for the socket and for its USB port. Both answers come from one
/// reading, because a socket allows so few connections.
const READING_STAYS_FRESH: Duration = Duration::from_millis(1500);

pub(crate) fn now_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

pub(crate) fn plug_address(plug: &PlugConfig) -> Option<SocketAddr> {
    plug.address.parse().ok().map(|ip| SocketAddr::new(IpAddr::V4(ip), plug.port))
}

/// Why this socket cannot be used because of its key. Fixed text: never the key.
pub(crate) fn bad_key(target: &str) -> String {
    format!("The local key for {target} is missing, or is not a base64 key of 16 bytes.")
}

struct Plug {
    id: String,
    address: Option<SocketAddr>,
    /// `Err` holds why this one socket cannot be used. The others still can.
    cipher: Result<Aes128, String>,
    /// Held for a whole connection, because a socket allows so few of them.
    one_at_a_time: Mutex<()>,
    /// The last status reading and when it arrived.
    recent: Mutex<Option<(Instant, Vec<u8>)>>,
}

/// One switch on a socket: the socket itself, or its USB port.
struct Channel {
    plug: Arc<Plug>,
    point: u32,
}

/// A live connection to one instance's sockets.
pub struct TuyaDriver {
    grant: LiveGrant,
    timeouts: Timeouts,
    channels: HashMap<String, Channel>,
}

impl TuyaDriver {
    /// Builds the driver. A socket whose key is missing or malformed is disabled
    /// on its own; the others still work.
    pub fn new(grant: LiveGrant, config: &TuyaConfig, secrets: &Secrets, timeouts: Timeouts) -> Self {
        let mut channels = HashMap::new();
        for plug in &config.plugs {
            let cipher = secrets
                .get(&plug.key_slot())
                .and_then(|stored| cipher_from_stored(stored.expose()))
                .ok_or_else(|| bad_key(&plug.target()));
            let shared = Arc::new(Plug {
                id: plug.id.clone(),
                address: plug_address(plug),
                cipher,
                one_at_a_time: Mutex::new(()),
                recent: Mutex::new(None),
            });
            channels.insert(plug.target(), Channel { plug: shared.clone(), point: MAIN_SWITCH });
            if let (Some(usb), Some(point)) = (plug.usb_target(), plug.usb_dp) {
                channels.insert(usb, Channel { plug: shared, point: u32::from(point) });
            }
        }
        Self { grant, timeouts, channels }
    }

    fn channel(&self, target: &str) -> Result<(&Channel, &Aes128), DeviceError> {
        let channel = self
            .channels
            .get(target)
            .ok_or_else(|| DeviceError::Disabled(format!("{target} is not a known socket.")))?;
        match &channel.plug.cipher {
            Ok(cipher) => Ok((channel, cipher)),
            Err(reason) => Err(DeviceError::Disabled(reason.clone())),
        }
    }

    /// The socket's status, from a reading made a moment ago if there is one.
    async fn status(&self, plug: &Plug, cipher: &Aes128) -> Result<Vec<u8>, DeviceError> {
        let mut recent = plug.recent.lock().await;
        if let Some((at, json)) = recent.as_ref() {
            if at.elapsed() < READING_STAYS_FRESH {
                return Ok(json.clone());
            }
        }
        let json = query(self.grant, self.timeouts, plug.address, &plug.one_at_a_time, &plug.id, cipher).await?;
        *recent = Some((Instant::now(), json.clone()));
        Ok(json)
    }
}

/// Asks a socket for its status once and returns the decrypted JSON.
pub(crate) async fn query(
    grant: LiveGrant,
    timeouts: Timeouts,
    address: Option<SocketAddr>,
    one_at_a_time: &Mutex<()>,
    id: &str,
    cipher: &Aes128,
) -> Result<Vec<u8>, DeviceError> {
    let reply =
        exchange(grant, timeouts, address, one_at_a_time, DP_QUERY, &query_payload(cipher, id, now_seconds())).await?;
    reply_json(cipher, &reply)?.ok_or_else(unreadable)
}

/// Connects, sends one frame, reads its answer and closes.
async fn exchange(
    grant: LiveGrant,
    timeouts: Timeouts,
    address: Option<SocketAddr>,
    one_at_a_time: &Mutex<()>,
    command: u32,
    payload: &[u8],
) -> Result<Vec<u8>, DeviceError> {
    let address = address.ok_or_else(|| DeviceError::Disabled("The socket's address is not valid.".to_string()))?;
    let _one_at_a_time = one_at_a_time.lock().await;
    let mut link = net::tcp_open(grant, address, timeouts).await?;
    link.send(&build_frame(1, command, payload)).await?;
    for _ in 0..MAX_FRAMES {
        let frame = read_frame(&mut link).await?;
        if frame.command == command {
            return Ok(frame.payload);
        }
    }
    Err(bad_frame())
}

/// Reads one frame, and never more than the header says, once the header has passed.
async fn read_frame(link: &mut TcpLink) -> Result<Frame, DeviceError> {
    let mut header = [0u8; HEADER_LEN];
    link.read_exact(&mut header).await?;
    let mut body = vec![0u8; body_length(&header)?];
    link.read_exact(&mut body).await?;
    parse_frame(&header, &body)
}

#[async_trait]
impl Driver for TuyaDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        let Op::PlugPower { on } = op else {
            return Err(DeviceError::Disabled("A socket cannot do that.".to_string()));
        };
        let (channel, cipher) = self.channel(device)?;
        let plug = &channel.plug;
        let payload = control_payload(cipher, &plug.id, channel.point, *on, now_seconds());
        let reply = exchange(self.grant, self.timeouts, plug.address, &plug.one_at_a_time, CONTROL, &payload).await?;
        *plug.recent.lock().await = None;
        reply_json(cipher, &reply)?;
        Ok(CommandAck)
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        let (channel, cipher) = self.channel(device)?;
        let json = self.status(&channel.plug, cipher).await?;
        Ok(State::Plug { on: is_on(&json, channel.point)? })
    }
}

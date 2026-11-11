//! The live driver: one RTSP probe per reading. Nothing can be sent to the camera.

use async_trait::async_trait;
use swifty_net::{self as net, LiveGrant, Timeouts};
use swifty_plugin_api::{CommandAck, DeviceError, Driver, Op, State};

use crate::config::CameraConfig;
use crate::protocol::{options_request, speaks_rtsp};
use crate::{DEVICE, READ_ONLY};

/// A live connection to one camera.
pub struct CameraDriver {
    grant: LiveGrant,
    host: String,
    port: u16,
    timeouts: Timeouts,
}

impl CameraDriver {
    /// The probe does not send the camera's login, so it is not needed here.
    /// `connect` still refuses to build a driver until the login is stored.
    pub fn new(grant: LiveGrant, config: &CameraConfig, timeouts: Timeouts) -> Self {
        Self { grant, host: config.host.clone(), port: config.port, timeouts }
    }
}

#[async_trait]
impl Driver for CameraDriver {
    async fn command(&self, _device: &str, _op: &Op) -> Result<CommandAck, DeviceError> {
        Err(DeviceError::Disabled(READ_ONLY.to_string()))
    }

    /// An RTSP OPTIONS request. Any RTSP status, even a refusal, shows the
    /// camera is up and speaking RTSP. Silence is an error, not a reading.
    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        if device != DEVICE {
            return Err(DeviceError::Disabled("A camera instance has one device, and this is not it.".to_string()));
        }
        let request = options_request(&self.host, self.port);
        let reply = net::tcp_exchange(self.grant, &self.host, self.port, request.as_bytes(), self.timeouts).await?;
        if speaks_rtsp(&reply) {
            Ok(State::Camera { streaming: true })
        } else {
            Err(DeviceError::Rejected("the camera did not answer in RTSP".to_string()))
        }
    }
}

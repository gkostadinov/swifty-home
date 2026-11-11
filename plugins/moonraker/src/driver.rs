//! The live driver: Klipper through Moonraker's HTTP API. State is read back from
//! `/printer/objects/query`.

use async_trait::async_trait;
use http::HeaderValue;
use serde_json::Value;
use swifty_net::{Http, LiveGrant, Reply, Timeouts};
use swifty_plugin_api::{CommandAck, DeviceError, Driver, Op, State};
use url::Url;

use crate::config::{MoonrakerConfig, PRINTER};
use crate::protocol::{self, KEY_HEADER, START_PATH, STATUS_PATH, STATUS_QUERY};

/// What this plugin does not do.
pub const NOT_A_PRINTER_OP: &str = "The printer cannot do that.";

/// Why a device that is not this instance's printer gets nothing.
pub fn not_here(device: &str) -> String {
    format!("This instance has no device {device}.")
}

/// Why this cannot go to the printer, if it cannot.
pub fn refusal(device: &str, op: &Op) -> Option<String> {
    if device != PRINTER {
        return Some(not_here(device));
    }
    match op {
        Op::PrintStart | Op::PrintPause | Op::PrintResume | Op::PrintEmergencyStop => None,
        _ => Some(NOT_A_PRINTER_OP.to_string()),
    }
}

/// What a live instance holds.
pub struct MoonrakerDriver {
    http: Http,
    /// Marked sensitive, so that no debug output of a request shows it.
    key_header: Option<HeaderValue>,
}

impl MoonrakerDriver {
    /// A driver for the configured printer. Nothing is sent yet.
    pub(crate) fn new(
        grant: LiveGrant,
        config: &MoonrakerConfig,
        key: Option<&str>,
        timeouts: Timeouts,
    ) -> Result<Self, String> {
        let key_header = match key {
            None => None,
            Some(key) => {
                let mut value =
                    HeaderValue::from_str(key).map_err(|_| "the Moonraker API key holds invalid text".to_string())?;
                value.set_sensitive(true);
                Some(value)
            }
        };
        Ok(Self { http: Http::new(grant, &config.url, timeouts, None)?, key_header })
    }

    /// Sends once, with the key as a header and never in the URL.
    async fn send(&self, post: bool, url: Url) -> Result<Reply, DeviceError> {
        let request = if post { self.http.post(url) } else { self.http.get(url) };
        let request = match &self.key_header {
            Some(key) => request.header(KEY_HEADER, key.clone()),
            None => request,
        };
        self.http.send(request).await
    }

    /// The printer's objects.
    pub(crate) async fn status(&self) -> Result<Value, DeviceError> {
        let mut url = self.http.url(STATUS_PATH)?;
        url.set_query(Some(STATUS_QUERY));
        protocol::status(&self.send(false, url).await?)
    }

    async fn post(&self, url: Url) -> Result<CommandAck, DeviceError> {
        let reply = self.send(true, url).await?;
        if !reply.status.is_success() {
            return Err(protocol::refused(&reply));
        }
        Ok(CommandAck)
    }

    /// Finding the file comes first, so a printer that cannot be read gets no
    /// command at all. The start itself is one POST. A printer that did not
    /// answer is still silence, so the engine sees it as such.
    async fn start(&self) -> Result<CommandAck, DeviceError> {
        let status = self.status().await.map_err(|error| {
            let why = format!("the printer could not be read: {error}. Nothing was sent.");
            if error.is_silence() {
                DeviceError::Unreachable(why)
            } else {
                DeviceError::Rejected(why)
            }
        })?;
        let filename = protocol::loaded_job(&status)?;
        let mut url = self.http.url(START_PATH)?;
        url.query_pairs_mut().append_pair("filename", &filename);
        self.post(url).await
    }
}

#[async_trait]
impl Driver for MoonrakerDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        if let Some(reason) = refusal(device, op) {
            return Err(DeviceError::Disabled(reason));
        }
        if *op == Op::PrintStart {
            return self.start().await;
        }
        let path = protocol::command_path(op).ok_or_else(|| DeviceError::Disabled(NOT_A_PRINTER_OP.to_string()))?;
        self.post(self.http.url(path)?).await
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        if device != PRINTER {
            return Err(DeviceError::Disabled(not_here(device)));
        }
        Ok(protocol::printer_state(&self.status().await?))
    }
}

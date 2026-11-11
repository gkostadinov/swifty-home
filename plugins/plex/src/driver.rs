//! The live driver: asks the server whether it is up and takes the token.
//! Nothing else can be sent to it.

use async_trait::async_trait;
use http::header::{HeaderValue, ACCEPT};
use swifty_net::{Http, LiveGrant, Timeouts};
use swifty_plugin_api::{CommandAck, DeviceError, Driver, Op, Secret, State};

use crate::config::PlexConfig;
use crate::{DEVICE, READ_ONLY};

/// The header Plex reads its token from. The token never goes in the URL.
pub const TOKEN_HEADER: &str = "x-plex-token";

/// Whether a reply is what Plex answers on `/` when asked for JSON: an object
/// holding a `MediaContainer` object. A reply that is cut short or spoiled is
/// not, so it is never taken for a server that is up.
pub fn is_media_container(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .is_ok_and(|reply| reply.get("MediaContainer").is_some_and(serde_json::Value::is_object))
}

/// A live connection to one Plex server.
pub struct PlexDriver {
    http: Http,
    token: HeaderValue,
}

impl PlexDriver {
    /// Builds the client. `connect` checks `config` first.
    pub fn new(grant: LiveGrant, config: &PlexConfig, token: &Secret, timeouts: Timeouts) -> Result<Self, String> {
        let mut token = HeaderValue::from_str(token.expose())
            .map_err(|_| "the Plex token holds characters a header cannot carry".to_string())?;
        token.set_sensitive(true);
        Ok(Self { http: Http::new(grant, &config.url, timeouts, None)?, token })
    }
}

#[async_trait]
impl Driver for PlexDriver {
    async fn command(&self, _device: &str, _op: &Op) -> Result<CommandAck, DeviceError> {
        Err(DeviceError::Disabled(READ_ONLY.to_string()))
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        if device != DEVICE {
            return Err(DeviceError::Disabled("A Plex instance has one device, and this is not it.".to_string()));
        }
        let request = self
            .http
            .get(self.http.url("/")?)
            .header(TOKEN_HEADER, self.token.clone())
            .header(ACCEPT, "application/json");
        let reply = self.http.send(request).await?;
        match reply.status.as_u16() {
            200..=299 if is_media_container(&reply.body) => Ok(State::Service { reachable: true }),
            200..=299 => Err(DeviceError::Rejected("the server's answer is not Plex's".to_string())),
            401 => Err(DeviceError::Rejected("Plex did not accept the token".to_string())),
            status => Err(DeviceError::Rejected(format!("Plex refused with HTTP {status}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_media_container_is_an_answer_from_plex() {
        assert!(is_media_container(br#"{"MediaContainer":{"friendlyName":"Plex"}}"#));
        assert!(!is_media_container(br#"{"MediaContainer":{"friendlyName":"Ple"#));
        assert!(!is_media_container(br#"{"MediaContainer":"no"}"#));
        assert!(!is_media_container(b"<html>a router's page</html>"));
        assert!(!is_media_container(b""));
    }
}

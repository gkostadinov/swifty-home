//! The live driver: one HTTP client per speaker, one SOAP action per call.
//!
//! A read-only speaker (a subwoofer has no AVTransport service) is only ever
//! asked for its RenderingControl values.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use swifty_net::{Http, LiveGrant, Timeouts};
use swifty_plugin_api::{CommandAck, DeviceError, Driver, Op, State};

use crate::config::SonosConfig;
use crate::protocol::{self, Service, INSTANCE, MASTER, NIGHT_MODE, PLAY};

/// Night mode is extra. Waiting on it must not cost the rest of the speaker's state.
const NIGHT_READ_WAIT: Duration = Duration::from_millis(1_500);

fn unreadable() -> DeviceError {
    DeviceError::Rejected("the speaker sent an unreadable reply".to_string())
}

/// Everything a speaker can be asked here. Each call sends one SOAP action.
pub(crate) struct Speaker {
    http: Http,
    read_only: bool,
}

impl Speaker {
    async fn soap(&self, service: Service, action: &str, arguments: &str) -> Result<String, DeviceError> {
        let request = self
            .http
            .post(self.http.url(service.path())?)
            .header("content-type", "text/xml; charset=\"utf-8\"")
            .header("soapaction", protocol::soap_action(service, action))
            .body(protocol::envelope(service, action, arguments));
        let reply = self.http.send(request).await?;
        if !reply.status.is_success() {
            return Err(DeviceError::Rejected(protocol::fault(reply.status.as_u16(), &reply.text())));
        }
        let text = reply.text();
        if !protocol::is_envelope(&text) {
            return Err(unreadable());
        }
        Ok(text)
    }

    async fn volume(&self) -> Result<u8, DeviceError> {
        let xml = self.soap(Service::Rendering, "GetVolume", MASTER).await?;
        protocol::volume(&xml).ok_or_else(unreadable)
    }

    async fn mute(&self) -> Result<bool, DeviceError> {
        let xml = self.soap(Service::Rendering, "GetMute", MASTER).await?;
        protocol::flag(&xml, "CurrentMute").ok_or_else(unreadable)
    }

    async fn set_volume(&self, level: u8) -> Result<(), DeviceError> {
        self.soap(Service::Rendering, "SetVolume", &protocol::set_volume(level)).await.map(|_| ())
    }

    async fn set_mute(&self, muted: bool) -> Result<(), DeviceError> {
        self.soap(Service::Rendering, "SetMute", &protocol::set_mute(muted)).await.map(|_| ())
    }

    async fn night(&self) -> Result<bool, DeviceError> {
        let xml = self.soap(Service::Rendering, "GetEQ", NIGHT_MODE).await?;
        protocol::flag(&xml, "CurrentValue").ok_or_else(unreadable)
    }

    /// `None` for a speaker that has no night mode, or that would not say in time.
    async fn night_if_known(&self) -> Option<bool> {
        tokio::time::timeout(NIGHT_READ_WAIT, self.night()).await.ok()?.ok()
    }

    async fn set_night(&self, on: bool) -> Result<(), DeviceError> {
        self.soap(Service::Rendering, "SetEQ", &protocol::set_night(on)).await.map(|_| ())
    }

    async fn playing(&self) -> Result<bool, DeviceError> {
        let xml = self.soap(Service::Transport, "GetTransportInfo", INSTANCE).await?;
        protocol::playing(&xml).ok_or_else(unreadable)
    }

    async fn source(&self) -> Result<String, DeviceError> {
        let xml = self.soap(Service::Transport, "GetMediaInfo", INSTANCE).await?;
        Ok(protocol::source(&xml).to_string())
    }
}

/// The speakers of one instance.
pub struct SonosDriver {
    speakers: BTreeMap<String, Speaker>,
}

impl SonosDriver {
    /// One client per speaker in the config. Opens nothing yet.
    pub fn new(grant: LiveGrant, config: &SonosConfig, timeouts: Timeouts) -> Result<Self, String> {
        let mut speakers = BTreeMap::new();
        for speaker in &config.speakers {
            let http = Http::new(grant, &speaker.url, timeouts, None)?;
            speakers.insert(speaker.id.clone(), Speaker { http, read_only: speaker.read_only });
        }
        Ok(Self { speakers })
    }

    fn speaker(&self, device: &str) -> Result<&Speaker, DeviceError> {
        self.speakers.get(device).ok_or_else(|| DeviceError::Disabled(crate::not_a_speaker(device)))
    }

    /// The speaker, if it may be commanded at all.
    fn writable(&self, device: &str) -> Result<&Speaker, DeviceError> {
        let speaker = self.speaker(device)?;
        if speaker.read_only {
            return Err(DeviceError::Disabled(crate::READ_ONLY.to_string()));
        }
        Ok(speaker)
    }
}

#[async_trait]
impl Driver for SonosDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        let speaker = self.writable(device)?;
        match op {
            Op::Volume { level } => speaker.set_volume(*level).await?,
            Op::VolumeCap { max } => {
                if speaker.volume().await? > *max {
                    speaker.set_volume(*max).await?;
                }
            }
            Op::Mute { muted } => speaker.set_mute(*muted).await?,
            Op::NightMode { on } => speaker.set_night(*on).await?,
            Op::Play => {
                speaker.soap(Service::Transport, "Play", PLAY).await?;
            }
            Op::Pause => {
                speaker.soap(Service::Transport, "Pause", INSTANCE).await?;
            }
            _ => return Err(DeviceError::Disabled(crate::CANNOT.to_string())),
        }
        Ok(CommandAck)
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        let speaker = self.speaker(device)?;
        if speaker.read_only {
            let (vol, mute) = tokio::try_join!(speaker.volume(), speaker.mute())?;
            return Ok(State::Speaker { vol, mute, playing: false, source: "Sub".to_string(), night: None });
        }
        let required =
            async { tokio::try_join!(speaker.volume(), speaker.mute(), speaker.playing(), speaker.source()) };
        let (required, night) = tokio::join!(required, speaker.night_if_known());
        let (vol, mute, playing, source) = required?;
        Ok(State::Speaker { vol, mute, playing, source, night })
    }
}

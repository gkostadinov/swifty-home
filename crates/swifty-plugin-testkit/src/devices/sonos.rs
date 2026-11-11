//! Sonos speakers: UPnP over HTTP.

use std::sync::{Arc, Mutex};

use super::http::{MockHttp, Seen};

// ----- Sonos

#[derive(Debug, Clone)]
pub struct SpeakerState {
    pub volume: u64,
    pub muted: bool,
    pub transport: String,
    pub uri: String,
    /// Answered as a UPnP fault to the next call that changes something.
    pub fault: Option<u32>,
    /// False: commands are accepted and change nothing.
    pub apply: bool,
    pub night: bool,
    /// Answers GetEQ with a fault, as a speaker with no night mode does.
    pub eq_refused: bool,
}

pub struct MockSonos {
    pub http: MockHttp,
    pub state: Arc<Mutex<SpeakerState>>,
}

fn soap_reply(action: &str, body: &str) -> (u16, String) {
    (
        200,
        format!(
            "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>\
             <u:{action}Response>{body}</u:{action}Response></s:Body></s:Envelope>"
        ),
    )
}

fn soap_fault(code: u32) -> (u16, String) {
    (
        500,
        format!("<s:Envelope><s:Body><s:Fault><detail><UPnPError><errorCode>{code}</errorCode></UPnPError></detail></s:Fault></s:Body></s:Envelope>"),
    )
}

/// The SOAP action a request names, without its service.
fn soap_action(seen: &Seen) -> String {
    seen.header("soapaction").trim_matches('"').rsplit('#').next().unwrap_or("").to_string()
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    Some(&text[start..start + text[start..].find(close)?])
}

impl MockSonos {
    /// A Sub is the same device with no AVTransport service.
    pub async fn start(has_transport: bool) -> Self {
        let state = Arc::new(Mutex::new(SpeakerState {
            volume: 24,
            muted: false,
            transport: "PLAYING".to_string(),
            uri: "x-sonos-htastream:RINCON_0:spdif".to_string(),
            fault: None,
            apply: true,
            night: false,
            eq_refused: false,
        }));
        let shared = state.clone();
        let http = MockHttp::start_rigged(move |seen, switches| {
            let mut speaker = shared.lock().unwrap();
            let action = soap_action(seen);
            let is_transport = seen.path == "/MediaRenderer/AVTransport/Control";
            if is_transport && !has_transport {
                return (404, String::new());
            }
            if !is_transport && seen.path != "/MediaRenderer/RenderingControl/Control" {
                return (404, String::new());
            }
            if action.starts_with("Set") || matches!(action.as_str(), "Play" | "Pause") {
                if let Some(code) = speaker.fault.take() {
                    return soap_fault(code);
                }
            }
            let apply = speaker.apply;
            match action.as_str() {
                "GetVolume" => soap_reply(&action, &format!("<CurrentVolume>{}</CurrentVolume>", speaker.volume)),
                "GetMute" => soap_reply(&action, &format!("<CurrentMute>{}</CurrentMute>", u8::from(speaker.muted))),
                "GetTransportInfo" => soap_reply(
                    &action,
                    &format!(
                        "<CurrentTransportState>{}</CurrentTransportState><CurrentSpeed>1</CurrentSpeed>",
                        speaker.transport
                    ),
                ),
                "GetMediaInfo" => {
                    let uri = switches.device_text(&speaker.uri);
                    soap_reply(&action, &format!("<NrTracks>0</NrTracks><CurrentURI>{uri}</CurrentURI>"))
                }
                "GetEQ" => {
                    if speaker.eq_refused || !seen.body.contains("<EQType>NightMode</EQType>") {
                        return soap_fault(401);
                    }
                    soap_reply(&action, &format!("<CurrentValue>{}</CurrentValue>", u8::from(speaker.night)))
                }
                "SetEQ" => {
                    let on = between(&seen.body, "<DesiredValue>", "</DesiredValue>").unwrap() == "1";
                    if apply {
                        speaker.night = on;
                    }
                    soap_reply(&action, "")
                }
                "SetVolume" => {
                    let level = between(&seen.body, "<DesiredVolume>", "</DesiredVolume>").unwrap().parse().unwrap();
                    if apply {
                        speaker.volume = level;
                    }
                    soap_reply(&action, "")
                }
                "SetMute" => {
                    let muted = between(&seen.body, "<DesiredMute>", "</DesiredMute>").unwrap() == "1";
                    if apply {
                        speaker.muted = muted;
                    }
                    soap_reply(&action, "")
                }
                "Play" => {
                    if apply {
                        speaker.transport = "PLAYING".to_string();
                    }
                    soap_reply(&action, "")
                }
                "Pause" => {
                    if apply {
                        speaker.transport = "PAUSED_PLAYBACK".to_string();
                    }
                    soap_reply(&action, "")
                }
                _ => soap_fault(401),
            }
        })
        .await;
        // A `Get...` action reads; every other one asks the speaker to do something.
        http.count_commands(|seen| {
            let action = soap_action(seen);
            !action.is_empty() && !action.starts_with("Get")
        });
        Self { http, state }
    }

    pub fn actions(&self) -> Vec<String> {
        self.http.requests().iter().map(soap_action).collect()
    }
}

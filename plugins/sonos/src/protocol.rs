//! UPnP SOAP as a Sonos speaker speaks it: the envelopes sent, and what is read
//! from the replies. Pure: nothing here touches the network.

use swifty_net::device_text;

/// The two UPnP services used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// Volume, mute and the EQ settings, night mode among them.
    Rendering,
    /// Play, pause and what is playing. A subwoofer has no such service.
    Transport,
}

impl Service {
    /// The service type, as the SOAP action names it.
    pub fn urn(self) -> &'static str {
        match self {
            Service::Rendering => "urn:schemas-upnp-org:service:RenderingControl:1",
            Service::Transport => "urn:schemas-upnp-org:service:AVTransport:1",
        }
    }

    /// The control path on the speaker.
    pub fn path(self) -> &'static str {
        match self {
            Service::Rendering => "/MediaRenderer/RenderingControl/Control",
            Service::Transport => "/MediaRenderer/AVTransport/Control",
        }
    }
}

/// The arguments that pick the master volume channel.
pub const MASTER: &str = "<InstanceID>0</InstanceID><Channel>Master</Channel>";
/// The arguments of a transport call.
pub const INSTANCE: &str = "<InstanceID>0</InstanceID>";
/// The arguments that pick the night mode EQ setting.
pub const NIGHT_MODE: &str = "<InstanceID>0</InstanceID><EQType>NightMode</EQType>";
/// The arguments of `Play`.
pub const PLAY: &str = "<InstanceID>0</InstanceID><Speed>1</Speed>";

/// The SOAP envelope for one action.
pub fn envelope(service: Service, action: &str, arguments: &str) -> String {
    let urn = service.urn();
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action} xmlns:u=\"{urn}\">{arguments}</u:{action}></s:Body></s:Envelope>"
    )
}

/// The `soapaction` header for one action.
pub fn soap_action(service: Service, action: &str) -> String {
    format!("\"{}#{action}\"", service.urn())
}

/// The arguments of `SetVolume`.
pub fn set_volume(level: u8) -> String {
    format!("{MASTER}<DesiredVolume>{level}</DesiredVolume>")
}

/// The arguments of `SetMute`.
pub fn set_mute(muted: bool) -> String {
    format!("{MASTER}<DesiredMute>{}</DesiredMute>", u8::from(muted))
}

/// The arguments of `SetEQ` for night mode.
pub fn set_night(on: bool) -> String {
    format!("{NIGHT_MODE}<DesiredValue>{}</DesiredValue>", u8::from(on))
}

/// Whether a reply is one whole SOAP envelope: it ends with the envelope's
/// closing tag. A reply that was cut short, spoiled, or followed by anything is
/// not read at all, so no part of it can pass for an answer.
pub fn is_envelope(xml: &str) -> bool {
    let xml = xml.trim();
    xml.starts_with('<')
        && xml.ends_with("Envelope>")
        && xml.rfind("</").is_some_and(|close| {
            let tag = &xml[close + 2..xml.len() - 1];
            tag == "Envelope"
                || tag.split_once(':').is_some_and(|(prefix, name)| name == "Envelope" && !prefix.is_empty())
        })
}

/// The text of the first `<name>` element.
pub fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)?;
    Some(&xml[start..start + end])
}

/// A volume from 0 to 100.
pub fn volume(xml: &str) -> Option<u8> {
    element(xml, "CurrentVolume").and_then(|v| v.parse::<u8>().ok()).filter(|v| *v <= 100)
}

/// A UPnP boolean, which is `1` or `0`.
pub fn flag(xml: &str, name: &str) -> Option<bool> {
    match element(xml, name) {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

/// Whether the transport is playing.
pub fn playing(xml: &str) -> Option<bool> {
    element(xml, "CurrentTransportState").map(|state| state == "PLAYING")
}

/// What is playing, from the current URI: the TV, a line input, music or nothing.
pub fn source(xml: &str) -> &'static str {
    match element(xml, "CurrentURI").unwrap_or_default() {
        "" => "None",
        uri if uri.starts_with("x-sonos-htastream:") => "TV",
        uri if uri.starts_with("x-rincon-stream:") => "Line-in",
        _ => "Music",
    }
}

/// Why the speaker refused: its UPnP error code, or the HTTP status.
pub fn fault(status: u16, body: &str) -> String {
    match element(body, "errorCode") {
        Some(code) => format!("the speaker refused with UPnP error {}", device_text(code)),
        None => format!("the speaker refused with HTTP {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_whole_envelope_is_a_reply() {
        let whole = "<s:Envelope><s:Body><u:GetVolumeResponse><CurrentVolume>24</CurrentVolume></u:GetVolumeResponse></s:Body></s:Envelope>";
        assert!(is_envelope(whole));
        assert!(is_envelope(&format!(
            "<?xml version=\"1.0\"?>{whole}
"
        )));
        assert!(!is_envelope(&whole[..whole.len() / 2]), "cut short");
        assert!(!is_envelope(&format!("{whole}xxxx")), "followed by more");
        assert!(!is_envelope("<CurrentVolume>24</CurrentVolume>"), "no envelope");
        assert!(!is_envelope(""));
    }

    #[test]
    fn replies_are_read_strictly() {
        assert_eq!(volume("<CurrentVolume>24</CurrentVolume>"), Some(24));
        assert_eq!(volume("<CurrentVolume>101</CurrentVolume>"), None);
        assert_eq!(volume("<CurrentVolume>-1</CurrentVolume>"), None);
        assert_eq!(volume("<CurrentVolume>24"), None);
        assert_eq!(flag("<CurrentMute>1</CurrentMute>", "CurrentMute"), Some(true));
        assert_eq!(flag("<CurrentMute>true</CurrentMute>", "CurrentMute"), None);
        assert_eq!(source("<CurrentURI>x-sonos-htastream:RINCON_0:spdif</CurrentURI>"), "TV");
        assert_eq!(source("<CurrentURI>x-rincon-stream:RINCON_0</CurrentURI>"), "Line-in");
        assert_eq!(source("<CurrentURI></CurrentURI>"), "None");
        assert_eq!(source("<NrTracks>0</NrTracks>"), "None");
        assert_eq!(source("<CurrentURI>x-sonos-spotify:abc</CurrentURI>"), "Music");
        assert_eq!(fault(500, "<errorCode>701</errorCode>"), "the speaker refused with UPnP error 701");
        assert_eq!(fault(500, "<errorCode>7\u{1b}01</errorCode>"), "the speaker refused with UPnP error 701");
        assert_eq!(fault(404, ""), "the speaker refused with HTTP 404");
    }

    #[test]
    fn an_envelope_names_its_action_and_service() {
        let body = envelope(Service::Rendering, "SetVolume", &set_volume(41));
        assert!(body.contains("<u:SetVolume xmlns:u=\"urn:schemas-upnp-org:service:RenderingControl:1\">"));
        assert!(body.contains("<DesiredVolume>41</DesiredVolume>"));
        assert_eq!(soap_action(Service::Transport, "Play"), "\"urn:schemas-upnp-org:service:AVTransport:1#Play\"");
    }
}

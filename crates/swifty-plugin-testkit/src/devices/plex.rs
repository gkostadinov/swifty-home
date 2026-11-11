//! A Plex server's reachability endpoint.

use serde_json::json;

use super::http::Seen;
use crate::conformance::Switches;

pub const PLEX_TOKEN: &str = "plex-token-0123456789";

pub fn plex_handler(seen: &Seen) -> (u16, String) {
    plex_rigged(seen, &Switches::default())
}

/// [`plex_handler`], reporting the conformance suite's text as the server's name.
pub fn plex_rigged(seen: &Seen, switches: &Switches) -> (u16, String) {
    if seen.header("x-plex-token") == PLEX_TOKEN {
        (200, json!({ "MediaContainer": { "friendlyName": switches.device_text("MyCloud") } }).to_string())
    } else {
        (401, String::new())
    }
}

//! Stand-ins for the real devices. Every one binds a loopback address and nothing else,
//! counts each connection or datagram that reaches it, and stops when dropped.
//!
//! One module per kind of device, plus the generic [`MockHttp`], [`MockUdp`],
//! [`MockTcp`] and [`MockWs`], and TLS servers on a chosen certificate
//! ([`MockTlsHttp`], [`MockTlsWs`]). Everything is re-exported here, so
//! `use swifty_plugin_testkit::devices::*` brings in all of it.

pub mod collector;
pub mod generic;
pub mod http;
pub mod hue;
pub mod moonraker;
pub mod petkit;
pub mod plex;
pub mod proxmox;
pub mod rtsp;
pub mod sonos;
pub mod tls;
pub mod toshiba;
pub mod tuya;
pub mod webos;
pub mod wiz;

pub use collector::*;
pub use generic::*;
pub use http::*;
pub use hue::*;
pub use moonraker::*;
pub use petkit::*;
pub use plex::*;
pub use proxmox::*;
pub use rtsp::*;
pub use sonos::*;
pub use tls::*;
pub use toshiba::*;
pub use tuya::*;
pub use webos::*;
pub use wiz::*;

/// Where every mock binds: loopback, on a port the system picks.
pub(crate) const LOOPBACK: &str = "127.0.0.1:0";

/// Every credential the mocks accept. None of them may ever appear in an error,
/// a log line, a state or a response.
pub const ALL_SECRETS: [&str; 11] = [
    HUE_KEY,
    PLEX_TOKEN,
    PROXMOX_TOKEN,
    COLLECTOR_TOKEN,
    MOONRAKER_KEY,
    TV_KEY,
    CAMERA_LOGIN,
    TUYA_KEY_BASE64,
    TOSHIBA_PASSWORD,
    TOSHIBA_BEARER,
    TOSHIBA_SIGNATURE,
];

// Each mock's `conformance::MockDevice` implementation lives in `conformance/mocks.rs`.

//! The shared test kit of Swifty Home: loopback stand-ins for real devices,
//! trap listeners that prove nothing was contacted, and the conformance suite
//! every plugin runs.
//!
//! Only tests use this crate. It is never part of the hub, so the hub's lint that
//! only `swifty-net` opens sockets does not look at it.
//!
//! - [`devices`]: one mock per kind of device (`wiz`, `hue`, `sonos`, `webos`,
//!   `moonraker`, `proxmox`, `tuya`, `toshiba`, `petkit`, `rtsp`, `plex`,
//!   `collector`), and the generic `MockHttp`, `MockUdp`, `MockTcp`, `MockWs`.
//! - [`trap`]: addresses that count every connection and datagram, and answer nothing.
//! - [`conformance`]: `Fixture`, `MockDevice` and `run`.

pub mod conformance;
pub mod devices;
pub mod trap;

//! swifty-hub: the Swifty Home hub as a library. The `swifty` binary runs it.
//!
//! Requests reach devices through exactly one path: a named entry in the fixed
//! [`catalog`], checked by [`policy`], executed by [`actions`], and dispatched
//! through the [`runtime::Registry`] to the plugin instance that carries the
//! target. There is no generic command, proxy or passthrough anywhere in this
//! crate. The one thing that moves raw bytes is the console relay, which carries a
//! single approved VM's VNC stream after a passkey step-up and goes nowhere else.

pub mod actions;
pub mod admin;
pub mod agent;
pub mod app;
pub mod assistant;
pub mod audit;
pub mod background;
pub mod catalog;
pub mod clock;
pub mod config;
pub mod console;
pub mod crypto;
pub mod domains;
pub mod energy;
pub mod error;
pub mod events;
pub mod health;
pub mod history;
pub mod http;
pub mod inventory;
pub mod memory;
pub mod migrate;
pub mod notes;
pub mod passkeys;
pub mod plugin_actions;
pub mod policy;
pub mod rooms;
pub mod runtime;
pub mod scenes;
pub mod secrets;
pub mod sessions;
pub mod setup;
pub mod store;
pub mod users;
pub mod voice;

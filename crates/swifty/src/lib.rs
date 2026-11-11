//! The library side of the `swifty` binary. The hub itself is the `swifty-hub`
//! crate; this crate adds the command line, the plugin template behind
//! `swifty plugin new`, and the list of plugins compiled into this build.
//!
//! The binary executes no other process and reads no environment variable.

pub mod cli;
pub mod plugins;
pub mod privileges;
pub mod scaffold;

//! TOML configuration.
//!
//! - [`v2`]: `swifty.toml`, version 2. What the process needs before it can open
//!   the database, and operator policy. Plugin instances live in the database and
//!   their credentials in the secret store ([`crate::secrets`]). The hub runs on it,
//!   and its names are the ones this module exports.
//! - [`legacy`]: glimdock-home's `config.toml`, version 1, frozen. Only `swifty
//!   migrate` reads it: [`crate::migrate::map_legacy`] turns it into version 2.
//! - [`shared`]: the sections both versions read the same way.
//!
//! Nothing is read from the environment.

pub mod legacy;
pub mod shared;
pub mod v2;

pub use shared::*;
pub use v2::*;

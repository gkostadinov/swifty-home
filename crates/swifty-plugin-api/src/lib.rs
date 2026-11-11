//! The contract between the Swifty Home hub and its plugins.
//!
//! A plugin is one integration (a kind of light, a speaker, a hypervisor). It
//! describes itself with a static [`Manifest`], checks its configuration, names
//! the devices it offers, and, only in a hub that is live, builds a [`Driver`]
//! that sends one [`Op`] at a time and reads a [`State`] back.
//!
//! The rules this crate enforces:
//!
//! 1. **The hub decides, the plugin drives.** A plugin never sees a role, a tier,
//!    a session or the audit log. It gets one validated [`Op`] for one device and
//!    says "accepted" or why not.
//! 2. **Confirmation belongs to the hub.** [`Op::is_confirmed_by`] and [`Expect`]
//!    live here and are evaluated by the engine on what
//!    [`Driver::read_state`] returns. A plugin cannot say "it worked".
//! 3. **Four calls are pure**: [`Plugin::validate`], [`Plugin::readiness`],
//!    [`Plugin::devices`] and [`Plugin::refusal`]. They take config, the names of
//!    the secret slots that hold a value, and the adopted devices. No socket,
//!    file, clock or secret value. They are all a dry-run hub ever calls.
//! 4. **Only [`Plugin::connect`], [`Driver`] and [`SetupFlow`] can reach the
//!    network**, and only with the [`net::LiveGrant`] the hub hands over. Only a
//!    hub whose config says `live = true` has one.
//! 5. **Everything is data.** The types that cross the boundary at run time
//!    serialise, and the manifest is `static` data that serialises, so the same
//!    contract can be spoken to a child process later.
//!
//! With the `openapi` feature, the types API v1 carries ([`Op`], [`State`],
//! [`Capability`], [`SetupStep`], [`Health`], [`Readiness`] and what they hold)
//! derive `utoipa::ToSchema`, so the hub's OpenAPI document describes them from
//! their own doc comments. The hub turns it on; a plugin never needs it.

#![deny(missing_docs)]

/// The socket helpers. Every one of them takes a [`net::LiveGrant`].
pub use swifty_net as net;

mod capability;
mod confirm;
mod device;
mod error;
mod examples;
mod machine;
mod manifest;
mod op;
mod plugin;
mod secret;
mod setup;
mod state;
mod tier;

pub use capability::{Audience, CapKind, Capability, DeviceClass};
pub use confirm::{Expect, MIN_LIGHT_DIMMING};
pub use device::{DeviceDecl, DeviceRecord};
pub use error::{ConfigProblem, DeviceError, PluginError};
pub use examples::Examples;
pub use machine::{ConsoleGrant, ExtraHost, HistoryRange, SeenMachine};
pub use manifest::{ConfigField, CustomAction, FieldKind, Manifest, NetUse, Reach, SecretSlot, API_VERSION};
pub use op::{Op, Scalar};
pub use plugin::{
    AgentPairing, CommandAck, Directory, Driver, Health, HealthStatus, InstanceView, LiveContext, MachineHost, Plugin,
    Readiness,
};
pub use secret::{Secret, Secrets};
pub use setup::{Choice, Field, FieldType, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep};
pub use state::State;
pub use tier::{ParamDef, ParamKind, Tier};

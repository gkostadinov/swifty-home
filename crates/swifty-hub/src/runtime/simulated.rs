//! What stands in for a device in dry-run.
//!
//! A dry-run registry never calls `Plugin::connect`, so it holds no driver and no
//! credential, and has no grant to open a socket with. Each instance is its
//! plugin's pure calls and nothing else:
//!
//! - availability comes from the plugin's `readiness`, so a device that live would
//!   list as waiting for setup is listed so in dry-run too;
//! - refusals come from the plugin's `refusal`, so dry-run refuses what live would;
//! - [`Registry::command`](super::Registry::command) answers
//!   [`Dispatched::Simulated`](super::Dispatched::Simulated), and reads, discovery,
//!   consoles, history and pairing answer `None`, each before anything is looked up.

/// What a dry-run outcome says happened: nothing.
pub const NOTHING_CONTACTED: &str = "Dry-run: no device was contacted.";

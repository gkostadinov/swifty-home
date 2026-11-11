//! The conformance suite every plugin runs, with this plugin's fixture: a
//! config that points at the stand-in device, the operations to try on it, and
//! the inputs that drive the setup flow to `Done`.
//!
//! `run_every_check` also fails when a check could not run, so a plugin made from
//! this template is held to all twelve from the start. The stand-in is rigged (see
//! `common/mod.rs`) so that the checks that spoil its replies and make it report a
//! hostile name have something to work with.

mod common;

use common::*;
{{#local}}
use swifty_plugin_testkit::conformance::{run_every_check, Fixture, Rigged};
{{/local}}
{{#cloud}}
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
{{/cloud}}

use {{lib}}::PLUGIN;

#[tokio::test]
async fn conformance() {
    let stand = Stand::start().await;
    let fixture = Fixture {
        config: stand.config(),
{{#local}}
        secrets: Vec::new(),
{{/local}}
{{#cloud}}
        secrets: vec![("token".to_string(), TOKEN.to_string())],
{{/cloud}}
        adopted: Vec::new(),
        ops: sample_ops(),
        setup: stand.setup_script(),
{{#local}}
        device: Box::new(Rigged::new(stand.device, stand.switches.clone())),
{{/local}}
{{#cloud}}
        device: Box::new(stand.device),
{{/cloud}}
    };
    run_every_check(&PLUGIN, fixture).await;
}

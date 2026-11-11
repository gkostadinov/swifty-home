//! `swifty device`: the devices of every integration, as the hub has them stored.
//! It reads the database and contacts nothing.

use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;
use serde_json::{json, Value};
use swifty_hub::runtime::Availability;

use super::plugin::{open_hub, Loaded};
use super::CliError;

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    /// List the devices: target, name, class, integration and status.
    List {
        /// Print JSON instead of text: what `GET /api/v1/devices` answers, with the
        /// devices that wait for approval as well.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(config: &Path, command: DeviceCommand) -> Result<ExitCode, CliError> {
    match command {
        DeviceCommand::List { json } => list(config, json),
    }
}

/// Every device, as `GET /api/v1/devices` lists it for the owner, and the ones that
/// wait for approval, marked `pending`.
pub fn devices_document(loaded: &Loaded) -> Value {
    let index = loaded.registry.index();
    let devices: Vec<Value> = index
        .all()
        .map(|device| {
            let (enabled, reason) = match loaded.registry.availability(&device.target) {
                Availability::Disabled(reason) => (false, Some(reason)),
                Availability::Enabled => (!device.pending, None),
            };
            json!({
                "target": device.target,
                "name": device.name,
                "class": device.class,
                "capabilities": device.capabilities,
                "plugin": device.plugin,
                "instance": device.instance,
                "audience": device.audience,
                "enabled": enabled,
                "reason": reason,
                "pending": device.pending,
            })
        })
        .collect();
    json!({ "live": loaded.live, "devices": devices })
}

fn list(config_path: &Path, json: bool) -> Result<ExitCode, CliError> {
    let hub = open_hub(config_path)?;
    let loaded = Loaded::read(&hub.config)?;
    let document = devices_document(&loaded);
    if json {
        println!("{}", serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?);
        return Ok(ExitCode::SUCCESS);
    }
    let mode = if loaded.live { "live: commands reach real devices" } else { "dry-run: no device is contacted" };
    println!("Devices ({mode})");
    println!();
    let devices = document["devices"].as_array().cloned().unwrap_or_default();
    if devices.is_empty() {
        println!("  None yet. Add an integration: sudo swifty plugin add <plugin>");
        println!("  The plugins in this build: swifty plugin list --available");
        return Ok(ExitCode::SUCCESS);
    }
    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
    let mut rows = vec![vec![
        "TARGET".to_string(),
        "NAME".to_string(),
        "CLASS".to_string(),
        "INTEGRATION".to_string(),
        "STATUS".to_string(),
    ]];
    for device in &devices {
        let status = if device["pending"] == true {
            "waiting for approval".to_string()
        } else if device["enabled"] == true {
            "ready".to_string()
        } else {
            format!("unavailable: {}", text(&device["reason"]))
        };
        rows.push(vec![
            text(&device["target"]),
            text(&device["name"]),
            text(&device["class"]),
            text(&device["instance"]),
            status,
        ]);
    }
    for line in super::plugin::table(&rows) {
        println!("{line}");
    }
    let waiting = devices.iter().filter(|device| device["pending"] == true).count();
    if waiting > 0 {
        println!();
        println!("A device waiting for approval is never contacted until the owner approves it in the app.");
    }
    Ok(ExitCode::SUCCESS)
}

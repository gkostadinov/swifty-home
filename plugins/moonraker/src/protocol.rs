//! What is said to Moonraker and how its replies are read. Pure: no socket, no
//! clock, no secret.

use serde_json::Value;
use swifty_net::{self as net, Reply};
use swifty_plugin_api::{DeviceError, Op, State};

/// Where the printer's objects are read.
pub const STATUS_PATH: &str = "/printer/objects/query";
/// The objects read, and nothing else.
pub const STATUS_QUERY: &str = "webhooks&print_stats&extruder&heater_bed&virtual_sdcard";
/// Starts the job that is loaded, by its file name.
pub const START_PATH: &str = "/printer/print/start";
/// The header Moonraker reads its API key from.
pub const KEY_HEADER: &str = "x-api-key";

/// The endpoint for an operation that is one POST, if it is one.
pub fn command_path(op: &Op) -> Option<&'static str> {
    match op {
        Op::PrintPause => Some("/printer/print/pause"),
        Op::PrintResume => Some("/printer/print/resume"),
        Op::PrintEmergencyStop => Some("/printer/emergency_stop"),
        _ => None,
    }
}

/// A reply this plugin cannot make sense of.
pub fn unreadable() -> DeviceError {
    DeviceError::Rejected("the printer sent an unreadable reply".to_string())
}

/// What a refusal says, in Moonraker's words made safe, or by its status.
pub fn refused(reply: &Reply) -> DeviceError {
    let message =
        reply.json().ok().and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(net::device_text));
    DeviceError::Rejected(match message {
        Some(message) => format!("the printer said: {message}"),
        None => format!("the printer refused with HTTP {}", reply.status.as_u16()),
    })
}

/// The printer's objects from a status reply.
pub fn status(reply: &Reply) -> Result<Value, DeviceError> {
    if reply.status.as_u16() == 503 {
        return Err(DeviceError::Unreachable("Moonraker has lost contact with Klipper".to_string()));
    }
    if !reply.status.is_success() {
        return Err(refused(reply));
    }
    reply.json()?.pointer("/result/status").cloned().ok_or_else(unreadable)
}

fn text<'a>(status: &'a Value, pointer: &str) -> &'a str {
    status.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn number(status: &Value, pointer: &str) -> f64 {
    status.pointer(pointer).and_then(Value::as_f64).unwrap_or(0.0)
}

fn count(status: &Value, pointer: &str) -> u32 {
    status.pointer(pointer).and_then(Value::as_u64).unwrap_or(0).min(u64::from(u32::MAX)) as u32
}

/// The console's name for what Klipper reports.
pub fn state_name(status: &Value) -> &'static str {
    match text(status, "/webhooks/state") {
        "shutdown" => return "stopped",
        "error" => return "error",
        "startup" => return "starting",
        _ => {}
    }
    match text(status, "/print_stats/state") {
        "printing" => "printing",
        "paused" => "paused",
        "standby" | "complete" | "cancelled" => "idle",
        "error" => "error",
        _ => "unknown",
    }
}

/// The printer's state from its objects.
pub fn printer_state(status: &Value) -> State {
    State::Printer {
        state: state_name(status).to_string(),
        nozzle: number(status, "/extruder/temperature"),
        bed: number(status, "/heater_bed/temperature"),
        layer: count(status, "/print_stats/info/current_layer"),
        layers: count(status, "/print_stats/info/total_layer"),
        pct: number(status, "/virtual_sdcard/progress") * 100.0,
    }
}

/// The file Klipper has loaded, which the catalog's "start the loaded job" means.
pub fn loaded_job(status: &Value) -> Result<String, DeviceError> {
    if state_name(status) != "idle" {
        return Err(DeviceError::Rejected(format!(
            "the printer is {}, so a job cannot start. Nothing was sent.",
            state_name(status)
        )));
    }
    match text(status, "/print_stats/filename") {
        "" => Err(DeviceError::Rejected("no job is loaded on the printer. Nothing was sent.".to_string())),
        name => Ok(name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status(webhooks: &str, print: &str, filename: &str) -> Value {
        json!({
            "webhooks": { "state": webhooks },
            "print_stats": { "state": print, "filename": filename, "info": { "current_layer": 3, "total_layer": 9 } },
            "extruder": { "temperature": 210.5 },
            "heater_bed": { "temperature": 60.0 },
            "virtual_sdcard": { "progress": 0.25 },
        })
    }

    #[test]
    fn klipper_states_get_the_console_names() {
        let name = |w, p| state_name(&status(w, p, ""));
        assert_eq!(name("ready", "printing"), "printing");
        assert_eq!(name("ready", "paused"), "paused");
        assert_eq!(name("ready", "standby"), "idle");
        assert_eq!(name("ready", "complete"), "idle");
        assert_eq!(name("ready", "cancelled"), "idle");
        assert_eq!(name("ready", "error"), "error");
        assert_eq!(name("shutdown", "printing"), "stopped");
        assert_eq!(name("ready", "something new"), "unknown");
    }

    #[test]
    fn the_console_fields_are_filled() {
        let state = printer_state(&status("ready", "printing", "a.gcode"));
        assert_eq!(
            state,
            State::Printer { state: "printing".into(), nozzle: 210.5, bed: 60.0, layer: 3, layers: 9, pct: 25.0 }
        );
    }

    #[test]
    fn only_an_idle_printer_with_a_file_can_start() {
        assert_eq!(loaded_job(&status("ready", "standby", "a.gcode")).unwrap(), "a.gcode");
        assert!(loaded_job(&status("ready", "standby", "")).is_err());
        assert!(loaded_job(&status("ready", "printing", "a.gcode")).is_err());
        assert!(loaded_job(&status("shutdown", "standby", "a.gcode")).is_err());
    }
}

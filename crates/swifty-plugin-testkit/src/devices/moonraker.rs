//! A Klipper printer behind Moonraker.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::http::MockHttp;

pub const MOONRAKER_KEY: &str = "moonraker-key-0123456789";

// ----- Moonraker

#[derive(Debug, Clone)]
pub struct PrinterState {
    pub webhooks: String,
    pub print: String,
    pub filename: String,
    pub nozzle: f64,
    pub bed: f64,
    pub progress: f64,
    /// False: commands are accepted and change nothing.
    pub apply: bool,
    /// Answers 503, as Moonraker does when it has lost Klipper.
    pub klippy_gone: bool,
}

pub struct MockMoonraker {
    pub http: MockHttp,
    pub state: Arc<Mutex<PrinterState>>,
}

impl MockMoonraker {
    pub async fn start() -> Self {
        let state = Arc::new(Mutex::new(PrinterState {
            webhooks: "ready".to_string(),
            print: "standby".to_string(),
            filename: "benchy.gcode".to_string(),
            nozzle: 24.0,
            bed: 23.5,
            progress: 0.0,
            apply: true,
            klippy_gone: false,
        }));
        let shared = state.clone();
        let http = MockHttp::start_rigged(move |seen, switches| {
            let mut printer = shared.lock().unwrap();
            if printer.klippy_gone {
                return (503, json!({ "error": { "message": "Klippy Host not connected" } }).to_string());
            }
            let ok = json!({ "result": "ok" }).to_string();
            let apply = printer.apply;
            match (seen.method.as_str(), seen.path.as_str()) {
                ("GET", "/printer/objects/query") => {
                    let body = json!({ "result": { "eventtime": 1.0, "status": {
                        "webhooks": { "state": printer.webhooks },
                        "print_stats": {
                            "state": switches.device_text(&printer.print),
                            "filename": switches.device_text(&printer.filename),
                            "info": { "current_layer": 12, "total_layer": 90 },
                        },
                        "extruder": { "temperature": printer.nozzle },
                        "heater_bed": { "temperature": printer.bed },
                        "virtual_sdcard": { "progress": printer.progress },
                    }}});
                    (200, body.to_string())
                }
                ("POST", "/printer/print/start") => {
                    if printer.print == "printing" {
                        return (400, json!({ "error": { "message": "Printer is busy" } }).to_string());
                    }
                    if apply {
                        printer.print = "printing".to_string();
                    }
                    (200, ok)
                }
                ("POST", "/printer/print/pause") => {
                    if apply {
                        printer.print = "paused".to_string();
                    }
                    (200, ok)
                }
                ("POST", "/printer/print/resume") => {
                    if apply {
                        printer.print = "printing".to_string();
                    }
                    (200, ok)
                }
                ("POST", "/printer/emergency_stop") => {
                    if apply {
                        printer.webhooks = "shutdown".to_string();
                    }
                    (200, ok)
                }
                _ => (404, json!({ "error": { "message": "not found" } }).to_string()),
            }
        })
        .await;
        Self { http, state }
    }
}

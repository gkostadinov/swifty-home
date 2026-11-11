//! A Glimdock collector: the telemetry snapshot.

use serde_json::json;

use super::http::Seen;
use crate::conformance::Switches;

pub const COLLECTOR_TOKEN: &str = "collector-token-0123456789";

pub const SNAPSHOT_PATH: &str = "/api/snapshot";

pub fn collector_handler(seen: &Seen) -> (u16, String) {
    if seen.path == SNAPSHOT_PATH && seen.header("authorization") == format!("Bearer {COLLECTOR_TOKEN}") {
        (200, json!({ "pkg_temp_c": 64, "pkg_watts": 118 }).to_string())
    } else {
        (401, String::new())
    }
}

/// A collector whose snapshot also names a disk, with the conformance suite's
/// text as that name and as a key, as a node reports its disks by name.
pub fn collector_rigged(seen: &Seen, switches: &Switches) -> (u16, String) {
    if seen.path == SNAPSHOT_PATH && seen.header("authorization") == format!("Bearer {COLLECTOR_TOKEN}") {
        let disk = switches.device_text("nvme0");
        let snapshot = json!({
            "pkg_temp_c": 64,
            "pkg_watts": 118,
            "disks": [{ "name": disk, "health": "PASSED", "media_errors": 0 }],
            "storage": { disk.clone(): { "used_pct": 41 } },
        });
        (200, snapshot.to_string())
    } else {
        (401, String::new())
    }
}

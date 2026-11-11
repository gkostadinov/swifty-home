//! What is said to the Proxmox VE API and how its replies are read. Pure: no
//! socket, no clock, no secret.
//!
//! Only typed calls exist here. There is no way to reach a shell on a node:
//! `vncshell` and `termproxy` are not spoken at all, and the console is only
//! ever `qemu/{vmid}/vncproxy` for a VM on the approved list.

use std::ops::RangeInclusive;

use serde_json::{Map, Value};
use swifty_net::{self as net, Reply};
use swifty_plugin_api::{DeviceError, HistoryRange, SeenMachine, State};

/// The content type of a power command's body.
pub const FORM: &str = "application/x-www-form-urlencoded";
/// What Proxmox accepts as a VM id.
pub const VMIDS: RangeInclusive<u32> = 100..=999_999_999;
/// More VMs than one host could hold, so a host cannot fill the list with junk.
pub const MAX_LISTED_VMS: usize = 1024;

/// The history fields passed on. Anything else Proxmox sends stays here.
pub const RRD_FIELDS: &[&str] = &[
    "time",
    "cpu",
    "maxcpu",
    "mem",
    "maxmem",
    "memused",
    "memtotal",
    "disk",
    "maxdisk",
    "netin",
    "netout",
    "diskread",
    "diskwrite",
    "loadavg",
    "iowait",
    "swapused",
    "swaptotal",
    "rootused",
    "roottotal",
];

/// The path of one VM's endpoint on a node.
pub fn vm_path(node: &str, vmid: u32, leaf: &str) -> String {
    format!("/api2/json/nodes/{node}/qemu/{vmid}/{leaf}")
}

/// The path of one of the node's own endpoints.
pub fn node_path(node: &str, leaf: &str) -> String {
    format!("/api2/json/nodes/{node}/{leaf}")
}

/// The VM list of a node.
pub fn vm_list_path(node: &str) -> String {
    format!("/api2/json/nodes/{node}/qemu")
}

/// Answers on any node to a valid token, whatever its privileges.
pub const VERSION_PATH: &str = "/api2/json/version";

/// The body of a node power command.
pub fn node_power_form(reboot: bool) -> &'static str {
    if reboot {
        "command=reboot"
    } else {
        "command=shutdown"
    }
}

/// The body that asks `vncproxy` for a websocket console.
pub const VNC_FORM: &str = "websocket=1";

/// A reply this plugin cannot make sense of.
pub fn unreadable() -> DeviceError {
    DeviceError::Rejected("Proxmox sent an unreadable reply".to_string())
}

/// What a refusal from the API means, in words that never repeat the token.
pub fn refused(reply: &Reply) -> DeviceError {
    DeviceError::Rejected(match reply.status.as_u16() {
        401 => "Proxmox did not accept the API token".to_string(),
        403 => "the Proxmox API token is not allowed to do this".to_string(),
        status => format!("Proxmox refused with HTTP {status}"),
    })
}

/// The `data` of a successful reply.
fn data(reply: &Reply) -> Result<Value, DeviceError> {
    if !reply.status.is_success() {
        return Err(refused(reply));
    }
    reply.json()?.get_mut("data").map(Value::take).ok_or_else(unreadable)
}

/// Proxmox's name for a chart's time span.
pub fn timeframe(range: HistoryRange) -> &'static str {
    match range {
        HistoryRange::Hour => "hour",
        HistoryRange::Day => "day",
        HistoryRange::Week => "week",
    }
}

/// Proxmox sends some numbers as JSON strings.
pub fn as_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(number) => number.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// A VM's `status/current`.
pub fn vm_state(reply: &Reply) -> Result<State, DeviceError> {
    let data = data(reply)?;
    let status = data.get("status").and_then(Value::as_str).ok_or_else(unreadable)?;
    let paused = data.get("qmpstatus").and_then(Value::as_str) == Some("paused");
    Ok(State::Vm {
        state: if paused { "paused".to_string() } else { net::device_text(status) },
        uptime_s: data.get("uptime").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// The node's own `status`.
pub fn node_state(reply: &Reply) -> Result<State, DeviceError> {
    let data = data(reply)?;
    Ok(State::Host { up: true, uptime_s: data.get("uptime").and_then(Value::as_u64).unwrap_or(0) })
}

/// A node's VM list, without templates and without anything that is not a VM id.
/// `host` is the id the VMs are listed under.
pub fn machines(reply: &Reply, host: &str) -> Result<Vec<SeenMachine>, DeviceError> {
    let data = data(reply)?;
    let rows = data.as_array().ok_or_else(unreadable)?;
    let mut seen = Vec::with_capacity(rows.len().min(MAX_LISTED_VMS));
    for row in rows.iter().take(MAX_LISTED_VMS) {
        // A template cannot run, so it is not a machine to approve.
        if row.get("template").and_then(Value::as_i64) == Some(1) {
            continue;
        }
        let Some(vmid) = row.get("vmid").and_then(as_u32).filter(|vmid| VMIDS.contains(vmid)) else { continue };
        let name = row.get("name").and_then(Value::as_str).map(net::device_text).unwrap_or_default();
        seen.push(SeenMachine { host: host.to_string(), vmid, name });
    }
    Ok(seen)
}

/// One point with only the numeric fields this plugin knows about.
pub fn rrd_point(row: &Value) -> Option<Value> {
    let row = row.as_object()?;
    row.get("time")?.as_f64()?;
    let point: Map<String, Value> = RRD_FIELDS
        .iter()
        .filter_map(|field| {
            let value = row.get(*field)?;
            value.as_f64().map(|_| ((*field).to_string(), value.clone()))
        })
        .collect();
    Some(Value::Object(point))
}

/// An `rrddata` reply as chart points.
pub fn rrd_points(reply: &Reply) -> Result<Vec<Value>, DeviceError> {
    let data = data(reply)?;
    let rows = data.as_array().ok_or_else(unreadable)?;
    Ok(rows.iter().filter_map(rrd_point).collect())
}

/// The port and ticket `vncproxy` handed out. The ticket goes nowhere but the
/// console's own websocket.
pub fn console_offer(reply: &Reply) -> Result<(u16, String), DeviceError> {
    let data = data(reply)?;
    let port = data.get("port").and_then(as_u32).and_then(|p| u16::try_from(p).ok()).filter(|p| *p > 0);
    let ticket = data.get("ticket").and_then(Value::as_str).filter(|t| !t.is_empty());
    match (port, ticket) {
        (Some(port), Some(ticket)) => Ok((port, ticket.to_string())),
        _ => Err(unreadable()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(status: u16, body: Value) -> Reply {
        Reply { status: status.try_into().unwrap(), body: body.to_string().into_bytes(), retry_after: None }
    }

    #[test]
    fn history_passes_on_only_known_numbers() {
        let point = rrd_point(&json!({
            "time": 1, "cpu": 0.5, "maxcpu": 32, "name": "secret", "mem": "12", "extra": 9
        }))
        .unwrap();
        assert_eq!(point, json!({ "time": 1, "cpu": 0.5, "maxcpu": 32 }));
        assert!(rrd_point(&json!({ "cpu": 0.5 })).is_none());
    }

    #[test]
    fn a_paused_vm_reads_as_paused() {
        let paused = reply(200, json!({ "data": { "status": "running", "qmpstatus": "paused", "uptime": 9 } }));
        assert_eq!(vm_state(&paused).unwrap(), State::Vm { state: "paused".into(), uptime_s: 9 });
        assert!(matches!(vm_state(&reply(200, json!({ "data": {} }))), Err(DeviceError::Rejected(_))));
        assert!(matches!(vm_state(&reply(401, json!({}))), Err(DeviceError::Rejected(text)) if text.contains("token")));
    }

    #[test]
    fn the_vm_list_leaves_out_templates_and_odd_ids_and_cleans_names() {
        let listed = reply(
            200,
            json!({ "data": [
                { "vmid": 100, "name": "one\u{7}" },
                { "vmid": "101", "name": "two" },
                { "vmid": 102, "name": "template", "template": 1 },
                { "vmid": 99, "name": "too low" },
                { "name": "no id" },
            ] }),
        );
        let seen = machines(&listed, "lab").unwrap();
        let ids: Vec<(u32, &str, &str)> = seen.iter().map(|m| (m.vmid, m.name.as_str(), m.host.as_str())).collect();
        assert_eq!(ids, [(100, "one", "lab"), (101, "two", "lab")]);
    }

    #[test]
    fn a_console_offer_needs_a_port_and_a_ticket() {
        let offer = reply(200, json!({ "data": { "port": "5901", "ticket": "PVEVNC:x" } }));
        assert_eq!(console_offer(&offer).unwrap(), (5901, "PVEVNC:x".to_string()));
        for data in
            [json!({ "port": 0, "ticket": "t" }), json!({ "port": 5901, "ticket": "" }), json!({ "port": 70000 })]
        {
            assert!(console_offer(&reply(200, json!({ "data": data }))).is_err());
        }
    }
}

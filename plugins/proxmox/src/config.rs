//! The instance config and its checks, and the shape of a VM's local id.
//!
//! An instance is one Proxmox VE node reached through its API. The API token and
//! the CA certificate live in the secret slots `token` and `ca`, never here.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;
use url::{Host, Url};

/// The host named by the instance config, as a VM's `host` says it.
pub const PRIMARY_HOST: &str = "primary";
/// The local id of the node itself.
pub const HOST_DEVICE: &str = "host";
/// The slot that holds `user@realm!tokenid=uuid`.
pub const TOKEN_SLOT: &str = "token";
/// The slot that holds the PEM of the CA that signed the API certificate.
pub const CA_SLOT: &str = "ca";

/// What an instance is configured with.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProxmoxConfig {
    /// The API's base URL: `https://` and the host's address with its port.
    pub url: String,
    /// The node's name, as Proxmox shows it.
    pub node: String,
    /// The API certificate chains to a public root, so no CA has to be stored.
    #[serde(default)]
    pub public_ca: bool,
    /// The VMs that may have a console, by local id. No other VM ever gets one.
    #[serde(default)]
    pub console_targets: Vec<ConsoleTarget>,
    /// The only directory a further host's token and CA files may come from.
    /// Adding a further host is refused until this is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_files_dir: Option<String>,
}

/// One row of `console_targets`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleTarget {
    /// A VM's local id, such as `vm101` or `two-vm200`.
    pub vm: String,
}

impl ProxmoxConfig {
    /// Whether the `ca` slot must hold a certificate before the host can be reached.
    pub fn needs_ca(&self) -> bool {
        !self.public_ca && Url::parse(&self.url).is_ok_and(|url| url.scheme() == "https")
    }

    /// The local ids of the VMs that may have a console.
    pub fn consoles(&self) -> BTreeSet<String> {
        self.console_targets.iter().map(|row| row.vm.clone()).collect()
    }
}

fn problem(field: &str, message: impl Into<String>) -> ConfigProblem {
    ConfigProblem { field: field.to_string(), message: message.into() }
}

/// The field a serde error names, which is the first one in backticks.
fn serde_problem(error: &serde_json::Error) -> ConfigProblem {
    let text = error.to_string();
    let field = text.split('`').nth(1).unwrap_or("config").to_string();
    ConfigProblem { field, message: text }
}

/// Parses and checks a config document.
pub fn parse(config: &Value) -> Result<ProxmoxConfig, ConfigProblem> {
    let parsed: ProxmoxConfig = serde_json::from_value(config.clone()).map_err(|error| serde_problem(&error))?;
    check(&parsed)?;
    Ok(parsed)
}

/// Whether a URL's host is this machine.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => name == "localhost",
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// A device's base URL: the scheme and authority, and nothing else.
pub fn check_device_url(field: &str, text: &str, schemes: &[&str]) -> Result<Url, ConfigProblem> {
    let url = Url::parse(text).map_err(|error| problem(field, error.to_string()))?;
    if !schemes.contains(&url.scheme()) {
        return Err(problem(field, format!("the scheme must be one of {schemes:?}")));
    }
    if url.host_str().is_none()
        || url.authority().contains('@')
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(problem(field, "must be only a scheme, host and port"));
    }
    Ok(url)
}

/// A node name: letters, digits and dashes.
pub fn is_plain_node(text: &str) -> bool {
    !text.is_empty() && text.len() <= 63 && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Today's `[devices.proxmox]`, `[console]` and `[machines]` checks.
pub fn check(config: &ProxmoxConfig) -> Result<(), ConfigProblem> {
    let url = check_device_url("url", &config.url, &["http", "https"])?;
    if url.scheme() != "https" && !is_loopback(&url) {
        return Err(problem("url", "must use https, so the token never crosses the network in clear"));
    }
    if !is_plain_node(&config.node) {
        return Err(problem("node", "must be a plain node name"));
    }
    if let Some(bad) = config.console_targets.iter().find(|row| parse_machine(&row.vm).is_none()) {
        return Err(problem("console_targets", format!("{} is not a VM's local id", bad.vm)));
    }
    if config.host_files_dir.as_deref().is_some_and(|dir| !Path::new(dir).is_absolute()) {
        return Err(problem("host_files_dir", "must be an absolute path"));
    }
    Ok(())
}

/// A VM, and the host it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRef {
    /// [`PRIMARY_HOST`] or a further host's id.
    pub host: String,
    /// Its number on that host.
    pub vmid: u32,
}

/// A name for a further host.
pub fn is_host_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 16
        && text.as_bytes()[0].is_ascii_lowercase()
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && text != PRIMARY_HOST
}

/// `vm<id>` on the configured host, or `<host id>-vm<id>` on a further host.
pub fn parse_machine(local_id: &str) -> Option<MachineRef> {
    let (host, rest) = match local_id.split_once('-') {
        Some((host, rest)) if is_host_id(host) => (host, rest),
        Some(_) => return None,
        None => (PRIMARY_HOST, local_id),
    };
    let digits = rest.strip_prefix("vm")?;
    if digits.is_empty() || digits.len() > 9 || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(MachineRef { host: host.to_string(), vmid: digits.parse().ok()? })
}

/// The local id of a VM on a host: the inverse of [`parse_machine`].
pub fn machine_id(host: &str, vmid: u32) -> String {
    if host == PRIMARY_HOST {
        format!("vm{vmid}")
    } else {
        format!("{host}-vm{vmid}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(extra: Value) -> Value {
        let mut base = json!({ "url": "https://192.0.2.10:8006", "node": "pve" });
        base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        base
    }

    #[test]
    fn a_machine_id_is_a_vm_number_with_an_optional_host() {
        assert_eq!(parse_machine("vm101"), Some(MachineRef { host: PRIMARY_HOST.into(), vmid: 101 }));
        assert_eq!(parse_machine("two-vm200"), Some(MachineRef { host: "two".into(), vmid: 200 }));
        for bad in ["vm", "vm0", "vm01", "101", "host", "vm-1", "Nas-vm1", "primary-vm100", "a-b-vm1", "vm1234567890"] {
            assert_eq!(parse_machine(bad), None, "{bad}");
        }
        for (host, vmid) in [(PRIMARY_HOST, 100), ("two", 200)] {
            let id = machine_id(host, vmid);
            assert_eq!(parse_machine(&id), Some(MachineRef { host: host.into(), vmid }));
        }
    }

    #[test]
    fn the_token_must_stay_off_the_network_in_clear() {
        assert!(parse(&config(json!({}))).is_ok());
        assert!(parse(&config(json!({ "url": "http://127.0.0.1:8006" }))).is_ok());
        assert!(parse(&config(json!({ "url": "http://localhost:8006" }))).is_ok());
        let error = parse(&config(json!({ "url": "http://192.0.2.10:8006" }))).unwrap_err();
        assert_eq!(error.field, "url");
        for url in ["ftp://192.0.2.10", "https://user@192.0.2.10", "https://192.0.2.10/api2", "https://192.0.2.10?x=1"]
        {
            assert_eq!(parse(&config(json!({ "url": url }))).unwrap_err().field, "url", "{url}");
        }
    }

    #[test]
    fn the_node_console_list_and_files_directory_are_checked() {
        assert_eq!(parse(&config(json!({ "node": "a/b" }))).unwrap_err().field, "node");
        assert_eq!(parse(&config(json!({ "node": "" }))).unwrap_err().field, "node");
        let rows = json!({ "console_targets": [{ "vm": "vm101" }, { "vm": "two-vm200" }] });
        assert_eq!(parse(&config(rows)).unwrap().consoles().len(), 2);
        for bad in ["vm100/../shell", "host", "vm"] {
            let rows = json!({ "console_targets": [{ "vm": bad }] });
            assert_eq!(parse(&config(rows)).unwrap_err().field, "console_targets", "{bad}");
        }
        assert_eq!(parse(&config(json!({ "host_files_dir": "relative/dir" }))).unwrap_err().field, "host_files_dir");
    }

    #[test]
    fn unknown_and_missing_keys_are_named() {
        assert_eq!(parse(&config(json!({ "colour": "red" }))).unwrap_err().field, "colour");
        assert_eq!(parse(&json!({ "url": "https://192.0.2.10:8006" })).unwrap_err().field, "node");
        assert_eq!(parse(&config(json!({ "console_targets": [{ "id": "vm1" }] }))).unwrap_err().field, "id");
    }

    #[test]
    fn a_ca_is_needed_only_for_https_without_a_public_root() {
        let parsed = |extra| parse(&config(extra)).unwrap();
        assert!(parsed(json!({})).needs_ca());
        assert!(!parsed(json!({ "public_ca": true })).needs_ca());
        assert!(!parsed(json!({ "url": "http://127.0.0.1:8006" })).needs_ca());
    }
}

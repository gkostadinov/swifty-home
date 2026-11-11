//! Proxmox VE for Swifty Home.
//!
//! One instance is one node reached through its API with a least-privilege token.
//! It offers the node itself (`host`: power, status and charts) and the VMs that
//! were approved (`vm<id>`, or `<further host>-vm<id>` on a further host: start,
//! shutdown, reboot, status, charts, and the VNC console for the VMs on the
//! instance's console list). VMs are found with [`Driver::discover`] and stay
//! pending until someone approves each one.
//!
//! Nothing here names a VM, a node or a host: they all come from the instance
//! config and from the devices the hub has adopted.

#![forbid(unsafe_code)]

pub mod config;
mod driver;
pub mod protocol;
mod setup;
mod trust;

use std::sync::Arc;

use serde_json::{json, Value};
use swifty_plugin_api::{
    Capability, ConfigField, ConfigProblem, DeviceClass, DeviceDecl, DeviceRecord, Driver, FieldKind, InstanceView,
    LiveContext, Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

pub use config::{
    machine_id, parse_machine, ConsoleTarget, MachineRef, ProxmoxConfig, CA_SLOT, HOST_DEVICE, PRIMARY_HOST, TOKEN_SLOT,
};
pub use driver::{ProxmoxDriver, FORCE_STOP_REFUSED, HOST_ONLY, NOT_A_CONSOLE};
pub use setup::ProxmoxSetup;

/// Why an instance without its token cannot work.
pub const TOKEN_MISSING: &str = "The Proxmox API token is not set.";
/// Why an instance whose host needs a private CA cannot work without it.
pub const CA_MISSING: &str = "The CA certificate of the Proxmox host is not set. Add it, or set public_ca when the \
     host's certificate is publicly trusted.";

const CONSOLE_ROW: &[ConfigField] = &[ConfigField {
    key: "vm",
    label: "VM",
    help: "A VM's local id: vm and its number, such as vm101, or a further host's id, a dash and that, such as \
           two-vm200.",
    kind: FieldKind::Text { max_len: 32 },
    required: true,
    default: None,
}];

const CONFIG: &[ConfigField] = &[
    ConfigField {
        key: "url",
        label: "API address",
        help: "https:// and the host's address with its port, usually 8006, and nothing after it. Plain http only \
               on loopback.",
        kind: FieldKind::Url { schemes: &["https", "http"] },
        required: true,
        default: None,
    },
    ConfigField {
        key: "node",
        label: "Node name",
        help: "The node's name as Proxmox shows it under Datacenter.",
        kind: FieldKind::Text { max_len: 63 },
        required: true,
        default: None,
    },
    ConfigField {
        key: "public_ca",
        label: "Publicly trusted certificate",
        help: "The host's certificate chains to a public root, so no CA certificate is stored. Proxmox's own \
               certificate does not.",
        kind: FieldKind::Bool,
        required: false,
        default: Some("false"),
    },
    ConfigField {
        key: "console_targets",
        label: "Consoles",
        help: "The VMs that may have a VNC console. No other VM ever gets one, approved or not.",
        kind: FieldKind::Rows { fields: CONSOLE_ROW },
        required: false,
        default: None,
    },
    ConfigField {
        key: "host_files_dir",
        label: "Further hosts' files",
        help: "An absolute path: the only directory a further host's token and CA files may come from. Adding a \
               further host is refused until this is set.",
        kind: FieldKind::Text { max_len: 4096 },
        required: false,
        default: None,
    },
];

const SECRETS: &[SecretSlot] = &[
    SecretSlot {
        slot: TOKEN_SLOT,
        label: "API token",
        help: "user@realm!tokenid=uuid, as Proxmox showed it when the token was made.",
        required: true,
    },
    SecretSlot {
        slot: CA_SLOT,
        label: "CA certificate",
        help: "The PEM of the CA that signed the host's certificate. Needed unless public_ca is set.",
        required: false,
    },
];

static MANIFEST: Manifest = Manifest {
    id: "proxmox",
    name: "Proxmox VE",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "A Proxmox VE node and its VMs: VM and node power, status, stored charts, the VNC console of chosen \
              VMs, and further hosts, through the API with a least-privilege token.",
    reach: Reach::Local,
    network: &[NetUse::Https { port: 8006 }, NetUse::Ws { port: 8006 }],
    classes: &[DeviceClass::Host, DeviceClass::Vm],
    config_version: 1,
    config: CONFIG,
    secrets: SECRETS,
    actions: &[],
    vocabulary: &[],
    nouns: &[("proxmox", DeviceClass::Host)],
    multi_instance: true,
};

/// The node itself, as a device.
fn host_decl() -> DeviceDecl {
    DeviceDecl {
        local_id: HOST_DEVICE.to_string(),
        target_hint: Some(HOST_DEVICE.to_string()),
        class: DeviceClass::Host,
        name: "Proxmox host".to_string(),
        capabilities: vec![Capability::NodePower, Capability::History],
        attrs: json!({}),
    }
}

/// An adopted record that is one of this plugin's VMs.
fn adopted_vm(record: &DeviceRecord) -> Option<MachineRef> {
    (record.class == DeviceClass::Vm).then(|| parse_machine(&record.local_id)).flatten()
}

/// The plugin. Use [`PLUGIN`].
pub struct ProxmoxPlugin;

/// The Proxmox plugin, for the hub's list of compiled-in plugins.
pub static PLUGIN: ProxmoxPlugin = ProxmoxPlugin;

impl Plugin for ProxmoxPlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        config::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        let config = match config::parse(instance.config) {
            Ok(config) => config,
            Err(problem) => return Readiness::Disabled(problem.to_string()),
        };
        if !instance.secrets_present.contains(TOKEN_SLOT) {
            return Readiness::NeedsSetup(TOKEN_MISSING.to_string());
        }
        if config.needs_ca() && !instance.secrets_present.contains(CA_SLOT) {
            return Readiness::NeedsSetup(CA_MISSING.to_string());
        }
        Readiness::Ready
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        let consoles = config::parse(instance.config).map(|config| config.consoles()).unwrap_or_default();
        let mut devices = vec![host_decl()];
        for record in instance.adopted {
            let Some(machine) = adopted_vm(record) else { continue };
            if devices.iter().any(|known| known.local_id == record.local_id) {
                continue;
            }
            devices.push(driver::vm_decl(&machine, &record.name, consoles.contains(&record.local_id)));
        }
        devices
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        let is_vm =
            |id: &str| instance.adopted.iter().any(|record| record.local_id == id && adopted_vm(record).is_some());
        driver::refusal(is_vm, device, op)
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = config::parse(&ctx.config).map_err(|problem| PluginError(problem.to_string()))?;
        let token = ctx.secrets.require(TOKEN_SLOT)?;
        let ca = ctx.secrets.get(CA_SLOT).map(|value| value.expose());
        if config.needs_ca() && ca.is_none() {
            return Err(PluginError(CA_MISSING.to_string()));
        }
        let driver = ProxmoxDriver::new(ctx.grant, &config, token.expose(), ca, ctx.timeouts, ctx.directory)
            .map_err(PluginError)?;
        Ok(Arc::new(driver))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(ProxmoxSetup::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Nothing is seeded any more. These are the four VMs a test house has adopted.
    fn four_vms() -> Vec<DeviceRecord> {
        (100..=103)
            .map(|id| DeviceRecord {
                target: format!("vm{id}"),
                local_id: format!("vm{id}"),
                class: DeviceClass::Vm,
                name: format!("VM {id}"),
                attrs: json!({ "host": PRIMARY_HOST, "vmid": id }),
            })
            .collect()
    }

    fn with_view<T>(adopted: &[DeviceRecord], call: impl FnOnce(&InstanceView<'_>) -> T) -> T {
        let config = json!({ "url": "https://192.0.2.10:8006", "node": "pve", "console_targets": [{ "vm": "vm102" }] });
        let present: BTreeSet<String> = [TOKEN_SLOT.to_string(), CA_SLOT.to_string()].into();
        call(&InstanceView { id: "proxmox", config: &config, secrets_present: &present, adopted })
    }

    #[test]
    fn the_seeded_list_is_exactly_the_four_vms() {
        let adopted = four_vms();
        let is_vm = |id: &str| with_view(&adopted, |view| PLUGIN.refusal(view, id, &Op::VmStart).is_none());
        for id in [100, 101, 102, 103] {
            assert!(is_vm(&format!("vm{id}")));
        }
        for bad in ["vm99", "vm104", "vm1000", "vm", "100", "host", "vm100/../101", "vm-100"] {
            assert!(!is_vm(bad), "{bad}");
        }
        let devices = with_view(&adopted, |view| PLUGIN.devices(view));
        let ids: Vec<&str> = devices.iter().map(|device| device.local_id.as_str()).collect();
        assert_eq!(ids, ["host", "vm100", "vm101", "vm102", "vm103"]);
    }

    #[test]
    fn only_the_three_vm_operations_and_node_power_are_allowed() {
        let adopted = four_vms();
        let refusal = |id: &str, op: Op| with_view(&adopted, |view| PLUGIN.refusal(view, id, &op));
        assert_eq!(refusal("vm101", Op::VmStart), None);
        assert_eq!(refusal("vm101", Op::VmShutdown), None);
        assert_eq!(refusal("vm101", Op::VmReboot), None);
        assert!(refusal("vm104", Op::VmStart).is_some());
        assert!(refusal("vm101", Op::VmForceStop).is_some());
        assert_eq!(refusal("host", Op::HostShutdown), None);
        assert_eq!(refusal("host", Op::HostReboot), None);
        assert!(refusal("vm101", Op::HostReboot).is_some());
    }

    #[test]
    fn a_vm_has_a_console_only_when_the_config_lists_it() {
        let adopted = four_vms();
        let devices = with_view(&adopted, |view| PLUGIN.devices(view));
        for device in &devices[1..] {
            let console = device.capabilities.contains(&Capability::Console);
            assert_eq!(console, device.local_id == "vm102", "{}", device.local_id);
            assert_eq!(device.target_hint.as_deref(), Some(device.local_id.as_str()));
            assert_eq!(device.class, DeviceClass::Vm);
        }
        assert_eq!(devices[0].capabilities, [Capability::NodePower, Capability::History]);
    }
}

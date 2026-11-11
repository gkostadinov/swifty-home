//! The live driver: power and status for VMs on the approved list, power for the
//! configured node itself, the stored history, the list of VMs, the VNC console of
//! one approved VM on the console list, and further hosts.
//!
//! A further host is reached only through these same typed calls, over https with
//! its certificate checked against the CA named for it, so it cannot be used as a
//! proxy.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::HeaderValue;
use serde_json::{json, Value};
use swifty_net::{self as net, ConsoleLink, Http, LiveGrant, Reply, Roots, Timeouts};
use swifty_plugin_api::{
    Capability, CommandAck, ConsoleGrant, DeviceClass, DeviceDecl, DeviceError, Directory, Driver, ExtraHost,
    HistoryRange, MachineHost, Op, SeenMachine, State,
};

use crate::config::{
    is_host_id, is_plain_node, machine_id, parse_machine, MachineRef, ProxmoxConfig, HOST_DEVICE, PRIMARY_HOST,
};
use crate::protocol;
use crate::trust;

/// Why a VM operation is refused on a device that is not an approved VM.
pub fn not_listed(device: &str) -> String {
    format!("{device} is not on the approved list.")
}

/// Force-stop is never sent: it is the one VM operation that can lose data.
pub const FORCE_STOP_REFUSED: &str =
    "Force-stop is not available: the Proxmox plugin only does start, shutdown, reboot and status.";
/// Only the configured node can be powered.
pub const HOST_ONLY: &str = "Only the host can be powered from here.";
/// What this plugin does not do at all.
pub const NOT_PROXMOX: &str = "Proxmox cannot do that.";
/// A VM that is approved but not on the console list.
pub const NOT_A_CONSOLE: &str = "This VM is not on the console list.";

/// Why `op` is not done on `device`, if it is not. `is_vm` says whether a local id
/// is an approved VM. The same rule serves the pure refusal and the driver.
pub fn refusal(is_vm: impl Fn(&str) -> bool, device: &str, op: &Op) -> Option<String> {
    match op {
        Op::VmStart | Op::VmShutdown | Op::VmReboot => (!is_vm(device)).then(|| not_listed(device)),
        Op::VmForceStop => Some(FORCE_STOP_REFUSED.to_string()),
        Op::HostShutdown | Op::HostReboot => (device != HOST_DEVICE).then(|| HOST_ONLY.to_string()),
        _ => Some(NOT_PROXMOX.to_string()),
    }
}

/// What a VM can do here. A console only when the config lists it.
pub fn vm_capabilities(console: bool) -> Vec<Capability> {
    let mut capabilities = vec![Capability::Lifecycle, Capability::History];
    if console {
        capabilities.push(Capability::Console);
    }
    capabilities
}

/// A VM as a device: its local id is also the target it would like.
pub fn vm_decl(machine: &MachineRef, name: &str, console: bool) -> DeviceDecl {
    let local_id = machine_id(&machine.host, machine.vmid);
    let name = name.trim();
    DeviceDecl {
        target_hint: Some(local_id.clone()),
        local_id,
        class: DeviceClass::Vm,
        name: if name.is_empty() { format!("VM {}", machine.vmid) } else { name.to_string() },
        capabilities: vm_capabilities(console),
        attrs: json!({ "host": machine.host, "vmid": machine.vmid }),
    }
}

/// A file a further host's credential or CA comes from, as today's secret files:
/// one value, readable by this service alone.
fn read_host_file(path: &Path) -> Result<String, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!("{} must not be readable by group or others", path.display()));
        }
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let value = text.trim();
    if value.is_empty() {
        return Err(format!("{} is empty", path.display()));
    }
    Ok(value.to_string())
}

/// The shape of a Proxmox token's uuid, which Proxmox shows on its own line. It
/// would pass for a node name, so it has to be told apart.
fn looks_like_token_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, length)| group.len() == length && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// One Proxmox API endpoint and how to speak to it.
pub(crate) struct Host {
    http: Http,
    node: String,
    authorization: HeaderValue,
    ca_pem: Option<Vec<u8>>,
    pub(crate) roots: Roots,
}

impl Host {
    /// The host the instance config names. Trusts the public roots and the `ca` slot.
    pub(crate) fn primary(
        grant: LiveGrant,
        config: &ProxmoxConfig,
        token: &str,
        ca_pem: Option<&str>,
        timeouts: Timeouts,
    ) -> Result<Self, String> {
        let ca_pem = ca_pem.map(|pem| pem.as_bytes().to_vec());
        let roots = Roots::PublicAndNamedCa;
        Ok(Self {
            http: trust::client(grant, &config.url, timeouts, ca_pem.as_deref(), roots)?,
            node: config.node.clone(),
            authorization: trust::authorization(token)?,
            ca_pem,
            roots,
        })
    }

    /// A further host. Added by address, so only the CA named for it can vouch for it.
    fn further(grant: LiveGrant, spec: &ExtraHost, timeouts: Timeouts) -> Result<Self, String> {
        let token = read_host_file(&spec.token_file)?;
        let ca_pem = std::fs::read(&spec.ca_file)
            .map_err(|e| format!("cannot read the certificate file {}: {e}", spec.ca_file.display()))?;
        let roots = Roots::NamedCaOnly;
        Ok(Self {
            http: trust::client_with_file(grant, &format!("https://{}", spec.address), timeouts, &spec.ca_file, roots)?,
            node: spec.node.clone(),
            authorization: trust::authorization(&token)?,
            ca_pem: Some(ca_pem),
            roots,
        })
    }

    async fn get(&self, path: &str) -> Result<Reply, DeviceError> {
        self.http.send(self.http.get(self.http.url(path)?).header(AUTHORIZATION, self.authorization.clone())).await
    }

    async fn post(&self, path: &str, form: &'static str) -> Result<Reply, DeviceError> {
        let mut request = self.http.post(self.http.url(path)?).header(AUTHORIZATION, self.authorization.clone());
        if !form.is_empty() {
            request = request.header(CONTENT_TYPE, protocol::FORM).body(form);
        }
        self.http.send(request).await
    }

    pub(crate) async fn read_vm(&self, vmid: u32) -> Result<State, DeviceError> {
        protocol::vm_state(&self.get(&protocol::vm_path(&self.node, vmid, "status/current")).await?)
    }

    /// The node's own status, for a real uptime. A token without `Sys.Audit` on
    /// the node falls back to showing only that the API answers.
    pub(crate) async fn read_node(&self) -> Result<State, DeviceError> {
        let reply = self.get(&protocol::node_path(&self.node, "status")).await?;
        if reply.status.as_u16() == 403 {
            let reply = self.get(protocol::VERSION_PATH).await?;
            if !reply.status.is_success() {
                return Err(protocol::refused(&reply));
            }
            return Ok(State::Host { up: true, uptime_s: 0 });
        }
        protocol::node_state(&reply)
    }

    pub(crate) async fn list_vms(&self, id: &str) -> Result<Vec<SeenMachine>, DeviceError> {
        protocol::machines(&self.get(&protocol::vm_list_path(&self.node)).await?, id)
    }

    async fn rrd(&self, path: &str, range: HistoryRange) -> Result<Vec<Value>, DeviceError> {
        let mut url = self.http.url(path)?;
        url.query_pairs_mut().append_pair("timeframe", protocol::timeframe(range)).append_pair("cf", "AVERAGE");
        let reply = self.http.send(self.http.get(url).header(AUTHORIZATION, self.authorization.clone())).await?;
        protocol::rrd_points(&reply)
    }

    /// Starts, stops or reboots one VM.
    async fn vm_power(&self, vmid: u32, verb: &str) -> Result<CommandAck, DeviceError> {
        let reply = self.post(&protocol::vm_path(&self.node, vmid, &format!("status/{verb}")), "").await?;
        if !reply.status.is_success() {
            return Err(protocol::refused(&reply));
        }
        Ok(CommandAck)
    }
}

/// The adopted devices of an instance, as the hub keeps them current.
fn approved(directory: &dyn Directory, device: &str) -> Option<MachineRef> {
    let record = directory.get(device)?;
    if record.class != DeviceClass::Vm {
        return None;
    }
    parse_machine(device)
}

/// What a live instance holds.
pub struct ProxmoxDriver {
    grant: LiveGrant,
    timeouts: Timeouts,
    pub(crate) primary: Arc<Host>,
    further: RwLock<HashMap<String, Arc<Host>>>,
    directory: Arc<dyn Directory>,
    consoles: BTreeSet<String>,
    host_files_dir: Option<PathBuf>,
}

impl ProxmoxDriver {
    /// A driver for the configured host. Nothing is sent yet.
    pub(crate) fn new(
        grant: LiveGrant,
        config: &ProxmoxConfig,
        token: &str,
        ca_pem: Option<&str>,
        timeouts: Timeouts,
        directory: Arc<dyn Directory>,
    ) -> Result<Self, String> {
        Ok(Self {
            grant,
            timeouts,
            primary: Arc::new(Host::primary(grant, config, token, ca_pem, timeouts)?),
            further: RwLock::new(HashMap::new()),
            directory,
            consoles: config.consoles(),
            host_files_dir: config.host_files_dir.as_deref().map(PathBuf::from),
        })
    }

    fn machine(&self, device: &str) -> Result<MachineRef, DeviceError> {
        approved(self.directory.as_ref(), device).ok_or_else(|| DeviceError::Disabled(not_listed(device)))
    }

    /// An approved VM that is on the console list.
    fn console_machine(&self, device: &str) -> Result<MachineRef, DeviceError> {
        let machine = self.machine(device)?;
        if !self.consoles.contains(device) {
            return Err(DeviceError::Disabled(NOT_A_CONSOLE.to_string()));
        }
        Ok(machine)
    }

    fn host_of(&self, machine: &MachineRef) -> Result<Arc<Host>, DeviceError> {
        if machine.host == PRIMARY_HOST {
            return Ok(self.primary.clone());
        }
        self.further
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&machine.host)
            .cloned()
            .ok_or_else(|| DeviceError::Disabled("That Proxmox host is not available.".to_string()))
    }

    pub(crate) fn further_hosts(&self) -> Vec<(String, Arc<Host>)> {
        self.further
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(id, host)| (id.clone(), host.clone()))
            .collect()
    }

    /// Everything that is checked before a further host is used: its id, its node
    /// name, and that its files come from `host_files_dir` and nowhere else.
    fn check_further(&self, spec: &ExtraHost) -> Result<(), String> {
        let Some(dir) = self.host_files_dir.as_deref() else {
            return Err("host_files_dir is not configured, so a further host cannot be added.".to_string());
        };
        if !is_host_id(&spec.id) {
            return Err("id must be a lower-case letter, then lower-case letters and digits".to_string());
        }
        if !is_plain_node(&spec.node) || looks_like_token_uuid(&spec.node) {
            return Err("node must be a plain node name".to_string());
        }
        if [&spec.token_file, &spec.ca_file].iter().any(|file| file.parent() != Some(dir)) {
            return Err("a further host's token and CA files must be in host_files_dir".to_string());
        }
        Ok(())
    }
}

#[async_trait]
impl Driver for ProxmoxDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        if let Some(reason) = refusal(|id| approved(self.directory.as_ref(), id).is_some(), device, op) {
            return Err(DeviceError::Disabled(reason));
        }
        if let Op::HostShutdown | Op::HostReboot = op {
            let form = protocol::node_power_form(*op == Op::HostReboot);
            let reply = self.primary.post(&protocol::node_path(&self.primary.node, "status"), form).await?;
            return if reply.status.is_success() { Ok(CommandAck) } else { Err(protocol::refused(&reply)) };
        }
        let machine = self.machine(device)?;
        let verb = match op {
            Op::VmStart => "start",
            Op::VmShutdown => "shutdown",
            Op::VmReboot => "reboot",
            _ => return Err(DeviceError::Disabled(NOT_PROXMOX.to_string())),
        };
        self.host_of(&machine)?.vm_power(machine.vmid, verb).await
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        if device == HOST_DEVICE {
            return self.primary.read_node().await;
        }
        let machine = self.machine(device)?;
        self.host_of(&machine)?.read_vm(machine.vmid).await
    }

    /// The VMs the hosts report, as candidates. VMs need approval, so the hub
    /// keeps them pending until someone approves each one.
    async fn discover(&self, _window: Duration) -> Result<Vec<DeviceDecl>, DeviceError> {
        let seen = MachineHost::list_machines(self).await?;
        Ok(seen
            .iter()
            .map(|machine| {
                let reference = MachineRef { host: machine.host.clone(), vmid: machine.vmid };
                let console = self.consoles.contains(&machine_id(&reference.host, reference.vmid));
                vm_decl(&reference, &machine.name, console)
            })
            .collect())
    }

    fn machines(&self) -> Option<&dyn MachineHost> {
        Some(self)
    }
}

#[async_trait]
impl MachineHost for ProxmoxDriver {
    async fn list_machines(&self) -> Result<Vec<SeenMachine>, DeviceError> {
        let mut seen = self.primary.list_vms(PRIMARY_HOST).await?;
        for (id, host) in self.further_hosts() {
            // One further host being down must not hide the others.
            match host.list_vms(&id).await {
                Ok(mut listed) => seen.append(&mut listed),
                Err(error) => tracing::warn!(host = %id, %error, "could not list that Proxmox host"),
            }
        }
        Ok(seen)
    }

    async fn history(&self, device: &str, range: HistoryRange) -> Result<Vec<Value>, DeviceError> {
        if device == HOST_DEVICE {
            let path = protocol::node_path(&self.primary.node, "rrddata");
            return self.primary.rrd(&path, range).await;
        }
        let machine = self.machine(device)?;
        let host = self.host_of(&machine)?;
        host.rrd(&protocol::vm_path(&host.node, machine.vmid, "rrddata"), range).await
    }

    async fn open_console(&self, device: &str) -> Result<ConsoleGrant, DeviceError> {
        let machine = self.console_machine(device)?;
        let host = self.host_of(&machine)?;
        let reply = host.post(&protocol::vm_path(&host.node, machine.vmid, "vncproxy"), protocol::VNC_FORM).await?;
        let (port, ticket) = protocol::console_offer(&reply)?;
        // A `Secret` from here on: its `Debug` and `Display` never show it.
        Ok(ConsoleGrant { target: device.to_string(), port, ticket: ticket.into() })
    }

    async fn connect_console(&self, grant: &ConsoleGrant) -> Result<ConsoleLink, DeviceError> {
        let machine = self.console_machine(&grant.target)?;
        let host = self.host_of(&machine)?;
        let mut url = host.http.url(&protocol::vm_path(&host.node, machine.vmid, "vncwebsocket"))?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme).map_err(|()| DeviceError::Disabled("the device URL is not valid".to_string()))?;
        // The ticket travels only in this URL, to the host that issued it, once its
        // certificate has checked out. It is never logged.
        url.query_pairs_mut()
            .append_pair("port", &grant.port.to_string())
            .append_pair("vncticket", grant.ticket.expose());
        net::console_connect(self.grant, &url, &host.authorization, host.ca_pem.as_deref(), host.roots, self.timeouts)
            .await
    }

    fn add_host(&self, spec: &ExtraHost) -> Result<(), String> {
        self.check_further(spec)?;
        let host = Host::further(self.grant, spec, self.timeouts)?;
        self.further.write().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(spec.id.clone(), Arc::new(host));
        Ok(())
    }

    fn remove_host(&self, id: &str) {
        self.further.write().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swifty_plugin_api::DeviceRecord;
    use swifty_plugin_testkit::devices::PROXMOX_TOKEN;

    struct Adopted(Vec<DeviceRecord>);

    impl Directory for Adopted {
        fn get(&self, local_id: &str) -> Option<DeviceRecord> {
            self.0.iter().find(|record| record.local_id == local_id).cloned()
        }
        fn all(&self) -> Vec<DeviceRecord> {
            self.0.clone()
        }
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }

    fn private_ca() -> String {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.self_signed(&key).unwrap().pem()
    }

    #[test]
    fn a_further_host_trusts_only_its_named_ca() {
        let dir = tempfile::tempdir().unwrap();
        let ca_file = write(dir.path(), "ca.crt", &private_ca());
        let token_file = write(dir.path(), "pve2.credential", PROXMOX_TOKEN);

        let grant = LiveGrant::when_live(true).unwrap();
        let timeouts = Timeouts { connect: Duration::from_millis(1), read: Duration::from_millis(1) };
        let config = ProxmoxConfig {
            url: "https://127.0.0.1:9".to_string(),
            node: "pve".to_string(),
            public_ca: false,
            console_targets: Vec::new(),
            host_files_dir: Some(dir.path().display().to_string()),
        };
        let ca = private_ca();
        let driver =
            ProxmoxDriver::new(grant, &config, PROXMOX_TOKEN, Some(&ca), timeouts, Arc::new(Adopted(Vec::new())))
                .unwrap();
        assert_eq!(driver.primary.roots, Roots::PublicAndNamedCa);

        let spec = ExtraHost {
            id: "two".to_string(),
            address: "127.0.0.1:9".parse().unwrap(),
            node: "pve2".to_string(),
            token_file,
            ca_file,
        };
        driver.add_host(&spec).unwrap();
        let hosts = driver.further_hosts();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].1.roots, Roots::NamedCaOnly);

        driver.remove_host("two");
        assert!(driver.further_hosts().is_empty());
    }

    #[test]
    fn a_further_host_is_refused_unless_its_files_come_from_the_files_directory() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let ca_file = write(dir.path(), "ca.crt", &private_ca());
        let token_file = write(dir.path(), "pve2.credential", PROXMOX_TOKEN);
        let stray = write(elsewhere.path(), "stray.credential", PROXMOX_TOKEN);
        let grant = LiveGrant::when_live(true).unwrap();
        let timeouts = Timeouts { connect: Duration::from_millis(1), read: Duration::from_millis(1) };
        let build = |files: Option<&Path>| {
            let config = ProxmoxConfig {
                url: "http://127.0.0.1:9".to_string(),
                node: "pve".to_string(),
                public_ca: false,
                console_targets: Vec::new(),
                host_files_dir: files.map(|dir| dir.display().to_string()),
            };
            ProxmoxDriver::new(grant, &config, PROXMOX_TOKEN, None, timeouts, Arc::new(Adopted(Vec::new()))).unwrap()
        };
        let spec = ExtraHost {
            id: "two".to_string(),
            address: "127.0.0.1:9".parse().unwrap(),
            node: "pve2".to_string(),
            token_file: token_file.clone(),
            ca_file: ca_file.clone(),
        };

        let unset = build(None);
        assert!(unset.add_host(&spec).unwrap_err().contains("host_files_dir"));
        let driver = build(Some(dir.path()));
        let refused = [
            ExtraHost { token_file: stray, ..spec.clone() },
            ExtraHost { id: PRIMARY_HOST.to_string(), ..spec.clone() },
            ExtraHost { id: "Nas2".to_string(), ..spec.clone() },
            ExtraHost { node: "a/b".to_string(), ..spec.clone() },
            ExtraHost { node: "11111111-2222-3333-4444-555555555555".to_string(), ..spec.clone() },
        ];
        for bad in refused {
            assert!(driver.add_host(&bad).is_err(), "{bad:?}");
        }
        assert!(driver.further_hosts().is_empty());
        driver.add_host(&spec).unwrap();
        assert_eq!(driver.further_hosts().len(), 1);
    }
}

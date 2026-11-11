//! The guided setup: create the API token, say where the host is, approve VMs,
//! and pick which of them may have a console. It replaces the token script and
//! the hand-written `[devices.proxmox]` and `[console]` sections.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;
use swifty_plugin_api::{
    Choice, DeviceDecl, Field, FieldType, Scalar, Secret, SeenMachine, SetupContext, SetupFlow, SetupInput,
    SetupResult, SetupStep,
};

use crate::config::{
    self, check, machine_id, ConsoleTarget, MachineRef, ProxmoxConfig, CA_SLOT, PRIMARY_HOST, TOKEN_SLOT,
};
use crate::driver::{vm_decl, Host};
use crate::trust;

/// The longest CA paste taken. A chain of a few certificates fits easily.
const MAX_CA: usize = 16 * 1024;

const LIVE_NEEDED: &str = "Setting up Proxmox reads the list of VMs from the host, which needs live mode. \
     Set live = true and start the setup again.";

const TOKEN_TEXT: &str = "In the Proxmox web interface, under Datacenter, Permissions, create a user for Swifty Home \
     and an API token for it with privilege separation. Give the token a role with VM.Audit, VM.PowerMgmt and \
     VM.Console on the VMs it should control, and Sys.Audit and Sys.PowerMgmt on the node if it should show the \
     node's uptime and power it. Nothing else is needed. Proxmox shows the token's value once: copy it as \
     user@realm!tokenid=uuid, then continue.";

/// Which step the person is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Token,
    Form,
    Machines,
    Consoles,
    /// A step failed; going back shows this step again.
    Failed(Retry),
    Done,
}

/// The steps a failure can go back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    Token,
    Form,
    Machines,
    Consoles,
}

impl Retry {
    fn stage(self) -> Stage {
        match self {
            Retry::Token => Stage::Token,
            Retry::Form => Stage::Form,
            Retry::Machines => Stage::Machines,
            Retry::Consoles => Stage::Consoles,
        }
    }
}

/// What the form gave.
struct Entered {
    config: ProxmoxConfig,
    /// `None` when the stored one is kept.
    token: Option<Secret>,
    /// `None` when the stored one is kept, or none is needed.
    ca: Option<Secret>,
}

/// The setup flow of one instance.
pub struct ProxmoxSetup {
    stage: Stage,
    existing: Option<ProxmoxConfig>,
    entered: Option<Entered>,
    host: Option<Host>,
    found: Vec<SeenMachine>,
    approved: Vec<String>,
    consoles: Vec<String>,
    problem: Option<String>,
}

impl ProxmoxSetup {
    /// A flow that has not begun.
    pub fn new() -> Self {
        Self {
            stage: Stage::Token,
            existing: None,
            entered: None,
            host: None,
            found: Vec::new(),
            approved: Vec::new(),
            consoles: Vec::new(),
            problem: None,
        }
    }

    /// The config the form starts from: what was entered, else what is stored.
    fn current(&self) -> Option<&ProxmoxConfig> {
        self.entered.as_ref().map(|entered| &entered.config).or(self.existing.as_ref())
    }

    fn fail(&mut self, reason: impl Into<String>, retry: Option<Retry>) -> SetupStep {
        match retry {
            Some(back) => self.stage = Stage::Failed(back),
            None => self.stage = Stage::Done,
        }
        SetupStep::Failed { reason: reason.into(), retry: retry.is_some() }
    }

    fn show(&mut self, stage: Stage, ctx: &SetupContext) -> SetupStep {
        self.stage = stage;
        match stage {
            Stage::Token => SetupStep::External {
                title: "Create an API token".to_string(),
                text: TOKEN_TEXT.to_string(),
                link: None,
                fields: Vec::new(),
            },
            Stage::Form => self.form(ctx),
            Stage::Machines => self.machines_step(),
            Stage::Consoles => self.consoles_step(),
            Stage::Failed(_) | Stage::Done => {
                SetupStep::Failed { reason: "This setup has finished.".to_string(), retry: false }
            }
        }
    }

    fn form(&mut self, ctx: &SetupContext) -> SetupStep {
        let current = self.current().cloned();
        let has = |slot: &str| ctx.secrets_present.contains(slot);
        let keep = |slot: &str, help: &str| {
            if has(slot) {
                format!("{help} Leave it empty to keep the stored one.")
            } else {
                help.to_string()
            }
        };
        let text = |value: Option<&str>| value.map(|value| Scalar::Text(value.to_string()));
        let intro = "Where the host's API answers, and the token from the last step.";
        let fields = vec![
            Field {
                key: "url".to_string(),
                label: "API address".to_string(),
                help: "https:// and the host's address with port 8006, and nothing after it.".to_string(),
                kind: FieldType::Url,
                required: true,
                value: text(current.as_ref().map(|c| c.url.as_str())),
            },
            Field {
                key: "node".to_string(),
                label: "Node name".to_string(),
                help: "The node's name as Proxmox shows it under Datacenter.".to_string(),
                kind: FieldType::Text { max_len: 63 },
                required: true,
                value: text(current.as_ref().map(|c| c.node.as_str())),
            },
            Field {
                key: TOKEN_SLOT.to_string(),
                label: "API token".to_string(),
                help: keep(TOKEN_SLOT, "user@realm!tokenid=uuid, as Proxmox showed it."),
                kind: FieldType::Secret,
                required: !has(TOKEN_SLOT),
                value: None,
            },
            Field {
                key: "public_ca".to_string(),
                label: "Publicly trusted certificate".to_string(),
                help: "Tick this only when the host's certificate comes from a public authority. Proxmox's own \
                       certificate does not."
                    .to_string(),
                kind: FieldType::Bool,
                required: false,
                value: Some(Scalar::Bool(current.as_ref().is_some_and(|c| c.public_ca))),
            },
            Field {
                key: CA_SLOT.to_string(),
                label: "CA certificate".to_string(),
                help: keep(
                    CA_SLOT,
                    "The PEM of the CA that signed the host's certificate: on the host, /etc/pve/pve-root-ca.pem.",
                ),
                kind: FieldType::Multiline { max_len: MAX_CA },
                required: false,
                value: None,
            },
        ];
        let text = match self.problem.take() {
            Some(problem) => format!("{problem} {intro}"),
            None => intro.to_string(),
        };
        SetupStep::Form { title: "Connect to Proxmox".to_string(), text, fields }
    }

    fn machines_step(&self) -> SetupStep {
        let options = self
            .found
            .iter()
            .map(|machine| {
                let id = machine_id(&machine.host, machine.vmid);
                Choice {
                    selected: self.approved.contains(&id),
                    label: label(machine),
                    detail: format!("VM {}", machine.vmid),
                    id,
                }
            })
            .collect();
        SetupStep::Choose {
            title: "Approve VMs".to_string(),
            text: "Pick the VMs Swifty Home may start, stop and restart. Others can be approved later from the \
                   machines list."
                .to_string(),
            options,
            multiple: true,
            min: 0,
        }
    }

    /// The approved VMs, and any VM the stored config already gives a console.
    fn console_options(&self) -> Vec<String> {
        let mut options = self.approved.clone();
        for vm in self.existing.iter().flat_map(|config| config.consoles()) {
            if !options.contains(&vm) {
                options.push(vm);
            }
        }
        options
    }

    fn consoles_step(&self) -> SetupStep {
        let stored = self.existing.as_ref().map(ProxmoxConfig::consoles).unwrap_or_default();
        let options = self
            .console_options()
            .into_iter()
            .map(|id| {
                let found = self.found.iter().find(|machine| machine_id(&machine.host, machine.vmid) == id);
                Choice {
                    selected: stored.contains(&id) || self.consoles.contains(&id),
                    label: found.map(label).unwrap_or_else(|| id.clone()),
                    detail: found.map(|machine| format!("VM {}", machine.vmid)).unwrap_or_default(),
                    id,
                }
            })
            .collect();
        SetupStep::Choose {
            title: "Consoles".to_string(),
            text: "Pick the VMs that may have a VNC console here. Opening one always takes a passkey. A VM \
                   whose screen shows nothing useful, such as one with its GPU passed through, is best left out."
                .to_string(),
            options,
            multiple: true,
            min: 0,
        }
    }

    async fn submit_form(&mut self, ctx: &SetupContext, values: &BTreeMap<String, Scalar>) -> SetupStep {
        let text = |key: &str| match values.get(key) {
            Some(Scalar::Text(text)) => text.trim().to_string(),
            _ => String::new(),
        };
        let public_ca = matches!(values.get("public_ca"), Some(Scalar::Bool(true)));
        let stored = self.existing.as_ref();
        let config = ProxmoxConfig {
            url: text("url"),
            node: text("node"),
            public_ca,
            console_targets: stored.map(|c| c.console_targets.clone()).unwrap_or_default(),
            host_files_dir: stored.and_then(|c| c.host_files_dir.clone()),
        };
        let token = Some(text(TOKEN_SLOT)).filter(|value| !value.is_empty());
        let ca = Some(text(CA_SLOT)).filter(|value| !value.is_empty());
        let has = |slot: &str| ctx.secrets_present.contains(slot);

        let problem = if let Err(problem) = check(&config) {
            Some(format!("{}: {}.", problem.field, problem.message))
        } else if token.is_none() && !has(TOKEN_SLOT) {
            Some("The API token is needed.".to_string())
        } else if let Some(Err(why)) = token.as_deref().map(trust::authorization) {
            Some(format!("{why}."))
        } else if ca.as_deref().is_some_and(|pem| !trust::is_pem_certificate(pem.as_bytes())) {
            Some("The CA certificate must be the PEM text, starting with -----BEGIN CERTIFICATE-----.".to_string())
        } else if config.needs_ca() && ca.is_none() && !has(CA_SLOT) {
            Some("Paste the CA certificate, or tick the box if the certificate is publicly trusted.".to_string())
        } else {
            None
        };
        let kept_ca = ca.is_none() && config.needs_ca();
        let entered = Entered { config, token: token.map(Into::into), ca: ca.map(Into::into) };
        if let Some(problem) = problem {
            // Whatever was typed into a secret field is not kept for the next form.
            self.entered = Some(Entered { token: None, ca: None, ..entered });
            self.problem = Some(problem);
            return self.show(Stage::Form, ctx);
        }

        // A stored credential cannot be read from here, so the host cannot be asked.
        if entered.token.is_none() || kept_ca {
            self.entered = Some(entered);
            return self.finish(
                "Saved. A stored credential was kept, so the host was not asked; approve VMs from the machines list.",
            );
        }
        let Some(grant) = ctx.grant else {
            self.entered = Some(entered);
            return self.fail(LIVE_NEEDED, None);
        };
        let token = entered.token.as_ref().map(|value| value.expose()).unwrap_or_default();
        let ca = entered.ca.as_ref().map(|value| value.expose());
        let host = match Host::primary(grant, &entered.config, token, ca, ctx.timeouts) {
            Ok(host) => host,
            Err(why) => {
                self.entered = Some(Entered { token: None, ca: None, ..entered });
                self.problem = Some(format!("{why}."));
                return self.show(Stage::Form, ctx);
            }
        };
        self.entered = Some(entered);
        match host.list_vms(PRIMARY_HOST).await {
            Ok(mut found) => {
                found.sort_by(|a, b| (&a.host, a.vmid).cmp(&(&b.host, b.vmid)));
                self.found = found;
                self.host = Some(host);
                self.show(Stage::Machines, ctx)
            }
            Err(error) => self.fail(format!("Proxmox could not be read: {error}."), Some(Retry::Form)),
        }
    }

    async fn choose_machines(&mut self, ctx: &SetupContext, ids: Vec<String>) -> SetupStep {
        let mut chosen: Vec<(String, u32)> = Vec::new();
        for id in ids {
            let vmid = self.found.iter().find(|m| machine_id(&m.host, m.vmid) == id).map(|m| m.vmid);
            let Some(vmid) = vmid else {
                return self.fail(format!("{id} was not in the list."), Some(Retry::Machines));
            };
            if !chosen.iter().any(|(known, _)| *known == id) {
                chosen.push((id, vmid));
            }
        }
        // One read of each chosen VM, so a missing privilege shows now and not later.
        let mut failure = None;
        if let Some(host) = &self.host {
            for (id, vmid) in &chosen {
                if let Err(error) = host.read_vm(*vmid).await {
                    failure = Some(format!("{id} could not be read: {error}."));
                    break;
                }
            }
        }
        if let Some(reason) = failure {
            return self.fail(reason, Some(Retry::Machines));
        }
        self.approved = chosen.into_iter().map(|(id, _)| id).collect();
        if self.console_options().is_empty() {
            return self.finish_with_consoles(Vec::new());
        }
        self.show(Stage::Consoles, ctx)
    }

    fn choose_consoles(&mut self, ids: Vec<String>) -> SetupStep {
        let options = self.console_options();
        if let Some(stray) = ids.iter().find(|id| !options.contains(id)) {
            return self.fail(format!("{stray} was not in the list."), Some(Retry::Consoles));
        }
        self.finish_with_consoles(ids)
    }

    fn finish_with_consoles(&mut self, ids: Vec<String>) -> SetupStep {
        self.consoles = ids;
        if let Some(entered) = self.entered.as_mut() {
            entered.config.console_targets = self.consoles.iter().map(|vm| ConsoleTarget { vm: vm.clone() }).collect();
        }
        let consoles = self.consoles.len();
        let approved = self.approved.len();
        let node = self.entered.as_ref().map(|e| e.config.node.clone()).unwrap_or_default();
        self.finish(&format!(
            "Connected to node {node}. {approved} {} approved, {consoles} with a console.",
            if approved == 1 { "VM" } else { "VMs" }
        ))
    }

    fn finish(&mut self, summary: &str) -> SetupStep {
        let Some(entered) = self.entered.take() else {
            return self.fail("Nothing was entered.", Some(Retry::Form));
        };
        let config: Value = match serde_json::to_value(&entered.config) {
            Ok(config) => config,
            Err(_) => return self.fail("The settings could not be stored.", Some(Retry::Form)),
        };
        if let Err(problem) = config::parse(&config) {
            return self.fail(format!("{problem}."), Some(Retry::Form));
        }
        let consoles = entered.config.consoles();
        let adopt: Vec<DeviceDecl> = self
            .approved
            .iter()
            .filter_map(|id| {
                let machine = self.found.iter().find(|m| machine_id(&m.host, m.vmid) == *id)?;
                let reference = MachineRef { host: machine.host.clone(), vmid: machine.vmid };
                Some(vm_decl(&reference, &machine.name, consoles.contains(id)))
            })
            .collect();
        let mut stored = Vec::new();
        if let Some(token) = entered.token {
            stored.push((TOKEN_SLOT.to_string(), token));
        }
        if let Some(ca) = entered.ca {
            stored.push((CA_SLOT.to_string(), ca));
        }
        self.stage = Stage::Done;
        SetupStep::Done(SetupResult { config, secrets: stored, adopt, summary: summary.to_string() })
    }
}

impl Default for ProxmoxSetup {
    fn default() -> Self {
        Self::new()
    }
}

/// A VM's name, or its number when it has none.
fn label(machine: &SeenMachine) -> String {
    let name = machine.name.trim();
    if name.is_empty() {
        format!("VM {}", machine.vmid)
    } else {
        name.to_string()
    }
}

#[async_trait]
impl SetupFlow for ProxmoxSetup {
    async fn begin(&mut self, ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        self.existing = existing.and_then(|config| config::parse(config).ok());
        self.show(Stage::Token, ctx)
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match (self.stage, input) {
            (Stage::Done, _) => self.show(Stage::Done, ctx),
            (stage, SetupInput::Poll) => self.show(stage, ctx),
            (Stage::Failed(back), SetupInput::Back) => self.show(back.stage(), ctx),
            (Stage::Failed(back), _) => self.fail("Go back to try that step again.", Some(back)),
            (Stage::Token, SetupInput::Values { .. }) => self.show(Stage::Form, ctx),
            (Stage::Token, SetupInput::Back) => self.show(Stage::Token, ctx),
            (Stage::Form, SetupInput::Values { values }) => self.submit_form(ctx, &values).await,
            (Stage::Form, SetupInput::Back) => self.show(Stage::Token, ctx),
            (Stage::Machines, SetupInput::Chosen { ids }) => self.choose_machines(ctx, ids).await,
            (Stage::Machines, SetupInput::Back) => self.show(Stage::Form, ctx),
            (Stage::Consoles, SetupInput::Chosen { ids }) => self.choose_consoles(ids),
            (Stage::Consoles, SetupInput::Back) => self.show(Stage::Machines, ctx),
            (stage, _) => {
                let back = match stage {
                    Stage::Token => Retry::Token,
                    Stage::Machines => Retry::Machines,
                    Stage::Consoles => Retry::Consoles,
                    _ => Retry::Form,
                };
                self.fail("That answer does not fit this step.", Some(back))
            }
        }
    }
}

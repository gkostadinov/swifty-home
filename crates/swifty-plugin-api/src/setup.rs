//! Setup flows: a small state machine inside a plugin that produces steps as data.
//!
//! The web wizard, `swifty plugin add` and any future app render the same steps
//! and send back the same inputs. The hub runs the flow, keeps it in memory, and
//! applies the result.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::examples::text;
use crate::{net, DeviceClass, DeviceDecl, Examples, Scalar, Secret};

/// A guided setup for one instance.
#[async_trait]
pub trait SetupFlow: Send {
    /// The first step. `existing` is the stored config when an instance is set up again.
    async fn begin(&mut self, ctx: &SetupContext, existing: Option<&Value>) -> SetupStep;
    /// The next step, given what the person entered. For a `Wait` step the hub
    /// calls this with `SetupInput::Poll` every `poll_ms` until it moves on.
    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep;
}

/// What a setup flow may use.
pub struct SetupContext {
    /// The id of the instance being set up.
    pub instance_id: String,
    /// `None` in dry-run. A flow that needs the network then returns `Failed`
    /// with a sentence saying live mode is needed. A form-only flow still works.
    pub grant: Option<net::LiveGrant>,
    /// How long to wait to connect and for each reply.
    pub timeouts: net::Timeouts,
    /// Which secret slots already hold a value, for "leave empty to keep".
    pub secrets_present: BTreeSet<String>,
}

/// One step of a setup flow, for the person to see.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum SetupStep {
    /// Ask for values.
    Form {
        /// The step's heading.
        title: String,
        /// What to do, in a sentence or two.
        text: String,
        /// The fields to fill in.
        fields: Vec<Field>,
    },
    /// Tell the person to do something physical and wait for it
    /// ("Press the button on the bridge", "Accept the prompt on the TV").
    Wait {
        /// The step's heading.
        title: String,
        /// What to do, in a sentence or two.
        text: String,
        /// How often the hub polls the flow, in milliseconds.
        poll_ms: u32,
        /// How long the hub waits before it gives up, in seconds.
        timeout_s: u32,
    },
    /// Pick from what was found (bridges, lights, units, VMs).
    Choose {
        /// The step's heading.
        title: String,
        /// What to do, in a sentence or two.
        text: String,
        /// What can be picked.
        options: Vec<Choice>,
        /// Whether more than one can be picked.
        multiple: bool,
        /// How many must be picked at least.
        min: u32,
    },
    /// Do something elsewhere first (create an API token, enable developer
    /// mode), then come back with values.
    External {
        /// The step's heading.
        title: String,
        /// What to do, in a sentence or two.
        text: String,
        /// Where to do it, when there is a page for it.
        link: Option<String>,
        /// The fields to fill in afterwards.
        fields: Vec<Field>,
    },
    /// Finished. The hub validates and stores the result.
    Done(SetupResult),
    /// It did not work. `retry` offers the previous step again.
    Failed {
        /// Why, as one sentence.
        reason: String,
        /// Whether the previous step can be tried again.
        retry: bool,
    },
}

/// One field of a form.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Field {
    /// The key its value is sent back under.
    pub key: String,
    /// The field's label.
    pub label: String,
    /// One sentence for the person filling it in.
    pub help: String,
    /// What it may hold.
    pub kind: FieldType,
    /// Whether it must be filled in.
    pub required: bool,
    /// A prefill. Never set for a secret.
    pub value: Option<Scalar>,
}

/// What a form field may hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldType {
    /// Text.
    Text {
        /// The most characters it may have.
        max_len: usize,
    },
    /// IP address or host name.
    Host,
    /// A URL.
    Url,
    /// A host and a port.
    HostPort,
    /// A port.
    Port,
    /// A whole number in a range.
    Int {
        /// The lowest value.
        min: i64,
        /// The highest value.
        max: i64,
    },
    /// `true` or `false`.
    Bool,
    /// One of these words.
    Choice {
        /// The words it may be.
        options: Vec<String>,
    },
    /// A MAC address.
    Mac,
    /// Masked on screen, sent once, never echoed, logged or audited.
    Secret,
    /// A longer paste, such as a CA certificate in PEM.
    Multiline {
        /// The most characters it may have.
        max_len: usize,
    },
}

/// One thing that can be picked in a `Choose` step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Choice {
    /// What is sent back when it is picked.
    pub id: String,
    /// Its name.
    pub label: String,
    /// One line more about it, such as its address.
    pub detail: String,
    /// Whether it starts picked.
    pub selected: bool,
}

/// What the person sent back for a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "input", rename_all = "snake_case")]
pub enum SetupInput {
    /// The values of a form, by key.
    Values {
        /// The values, by field key.
        values: BTreeMap<String, Scalar>,
    },
    /// What was picked in a `Choose` step.
    Chosen {
        /// The ids that were picked.
        ids: Vec<String>,
    },
    /// The hub asking a `Wait` step whether it is done.
    Poll,
    /// Go back one step.
    Back,
}

/// What a finished setup hands to the hub. `Debug` redacts `secrets`.
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SetupResult {
    /// The instance config. Must pass `Plugin::validate`. No secrets in it.
    pub config: Value,
    /// Slot and value. Stored by the hub, nowhere else.
    #[serde(skip)]
    pub secrets: Vec<(String, Secret)>,
    /// Devices to adopt at once (the lights that were ticked).
    pub adopt: Vec<DeviceDecl>,
    /// One line for the person: "Paired. 6 lights added."
    pub summary: String,
}

impl fmt::Debug for SetupResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetupResult")
            .field("config", &self.config)
            .field("secrets", &self.secrets.iter().map(|(slot, _)| (slot, "<redacted>")).collect::<Vec<_>>())
            .field("adopt", &self.adopt)
            .field("summary", &self.summary)
            .finish()
    }
}

impl Examples for FieldType {
    fn examples() -> Vec<Self> {
        vec![
            FieldType::Text { max_len: 64 },
            FieldType::Host,
            FieldType::Url,
            FieldType::HostPort,
            FieldType::Port,
            FieldType::Int { min: 1, max: 3600 },
            FieldType::Bool,
            FieldType::Choice { options: vec![text("eu"), text("us")] },
            FieldType::Mac,
            FieldType::Secret,
            FieldType::Multiline { max_len: 8192 },
        ]
    }
}

impl Examples for SetupStep {
    fn examples() -> Vec<Self> {
        let field = |key: &str, label: &str, help: &str, kind: FieldType, value: Option<Scalar>| Field {
            key: text(key),
            label: text(label),
            help: text(help),
            kind,
            required: value.is_none(),
            value,
        };
        let blind = DeviceDecl {
            local_id: text("blind-1"),
            target_hint: None,
            class: DeviceClass::Other,
            name: text("Blind 1"),
            capabilities: vec![crate::Capability::Custom { action: text("acme-blinds.calibrate") }],
            attrs: serde_json::json!({ "address": "192.0.2.21" }),
        };
        vec![
            SetupStep::Form {
                title: text("Where is the hub?"),
                text: text("Its address on your network, and the port it listens on."),
                fields: vec![
                    field("address", "Address", "Its IP address, such as 192.0.2.20.", FieldType::Host, None),
                    field(
                        "port",
                        "Port",
                        "Leave it as it is unless you changed it.",
                        FieldType::Port,
                        Some(Scalar::Int(8080)),
                    ),
                ],
            },
            SetupStep::Wait {
                title: text("Press the button on the hub"),
                text: text("Press the round button on top of the hub once. This page goes on by itself."),
                poll_ms: 2000,
                timeout_s: 90,
            },
            SetupStep::Choose {
                title: text("Which blinds?"),
                text: text("Tick the blinds to add."),
                options: vec![
                    Choice { id: text("blind-1"), label: text("Blind 1"), detail: text("192.0.2.21"), selected: true },
                    Choice { id: text("blind-2"), label: text("Blind 2"), detail: text("192.0.2.22"), selected: false },
                ],
                multiple: true,
                min: 1,
            },
            SetupStep::External {
                title: text("Create an access token"),
                text: text(
                    "Open the vendor's developer page, create a token with read and control rights, and paste it here.",
                ),
                link: Some(text("https://developer.example.org/tokens")),
                fields: vec![field("token", "Access token", "The token you created.", FieldType::Secret, None)],
            },
            SetupStep::Done(SetupResult {
                config: serde_json::json!({ "address": "192.0.2.20", "port": 8080 }),
                secrets: vec![(text("token"), text("not-a-real-token").into())],
                adopt: vec![blind],
                summary: text("Paired. 1 blind added."),
            }),
            SetupStep::Failed {
                reason: text("Pairing needs a hub in live mode. Run `swifty go-live`, then set this up again."),
                retry: false,
            },
        ]
    }
}

impl Examples for SetupInput {
    fn examples() -> Vec<Self> {
        vec![
            SetupInput::Values {
                values: [(text("address"), Scalar::Text(text("192.0.2.20"))), (text("port"), Scalar::Int(8080))].into(),
            },
            SetupInput::Chosen { ids: vec![text("blind-1")] },
            SetupInput::Poll,
            SetupInput::Back,
        ]
    }
}

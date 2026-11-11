//! The guided setup, which pairs the TV:
//!
//! 1. **Form**: the TV's address and the MAC address for Wake-on-LAN.
//! 2. **Choose**, for `wss` only: the flow connects once, sends nothing but the
//!    TLS handshake, and shows the SHA-256 of the certificate the TV presented
//!    (trust on first use). The person confirms it, and from then on only that
//!    certificate is accepted. A TV set up again that still presents the
//!    certificate stored for it skips this step.
//! 3. **Wait**: the flow connects, registers *without* a key and asks for a
//!    prompt. The TV shows it, and the person accepts it with the remote. The
//!    hub polls; each poll reads whatever the TV has said since.
//! 4. **Done**: the TV answers `registered` with a client key. The flow hangs up,
//!    reads the TV once with that key, and hands the key to the hub for the
//!    secret slot `client_key`. The key is never shown, logged or put in config.
//!
//! On `wss` every connection after the first is pinned to the certificate the
//! person confirmed, exactly as the driver's are: a TV that presents any other
//! certificate is never sent a registration.

use std::collections::BTreeMap;
use std::mem;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value};
use swifty_net::{self as net, device_text, Ws};
use swifty_plugin_api::{
    Choice, DeviceError, Driver, Field, FieldType, Scalar, Secret, SetupContext, SetupFlow, SetupInput, SetupResult,
    SetupStep, State,
};
use tokio::time::Instant;

use crate::config::WebosConfig;
use crate::driver::WebosDriver;
use crate::protocol::{self, REGISTER_ID};
use crate::{CLIENT_KEY, TV_ID};

/// How long the person has to accept the prompt, as the pairing script allowed.
pub const PROMPT_WAIT: Duration = Duration::from_secs(60);
/// How often the hub is asked to poll while the prompt is up.
const POLL_MS: u32 = 1_000;
/// How long one poll listens for the TV before it answers "not yet".
const LISTEN: Duration = Duration::from_millis(250);
/// The most messages one poll reads. A TV that floods is not waited for.
const MAX_MESSAGES_PER_POLL: usize = 16;

/// The fields of the form. The certificate pin is learned, not typed.
const KEYS: [&str; 3] = ["url", "wol_mac", "wol_broadcast"];

/// The id of the one choice on the certificate step.
pub const TRUST: &str = "trust";

/// A pin that only stands in while the rest of the form is checked.
const ANY_PIN: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A registration sent, and the TV not yet done answering it.
struct Pairing {
    ws: Ws,
    config: Value,
    parsed: WebosConfig,
    /// The TV has answered the registration: it showed the prompt.
    acknowledged: bool,
    deadline: Instant,
}

/// A certificate learned from the TV, waiting for the person to trust it.
struct Learned {
    config: Map<String, Value>,
    pin: String,
}

enum Stage {
    Form,
    Confirm(Box<Learned>),
    Waiting(Box<Pairing>),
}

/// The setup flow.
pub struct WebosSetup {
    stage: Stage,
    /// The values shown in the form, for going back to it.
    shown: BTreeMap<String, String>,
    /// The address and pin of the instance being set up again, if it had both.
    stored_pin: Option<(String, String)>,
    wait: Duration,
}

impl Default for WebosSetup {
    fn default() -> Self {
        Self::waiting_at_most(PROMPT_WAIT)
    }
}

/// What one poll found.
enum Heard {
    Nothing,
    Key(String),
    Failed(String),
}

fn text(values: &BTreeMap<String, Scalar>, key: &str) -> Option<String> {
    match values.get(key) {
        Some(Scalar::Text(text)) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    }
}

fn failed(reason: impl Into<String>) -> SetupStep {
    SetupStep::Failed { reason: reason.into(), retry: true }
}

fn needs_live() -> SetupStep {
    SetupStep::Failed {
        reason: "Pairing talks to the TV, so it needs the hub in live mode (live = true).".to_string(),
        retry: false,
    }
}

/// A pin as people compare fingerprints: upper-case pairs with colons.
pub fn fingerprint(pin: &str) -> String {
    let upper = pin.to_ascii_uppercase();
    let pairs: Vec<&str> = upper.as_bytes().chunks(2).filter_map(|pair| std::str::from_utf8(pair).ok()).collect();
    pairs.join(":")
}

/// The certificate step: what the TV presented, to trust or not.
fn confirm_step(address: &str, pin: &str, changed: bool) -> SetupStep {
    let changed = if changed {
        "This is not the certificate stored for this TV before: trust it only if the TV was reset or replaced. "
    } else {
        ""
    };
    SetupStep::Choose {
        title: "Check the TV's certificate".to_string(),
        text: format!(
            "The TV at {address} answered with a certificate whose SHA-256 fingerprint is {}. {changed}Trust it if \
             this is your TV: from now on only this certificate is accepted from it. Nothing else has been sent \
             to the TV yet.",
            fingerprint(pin)
        ),
        options: vec![Choice {
            id: TRUST.to_string(),
            label: "Trust this certificate".to_string(),
            detail: fingerprint(pin),
            selected: false,
        }],
        multiple: false,
        min: 1,
    }
}

fn wait_step(wait: Duration) -> SetupStep {
    SetupStep::Wait {
        title: "Accept the prompt on the TV".to_string(),
        text: "The TV now asks whether to allow a new connection. Accept it with the remote. \
               Nothing else is changed on the TV."
            .to_string(),
        poll_ms: POLL_MS,
        timeout_s: u32::try_from(wait.as_secs()).unwrap_or(u32::MAX).max(1),
    }
}

impl WebosSetup {
    /// A flow that gives the person `wait` to accept the prompt. [`PROMPT_WAIT`] by default.
    pub fn waiting_at_most(wait: Duration) -> Self {
        Self { stage: Stage::Form, shown: BTreeMap::new(), stored_pin: None, wait }
    }

    fn form(&mut self) -> SetupStep {
        self.stage = Stage::Form;
        let field = |key: &str, label: &str, help: &str, kind: FieldType, required: bool| Field {
            key: key.to_string(),
            label: label.to_string(),
            help: help.to_string(),
            kind,
            required,
            value: self.shown.get(key).cloned().map(Scalar::Text),
        };
        SetupStep::Form {
            title: "LG webOS TV".to_string(),
            text: "Turn the TV on and give it a fixed address in your router. The next step shows a prompt \
                   on the TV, which you accept with the remote."
                .to_string(),
            fields: vec![
                field(
                    "url",
                    "TV address",
                    "ws://<address>:3000, or wss://<address>:3001 for a TV that only offers TLS. For wss, the next \
                     step shows the TV's certificate for you to trust.",
                    FieldType::Url,
                    true,
                ),
                field(
                    "wol_mac",
                    "MAC address",
                    "Optional. The TV's MAC address, so that it can be turned on with Wake-on-LAN.",
                    FieldType::Mac,
                    false,
                ),
                field(
                    "wol_broadcast",
                    "Wake-on-LAN broadcast",
                    "Optional. Where the wake packet goes, as IP:port. 255.255.255.255:9 when empty.",
                    FieldType::HostPort,
                    false,
                ),
            ],
        }
    }

    async fn submit(&mut self, ctx: &SetupContext, values: &BTreeMap<String, Scalar>) -> SetupStep {
        self.hang_up().await;
        self.shown.clear();
        let mut config = Map::new();
        for key in KEYS {
            if let Some(value) = text(values, key) {
                self.shown.insert(key.to_string(), value.clone());
                config.insert(key.to_string(), Value::String(value));
            }
        }
        let wss = config
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|url| url.get(..6).is_some_and(|scheme| scheme.eq_ignore_ascii_case("wss://")));
        if !wss {
            return self.pair(ctx, config).await;
        }
        // Everything else in the form is checked before the TV is contacted.
        let mut checked = config.clone();
        checked.insert("cert_sha256".to_string(), Value::String(ANY_PIN.to_string()));
        let parsed = match WebosConfig::parse(&Value::Object(checked)) {
            Ok(parsed) => parsed,
            Err(problem) => return failed(problem.to_string()),
        };
        let Some(grant) = ctx.grant else { return needs_live() };
        let Ok(url) = parsed.url() else { return failed("url is not valid") };
        let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
            return failed("url names no TV");
        };
        let pin = match net::first_use_pin(grant, host, port, ctx.timeouts).await {
            Ok(pin) => pin,
            Err(error) => {
                return failed(format!(
                    "The TV could not be reached: {error}. Check that it is on, and that the address is the TV's \
                     secure port."
                ))
            }
        };
        let stored = self.stored_pin.as_ref().is_some_and(|(address, stored)| {
            Some(address.as_str()) == config.get("url").and_then(Value::as_str) && stored.eq_ignore_ascii_case(&pin)
        });
        if stored {
            config.insert("cert_sha256".to_string(), Value::String(pin));
            return self.pair(ctx, config).await;
        }
        let address = format!("{host}:{port}");
        let changed = self.stored_pin.is_some();
        self.stage = Stage::Confirm(Box::new(Learned { config, pin: pin.clone() }));
        confirm_step(&address, &pin, changed)
    }

    /// The person answered the certificate step.
    async fn trust(&mut self, ctx: &SetupContext, ids: &[String]) -> SetupStep {
        if !matches!(self.stage, Stage::Confirm(_)) {
            return failed("This step has nothing to choose.");
        }
        if !ids.iter().any(|id| id == TRUST) {
            // The question stays, so it can be answered again.
            return failed("The certificate was not trusted, so nothing was sent to the TV.");
        }
        let Stage::Confirm(learned) = mem::replace(&mut self.stage, Stage::Form) else {
            return failed("This step has nothing to choose.");
        };
        let Learned { mut config, pin } = *learned;
        config.insert("cert_sha256".to_string(), Value::String(pin));
        self.pair(ctx, config).await
    }

    /// Connects (pinned on `wss`), registers without a key, and waits for the prompt.
    async fn pair(&mut self, ctx: &SetupContext, config: Map<String, Value>) -> SetupStep {
        let config = Value::Object(config);
        let parsed = match WebosConfig::parse(&config) {
            Ok(parsed) => parsed,
            Err(problem) => return failed(problem.to_string()),
        };
        let Some(grant) = ctx.grant else { return needs_live() };
        let url = match parsed.url() {
            Ok(url) => url,
            Err(why) => return failed(why),
        };
        let unreachable = |error: DeviceError| {
            let pin_hint = if parsed.cert_sha256.is_some() {
                ", and that it still presents the certificate you trusted"
            } else {
                ""
            };
            failed(format!("The TV could not be reached: {error}. Check that it is on, the address{pin_hint}."))
        };
        let mut ws = match net::ws_connect(grant, &url, parsed.cert_sha256.as_deref(), ctx.timeouts).await {
            Ok(ws) => ws,
            Err(error) => return unreachable(error),
        };
        if let Err(error) = ws.send(protocol::register_for_pairing()).await {
            ws.close().await;
            return unreachable(error);
        }
        let deadline = Instant::now() + self.wait;
        self.stage = Stage::Waiting(Box::new(Pairing { ws, config, parsed, acknowledged: false, deadline }));
        wait_step(self.wait)
    }

    /// Reads what the TV has said since the last poll.
    async fn listen(pairing: &mut Pairing) -> Heard {
        for _ in 0..MAX_MESSAGES_PER_POLL {
            let text = match tokio::time::timeout(LISTEN, pairing.ws.recv()).await {
                Err(_) | Ok(Err(DeviceError::Timeout)) => return Heard::Nothing,
                Ok(Err(_)) => {
                    return Heard::Failed("The TV closed the connection before the prompt was accepted.".to_string())
                }
                Ok(Ok(text)) => text,
            };
            let Ok(message) = protocol::parse(&text) else {
                return Heard::Failed("The TV sent something that is not SSAP. Is this the TV's address?".to_string());
            };
            if protocol::id_of(&message) != Some(REGISTER_ID) {
                continue;
            }
            match protocol::type_of(&message) {
                Some("registered") => {
                    return match protocol::client_key(&message) {
                        Some(key) => Heard::Key(key),
                        None => Heard::Failed("The TV accepted, but sent a key that cannot be used.".to_string()),
                    }
                }
                // The TV took the request and is showing the prompt.
                Some("response") => pairing.acknowledged = true,
                Some("error") => {
                    let why = message.get("error").and_then(Value::as_str).unwrap_or("no reason given");
                    return Heard::Failed(format!("The TV refused: {}.", device_text(why)));
                }
                _ => return Heard::Failed("The TV sent an unreadable reply.".to_string()),
            }
        }
        Heard::Nothing
    }

    async fn poll(&mut self, ctx: &SetupContext) -> SetupStep {
        let Stage::Waiting(pairing) = &mut self.stage else {
            return failed("Nothing is waiting for the TV. Enter its address again.");
        };
        match Self::listen(pairing).await {
            Heard::Nothing if Instant::now() < pairing.deadline => wait_step(self.wait),
            Heard::Nothing => {
                let reason = if pairing.acknowledged {
                    "The TV showed the prompt, but nobody accepted it in time."
                } else {
                    "The TV took the connection but never answered, so it showed no prompt."
                };
                self.hang_up().await;
                failed(reason)
            }
            Heard::Failed(reason) => {
                self.hang_up().await;
                failed(reason)
            }
            Heard::Key(key) => {
                let Stage::Waiting(pairing) = mem::replace(&mut self.stage, Stage::Form) else { unreachable!() };
                let Pairing { ws, config, parsed, .. } = *pairing;
                ws.close().await;
                Self::finish(ctx, config, &parsed, key).await
            }
        }
    }

    /// One read of the TV with its new key, then the result for the hub.
    async fn finish(ctx: &SetupContext, config: Value, parsed: &WebosConfig, key: String) -> SetupStep {
        let Some(grant) = ctx.grant else {
            return failed("The hub left live mode during pairing.");
        };
        let key = Secret::new(key);
        let driver = match WebosDriver::new(grant, parsed, key.clone(), ctx.timeouts) {
            Ok(driver) => driver,
            Err(why) => return failed(why),
        };
        let summary = match driver.read_state(TV_ID).await {
            Ok(State::Tv { on: true, input }) if !input.is_empty() => format!("Paired. The TV is on {input}."),
            Ok(State::Tv { on: false, .. }) => "Paired. The TV says it is off.".to_string(),
            Ok(_) => "Paired. The TV is on.".to_string(),
            Err(error) => return failed(format!("The TV paired, but could not be read with its new key: {error}.")),
        };
        SetupStep::Done(SetupResult {
            config,
            secrets: vec![(CLIENT_KEY.to_string(), key)],
            adopt: Vec::new(),
            summary,
        })
    }

    async fn hang_up(&mut self) {
        if let Stage::Waiting(pairing) = mem::replace(&mut self.stage, Stage::Form) {
            pairing.ws.close().await;
        }
    }
}

#[async_trait]
impl SetupFlow for WebosSetup {
    async fn begin(&mut self, _ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        self.hang_up().await;
        self.shown.clear();
        self.stored_pin = None;
        if let Some(existing) = existing.and_then(Value::as_object) {
            for key in KEYS {
                if let Some(value) = existing.get(key).and_then(Value::as_str) {
                    self.shown.insert(key.to_string(), value.to_string());
                }
            }
            let text = |key: &str| existing.get(key).and_then(Value::as_str).map(str::to_string);
            self.stored_pin = text("url").zip(text("cert_sha256"));
        }
        self.form()
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match input {
            SetupInput::Values { values } => self.submit(ctx, &values).await,
            SetupInput::Poll => self.poll(ctx).await,
            SetupInput::Back => {
                self.hang_up().await;
                self.form()
            }
            SetupInput::Chosen { ids } => self.trust(ctx, &ids).await,
        }
    }
}

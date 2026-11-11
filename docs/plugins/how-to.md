# How to write a plugin

From an empty directory to a plugin merged into Swifty Home. The example is a plugin for
made-up "Acme Blinds"; replace the id, the name and the class with yours. Read
[contract.md](contract.md) once before you start: this page tells you what to do, that one
tells you why.

Code on this page is quoted from the template (`templates/plugin`) and from first-party
plugins by `cargo xtask gen-docs`, so it is what the repository holds today. In the
template, `{{Type}}` and the other words in double braces are filled in by
`swifty plugin new`; a block between `{{#local}}` and `{{/local}}` (or `{{#cloud}}`) is kept
for one kind of plugin and dropped for the other.

## 0. What you need

- A checkout of the swifty-home repository. Work inside it: the plugin goes in
  `plugins/<id>` and builds with the workspace. (Working elsewhere is possible; see the last
  section.)
- The Rust toolchain the repository pins in `rust-toolchain.toml`. `rustup` installs it on
  the first `cargo` command.
- The device's protocol: its documentation, captures of its traffic, or an open-source
  client and its licence. You will write down where everything came from.
- The device itself, for the last step. Everything before that runs against a stand-in on
  your own machine, so you can write and test the whole plugin without it.

## 1. Map the device onto the contract

Before any code, decide four things. [reference/capabilities.md](reference/capabilities.md)
has every option.

1. **The class of each kind of device** it offers: `light`, `plug`, `aircon`, `speaker`,
   `tv`, `feeder`, `litter`, `printer`, `camera`, `bridge`, `service`, `telemetry`, or
   `other` for anything else. `vm`, `host` and `router` exist too, but need the
   `MachineHost` extension and an approval for every device; start from `plugins/proxmox`
   for those.
2. **The capabilities** of each device: what it can do, among those the class has
   (`power`, `brightness { min }`, `color`, `inputs { inputs }`, ...). The lists inside a
   capability are the values this device accepts.
3. **The operations** you will handle: each standard action sends one `Op`
   ([reference/ops.md](reference/ops.md)), and each operation is confirmed from the class's
   `State` ([reference/states.md](reference/states.md)). Check that your device can report
   what that `State` needs; if it cannot report a field, that field is `None`.
4. **How it is reached**: the local network (which protocol, which port) or a vendor's
   cloud (which hosts, and whether the vendor documents the API for third parties).

Only when nothing in the catalog fits, plan a custom action
([contract.md](contract.md#custom-actions)). A thing the device does that no person needs
from the hub is simply left out.

## 2. Write the crate with `swifty plugin new`

From the root of the checkout:

```sh
cargo run -p swifty -- plugin new acme-blinds --class plug --name "Acme Blinds"
cargo test -p swifty-plugin-acme-blinds
```

The first command writes `plugins/acme-blinds`, a crate called `swifty-plugin-acme-blinds`
(library `swifty_plugin_acme_blinds`). The second runs its tests, which pass as written:
the template speaks a made-up protocol to a stand-in device on loopback, so you start from
a working plugin and change it one piece at a time. The first build also adds the crate to
`Cargo.lock`; that change belongs to your plugin and goes into its pull request.

- `--class` takes one or more classes, separated by commas: `--class light,plug`.
- `--cloud` makes a plugin that reaches its devices through a vendor's API with an access
  token, instead of the local network.
- `--name` is the name people see; without it, it is made from the id.

What you get:

| File | What it holds | Pure? |
| --- | --- | --- |
| `Cargo.toml` | The crate, with `swifty-plugin-api` and, for tests, `swifty-plugin-testkit` | |
| `README.md` | For people who use the plugin: what it supports, setup, what it reaches, what was tested on hardware | |
| `DEVELOPING.md` | For people who work on it: protocol notes, quirks, sources and their licences | |
| `src/lib.rs` | `MANIFEST`, `pub static PLUGIN`, and `impl Plugin` | yes, except `connect` |
| `src/config.rs` | The instance config as a typed struct, and its checks | yes |
| `src/protocol.rs` | What the device speaks: operations to bytes, replies to states, with unit tests | yes |
| `src/driver.rs` | `impl Driver`: sends the bytes through `swifty_plugin_api::net` | no |
| `src/setup.rs` | The guided setup: a form, a test read, done | no |
| `tests/common/mod.rs` | The stand-in device, and helpers the tests share | |
| `tests/mock_device.rs` | Each operation against the stand-in: what is sent, what is read, confirmed, silence, refusal | |
| `tests/conformance.rs` | The conformance suite, with this plugin's fixture | |

Work through the files in the order of the next sections. After each one,
`cargo test -p swifty-plugin-acme-blinds` should pass again.

## 3. The protocol: `src/protocol.rs`

Everything the device speaks, as pure functions: what to send for each operation, and what
a reply means. Nothing here opens a socket, so all of it is unit-tested without a device.

The template's stand-in protocol sends one JSON document and gets one back. Replace it with
your device's, keeping the same shape of functions: an `encode` that turns an `Op` into the
request (or says why it cannot), and a parser that turns a reply into fields or a
`DeviceError`. A reply is parsed defensively: anything unexpected is an error, never a
panic.

<!-- quote templates/plugin/src/protocol.rs "pub fn parse_reply" "=}" -->
```rust
pub fn parse_reply(reply: &[u8]) -> Result<Fields, DeviceError> {
    let value: Value = serde_json::from_slice(reply).map_err(|_| unreadable())?;
    let reply = value.as_object().ok_or_else(unreadable)?;
    match reply.get("ok").and_then(Value::as_bool) {
        Some(true) => match reply.get("state") {
            None => Ok(Fields::new()),
            Some(Value::Object(state)) => Ok(state.clone()),
            Some(_) => Err(unreadable()),
        },
        // What the device says it could not do is not repeated. The text of an error is fixed English
        // (a device's own words can hold anything); map its error codes to sentences of your own here.
        Some(false) => Err(DeviceError::Rejected(REFUSED.to_string())),
        None => Err(unreadable()),
    }
}
```
<!-- /quote -->

The stand-in's refusal carries its own reason, which the template passes on through
`device_text`. With a real device, do not: map each error code or status the documentation
lists to a fixed sentence of your own, and anything else to one general sentence
([contract.md](contract.md#errors)).

Write a unit test for every frame you encode and decode, built from the documentation, and
at least one for a malformed reply. Keep the tests the template has for the parts you keep.

Decoding into a `State`: fill each field from what the device said. A field the device
does not report is `None` where the variant has an `Option`; where it has none (a light's
`on`), a reply without it is unreadable, never a guess. A light in white mode reports
`color: false` and a `hue` that only tints the card
([contract.md](contract.md#confirmation-report-the-truth-and-the-hub-decides) has the values
the first-party plugins use).

## 4. The config and the manifest: `src/config.rs`, `src/lib.rs`

The **config** is what a person enters for one instance, stored as JSON. `config.rs` holds
it as a typed struct with `#[serde(deny_unknown_fields)]`, and checks it in one pure
function. A problem names its field, so a form can show it next to the right box:

<!-- quote templates/plugin/src/config.rs "pub fn parse(config: &Value)" "=}" -->
```rust
pub fn parse(config: &Value) -> Result<Config, ConfigProblem> {
    let parsed: Config = serde_json::from_value(config.clone()).map_err(|error| {
        // serde names the key only of a field that is unknown or missing ("unknown field `x`", "missing
        // field `id`"). For a value of the wrong type it names the value, not the key, so the problem is
        // put on `devices`, with serde's words.
        let text = error.to_string();
        let named = ["unknown field `", "missing field `"].iter().find_map(|start| text.strip_prefix(start));
        let field = named.and_then(|rest| rest.split('`').next()).unwrap_or("devices").to_string();
        problem(field, text)
    })?;
    check(&parsed)?;
    Ok(parsed)
}
```
<!-- /quote -->

`Plugin::validate` calls it, and so does every other call that needs the config. Never put
a secret in the config: declare a secret slot instead. Nothing about one
particular home goes in the code either: an address, a name, an account, a time zone all
come from config fields or setup steps.

serde's message names the key for a missing or an unknown field (`missing field \`id\``),
which is what the template takes as the problem's `field`. For a value of the wrong type it
names the value instead (`invalid value: integer \`70000\`, expected u16`), so the template
puts such a problem on `devices` with serde's own words rather than take the value for a
key. A field whose type a person can get wrong (a port, a number) is better read as a plain
JSON value and checked by hand, with the key in the problem.

The **manifest** describes the plugin before it runs: what it reaches, its config fields,
its secret slots, its custom actions, and the words voice may hear for it. Every field and
its rules are in [reference/manifest.md](reference/manifest.md). The WiZ plugin's is a good
model of a small one:

<!-- quote plugins/wiz/src/lib.rs "pub static MANIFEST: Manifest = Manifest {" "=};" -->
```rust
pub static MANIFEST: Manifest = Manifest {
    id: "wiz",
    name: "WiZ",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "WiZ smart bulbs on the local network: power, brightness and colour over UDP, with no account and no key.",
    reach: Reach::Local,
    network: &[NetUse::Udp { port: DEFAULT_PORT }, NetUse::UdpBroadcast { port: DEFAULT_PORT }],
    classes: &[DeviceClass::Light],
    config_version: 1,
    config: &[
        ConfigField {
            key: "bulbs",
            label: "Bulbs by address",
            help: "Bulbs you name by address. Bulbs found by discovery do not need to be listed.",
            kind: FieldKind::Rows { fields: BULB_FIELDS },
            required: false,
            default: None,
        },
        ConfigField {
            key: "port",
            label: "Port",
            help: "The UDP port of a bulb whose address names none.",
            kind: FieldKind::Port,
            required: false,
            default: Some("38899"),
        },
        ConfigField {
            key: "broadcast",
            label: "Broadcast address",
            help: "Where discovery sends its question, as IPv4 address:port: the top address of the bulbs' \
                   network, or 255.255.255.255:38899. Discovery is off until this is set.",
            kind: FieldKind::HostPort,
            required: false,
            default: None,
        },
    ],
    secrets: &[],
    actions: &[],
    vocabulary: &[("whiz", "wiz")],
    nouns: &[("wiz", DeviceClass::Light)],
    multi_instance: false,
};
```
<!-- /quote -->

Keep the manifest's `config` and the typed struct in step: conformance check 2 fails when a
declared key does not parse, when a required key can be left out, or when an unknown key is
accepted. A field is either `required` or has a `default`, never both.

## 5. Devices, readiness and refusal: `src/lib.rs`

Three pure calls describe an instance to the hub.

**`readiness`** says whether the instance can work at all: `Ready`; `NeedsSetup(reason)`
when it is configured but not usable yet (not paired, a required secret slot is empty);
`Disabled(reason)` when the config cannot work. Conformance needs `Ready` for its fixture.

**`devices`** lists every device the instance offers right now: one per config entry, plus
one for each adopted record it recognises. Each has a local id, a class, a name and its
capabilities. The mock plugin's shows both sources:

<!-- quote plugins/mock/src/lib.rs "fn devices(&self, instance" "=    }" -->
```rust
fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
    let Ok(config) = config::parse(instance.config) else {
        return Vec::new();
    };
    let mut decls: Vec<DeviceDecl> = config
        .devices
        .iter()
        .map(|row| DeviceDecl {
            local_id: row.id.clone(),
            target_hint: row.target.clone(),
            class: row.class,
            name: row.display_name(),
            capabilities: row.capability_list(),
            attrs: json!({}),
        })
        .collect();
    for record in instance.adopted {
        if !decls.iter().any(|decl| decl.local_id == record.local_id) {
            decls.push(DeviceDecl {
                local_id: record.local_id.clone(),
                target_hint: Some(record.target.clone()),
                class: record.class,
                name: record.name.clone(),
                capabilities: adopted_capabilities(record),
                attrs: record.attrs.clone(),
            });
        }
    }
    decls
}
```
<!-- /quote -->

A local id matches `[a-z0-9][a-z0-9_-]{0,63}` and never changes. Leave `target_hint` as
`None`: the hub then names the device `<instance>-<local id>`. Give a device a generic
default name ("Blind 1"); the owner renames it in the app.

**`refusal`** says why an operation will not be done on a device, in one sentence, or
`None`. It is pure, so dry-run shows the same refusals as live. Refuse an unknown device,
any operation the device cannot do, and any value it does not accept. The WiZ plugin's is
short:

<!-- quote plugins/wiz/src/lib.rs "pub fn refusal(config: &WizConfig" "=}" -->
```rust
pub fn refusal(config: &WizConfig, adopted: &[DeviceRecord], device: &str, op: &Op) -> Option<String> {
    if !matches!(op, Op::LightPower { .. } | Op::LightBrightness { .. } | Op::LightHue { .. } | Op::LightSet { .. }) {
        return Some("A WiZ bulb cannot do that.".to_string());
    }
    if is_discovered_id(device) {
        let known = adopted.iter().filter(|record| record.local_id == device).find_map(adopted_address);
        return known.is_none().then(|| format!("{device} is not a known light."));
    }
    config.address(device).is_none().then(|| format!("{device} has no address in this instance's bulbs."))
}
```
<!-- /quote -->

A useful pattern, which the template follows: let `protocol::encode` return
`Err(reason)` for anything it cannot encode, and let `refusal` call it. Then the pure
refusal and the driver can never disagree.

Values: refuse what the catalog allows and your device does not. A light whose lowest level
is 3 declares `Capability::Brightness { min: 3 }` and refuses 1 and 2, because the catalog's
range starts at 1 and conformance check 4 sends the value just under your minimum. Do not
send a lower request as the minimum ([contract.md](contract.md#confirmation-report-the-truth-and-the-hub-decides)
says why, and names the one exception).

`connect` builds the driver from `LiveContext` alone: the config, the secrets, the grant,
the timeouts and the directory of adopted devices. It reads nothing else, and returns a
`PluginError` when a required slot is empty (`ctx.secrets.require("token")?`).

## 6. The driver: `src/driver.rs`

The driver is the only code that talks to the device, and only a live hub builds it. Two
methods are required:

- `command(device, op)` sends one operation, **once**, and returns `Ok(CommandAck)` as soon
  as the device has accepted it. Before sending, it refuses a device that is not this
  instance's and an operation it cannot encode, with `DeviceError::Disabled`, sending
  nothing.
- `read_state(device)` reads the device's current state, in the `State` variant of its
  class.

The WiZ driver is the whole pattern in a few lines: find the device, encode, one exchange,
check the answer:

<!-- quote plugins/wiz/src/driver.rs "impl Driver for WizDriver" "=}" -->
```rust
impl Driver for WizDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        let message =
            protocol::set_pilot(op).ok_or_else(|| DeviceError::Disabled("A WiZ bulb cannot do that.".to_string()))?;
        let peer = self.address(device)?;
        protocol::accepted(&exchange(self.grant, peer, &message, self.wait).await?)?;
        Ok(CommandAck)
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        let peer = self.address(device)?;
        read_at(self.grant, peer, self.wait).await
    }

    async fn discover(&self, window: Duration) -> Result<Vec<DeviceDecl>, DeviceError> {
        let broadcast = self.config.broadcast().ok_or_else(|| {
            DeviceError::Disabled("No broadcast address is configured, so discovery has nowhere to send.".to_string())
        })?;
        let found = discover(self.grant, broadcast, window.min(MAX_DISCOVERY_WINDOW)).await?;
        Ok(found.iter().map(FoundBulb::decl).collect())
    }
}
```
<!-- /quote -->

Pick the helper of `swifty_plugin_api::net` that fits the device's transport; the table is
in [contract.md](contract.md#the-network-one-way-out). Pass it the grant and the timeouts
from `LiveContext` and nothing longer. Some details that trip people up:

- `net::tcp_exchange` reads until a blank line (`\r\n\r\n`), until the device closes the
  connection, or 8 kB. For a device that answers one line and keeps the connection open,
  use `net::tcp_open` and read from the `TcpLink` what the protocol says comes back (for a
  line, one byte at a time up to `\n`, with a limit).
- `net::Http::new` takes the device's base URL. End it with `/` when it has a path, or the
  last part is lost when a path is joined. `Http::url(path)` refuses any path that would
  leave that device, so a token can never be sent elsewhere.
- `Http::get`, `post` and `put` give the HTTP client's request builder: chain `.header(..)`,
  `.json(&value)` or `.body(..)` on it without naming its type, and pass it to
  `Http::send`. The reply's `status` has `as_u16()` and `is_success()`. The details are in
  [contract.md](contract.md#the-network-one-way-out).
- A header that carries a credential takes it on the line that sends it, and nowhere else,
  as in `.header("x-api-key", token.expose())`. Check such a value once, in `connect`: one
  that cannot go into a header (a line break in a stored key) would otherwise fail every
  request as `Disabled("the request is not valid")`, with nothing sent.
- An HTTP status other than success is your protocol's business: map 401 or 403 to
  `Rejected("the key was refused")`, 429 to a `Rejected` that says to slow down, and never
  copy the body into the error.

Optional methods: `discover` (next section), `health` (one cheap read, for the hub's status
page), `poll_refusal` (why a background read must wait, such as a cloud's rate limit), and
the `machines` and `agents` extensions for hypervisors and collectors.

## 7. Setup: `src/setup.rs`

A setup flow guides a person through adding an instance. It is a small state machine that
returns **steps as data**; the web app and `swifty plugin add` draw them. The template's
flow is Form, then one test read, then Done. Use what your device needs, and ask for as
little as possible:

| Step | Use it for |
| --- | --- |
| `Form` | Values: an address, a port, a choice. A `FieldType::Secret` field for a key |
| `Wait` | Something physical: press the button on the bridge, accept the prompt on the TV. The hub sends `Poll` every `poll_ms` until you return another step |
| `Choose` | Pick from what was found: bulbs, units, VMs |
| `External` | Do something elsewhere first (create a token on the vendor's site), then come back with values |
| `Done` | The config, the secrets, the devices to adopt, and one line of summary |
| `Failed` | It did not work: one sentence, and whether the previous step can be tried again |

Every step and field type, with its JSON and how it is drawn, is in
[reference/setup-steps.md](reference/setup-steps.md).

Pairing is a `Wait` step. The Hue plugin's waits for the link button:

<!-- quote plugins/hue/src/setup.rs "fn wait() -> SetupStep" "=    }" -->
```rust
fn wait() -> SetupStep {
    SetupStep::Wait {
        title: "Press the link button".to_string(),
        text: "Press the round button on top of the bridge. Swifty Home asks the bridge for a key every few \
               seconds and goes on by itself once the bridge agrees."
            .to_string(),
        poll_ms: POLL_MS,
        timeout_s: TIMEOUT_S,
    }
}
```
<!-- /quote -->

and each `SetupInput::Poll` asks the bridge once, returning the same `Wait` step until it
agrees. In dry-run there is no grant, and a step that needs the network says so:

<!-- quote plugins/hue/src/setup.rs "        if ctx.grant.is_none() {" "=        }" -->
```rust
if ctx.grant.is_none() {
    return SetupStep::Failed {
        reason: "Pairing the bridge needs live mode: in dry-run Swifty Home sends nothing. Set live = true \
                 and start the setup again."
            .to_string(),
        retry: false,
    };
}
```
<!-- /quote -->

Rules for every flow:

- **Secrets** go only in `SetupResult::secrets`, as `(slot, Secret)`. Never prefill a secret
  field. When an instance is set up again, `existing` is its stored config and
  `ctx.secrets_present` says which slots hold a value: an empty secret field then means
  "keep it", so leave that slot out of the result.
- **When live** (`ctx.grant` is `Some`), end by reading each chosen device once, and fail
  with a helpful reason (`retry: true`) if that read fails. The one case where you cannot:
  an instance set up again that keeps its stored key, because a flow never sees a stored
  value. Then finish without the read and say so in the summary, or offer to pair again, as
  the Hue plugin's form does with its "Pair again" box.
- **In dry-run** (`ctx.grant` is `None`), do what needs no network, and otherwise return
  `Failed` saying live mode is needed.
- The `config` in `Done` must pass `Plugin::validate`.
- Read what a person typed carefully: trim text, accept a number as `Scalar::Int` or as
  digits in `Scalar::Text`, and answer a missing value with the form again or a `Failed`
  with `retry: true`.
- A cloud the vendor does not document for third parties says so in its first step.

## 8. Discovery and adoption (when the device can be found)

If the device answers a broadcast or can list its peers, implement `Driver::discover`. It
returns candidates as `DeviceDecl`s and adopts nothing; the hub stores them and a person
picks. Put what the driver needs to find the device again in `attrs`, never a secret:

<!-- quote plugins/wiz/src/driver.rs "    pub fn decl(&self) -> DeviceDecl {" "=    }" -->
```rust
pub fn decl(&self) -> DeviceDecl {
    let id = discovered_id(&self.mac);
    DeviceDecl {
        local_id: id.clone(),
        target_hint: Some(id),
        class: DeviceClass::Light,
        name: format!("WiZ light {}", &self.mac[8..]),
        capabilities: light_capabilities(),
        attrs: json!({ "mac": self.mac, "address": self.address.to_string() }),
    }
}
```
<!-- /quote -->

Once adopted, the device comes back to the pure calls in `InstanceView::adopted` and to the
driver through `LiveContext::directory`, with those `attrs`:

<!-- quote plugins/wiz/src/driver.rs "pub fn adopted_address" "=}" -->
```rust
pub fn adopted_address(record: &DeviceRecord) -> Option<SocketAddr> {
    if !is_discovered_id(&record.local_id) {
        return None;
    }
    record.attrs.get("address").and_then(Value::as_str).and_then(|text| parse_address(text, protocol::DEFAULT_PORT))
}
```
<!-- /quote -->

A setup flow can offer what it found in a `Choose` step and return the chosen ones in
`SetupResult::adopt`.

## 9. Tests: `tests/`

Three layers, all of which run with `cargo test` and none of which need the device:

1. **Unit tests** in `protocol.rs` and `config.rs`: every frame, every config rule, a
   malformed reply.
2. **`tests/mock_device.rs`** against the stand-in in `tests/common/mod.rs`: for each
   operation, what is sent, the state read back, `op.is_confirmed_by(&state, baseline)`, a
   timeout against a silent device, and a refusal that sends nothing.
3. **`tests/conformance.rs`**: the conformance suite with this plugin's fixture.

Change the stand-in so it behaves like your device, including how it refuses.
[testing.md](testing.md) explains the test kit, how to write a stand-in for each kind of
transport, and what each conformance check needs from the fixture.

The template's conformance test calls `run_every_check`, which fails on anything that could
not run with the fixture, as every first-party plugin's does. Its stand-in is rigged: it can
spoil its replies and report a hostile name when the suite asks (`Switches`, in
`tests/common/mod.rs`), so all twelve checks run from the start. When you change the
stand-in to behave like your device, keep it rigged, or checks 10 and 12 will be reported as
not run (`run` alone would let that pass without a word).

## 10. Check it

```sh
cargo fmt -p swifty-plugin-acme-blinds
cargo clippy -p swifty-plugin-acme-blinds --all-targets -- -D warnings
cargo test -p swifty-plugin-acme-blinds
cargo xtask check-plugin acme-blinds
```

`check-plugin` runs the source checks (`#![forbid(unsafe_code)]`; no `std::net`,
`tokio::net`, `reqwest`, `tungstenite`, `process::Command` or `env::var` anywhere in `src/`
or `tests/`; no network client among the dependencies; a `tests/conformance.rs` that runs
the suite), then the tests, clippy, and `cargo deny` when it is installed. It must pass
with no warnings.

## 11. Run it in a hub

Add the plugin to the list of plugins compiled into `swifty`:

```sh
cargo xtask add-plugin plugins/acme-blinds
```

It adds a dependency and a feature to `crates/swifty/Cargo.toml` and one line to
`crates/swifty/src/plugins.rs`. Then start a hub of your own, with its files outside the
checkout:

```sh
cargo run -p swifty -- init --config ../swifty-dev/swifty.toml --data-dir ../swifty-dev/data \
    --origin http://localhost:8790 --yes
cargo run -p swifty -- serve --config ../swifty-dev/swifty.toml
```

`init` prints a one-time link to enrol the first passkey at `http://localhost:8790`. Add the
plugin under *Integrations*, or in a second terminal with
`cargo run -p swifty -- plugin add acme-blinds --config ../swifty-dev/swifty.toml`.

The hub starts in **dry-run**: every action is planned, refused or `simulated`, and nothing
is sent. Check that the devices appear with the right controls and that the refusals read
well. Then stop the hub, run `swifty go-live` with the same `--config` (or start over with
`init --live`), start it again, and try each action on the real device. Write down the
models and firmware you tried in `README.md`.

## 12. Write the two manuals

`swifty plugin new` wrote both with TODOs. Before review, every TODO is gone:

- **`README.md`**, for people who use it: what it supports (and what not), how setup goes,
  what it reaches on the network, what was and was not tested on real hardware, and for an
  unofficial cloud a plain warning.
- **`DEVELOPING.md`**, for people who work on it: the protocol as you understood it, its
  quirks, and every source you learned it from with its licence. Those go into the
  project's `NOTICE`. Copy no code or table from a source whose licence does not allow it.

## 13. Open the pull request

1. Run `cargo xtask gen-docs` and commit what it writes: the reference has a page for every
   first-party plugin (`docs/plugins/reference/plugins/acme-blinds.md`), and CI fails while
   it is missing or out of date.
2. Keep the `cargo xtask add-plugin` change in the same pull request, so the plugin is in
   the default build.
3. In the description, list every assumption the documentation did not settle, and what
   you checked on the real device.

Review looks at what no check can: that the tiers you asked for, `idempotent` and `Expect`
are honest; that `refusal` covers what the device refuses; that nothing about one home is
in the code; and that the plugin does nothing it does not need to.

## Working outside the repository

Run `swifty plugin new` anywhere outside a checkout and the crate goes in the current
directory, depending on `swifty-plugin-api` and `swifty-plugin-testkit` by version, with a CI
workflow of its own. Until those crates are published, build it from a checkout: in the
checkout, `cargo xtask add-plugin ../acme-blinds` adds it to the build and points
`swifty-plugin-api` at the checkout's copy (a `[patch.crates-io]` entry in the root
`Cargo.toml`). To ship it, open a pull request that adds the crate as `plugins/acme-blinds`
with its dependencies switched to `workspace = true`, as the other plugins have them.

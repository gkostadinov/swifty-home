# The contract

The rules every plugin follows, why each exists, and what it looks like in code. The exact
types are in the rustdoc of `swifty-plugin-api` and in [reference/](reference/README.md).
The code shown here is quoted from the repository by `cargo xtask gen-docs`, so it is the
code as it is today.

## The five rules

1. **The hub decides, the plugin drives.** A plugin never sees a role, a tier, a session or
   the audit log. It gets one validated `Op` for one device and says "accepted" or why not.
2. **Confirmation belongs to the hub.** `Op::is_confirmed_by` and `Expect` live in the API
   crate and the hub evaluates them on what `Driver::read_state` returns. A plugin cannot
   say "it worked".
3. **Four calls are pure**: `validate`, `readiness`, `devices`, `refusal`. They take the
   config, the names of the secret slots that hold a value, and the adopted devices. No
   socket, file, clock or secret value. They are all a dry-run hub ever calls.
4. **Only `connect`, the `Driver` and the `SetupFlow` can reach the network**, and only with
   the `LiveGrant` the hub hands over. Only a hub whose config says `live = true` has one.
5. **Everything is data.** Every type that crosses the boundary serialises, and the manifest
   is `static` data. The same contract can later be spoken to a child process
   ([bridge.md](bridge.md)).

## Who does what

| The hub | The plugin |
| --- | --- |
| Knows the people, their roles and their sessions | Knows nothing about people |
| Owns the catalog of actions, their tiers and their parameter ranges | Maps its device onto the classes, capabilities and operations of the catalog |
| Asks for a confirmation or a passkey before a risky action | Never asks anything of a person outside its setup flow |
| Checks every value against the action's range | Refuses what its particular device does not accept, in `refusal` |
| Sends an idempotent action a second time after silence, and nothing else ever | Sends each operation exactly once |
| Reads the state back and decides whether the command worked | Reports the state the device gives, nothing more |
| Holds the secrets and hands them over only when live | Declares secret slots and uses the values only to talk to the device |
| Assigns each device its public id (target) once, for good | Names its devices with stable local ids |
| Writes the audit log | Never sees it |

## The calls, in the order the hub makes them

```text
Plugin::manifest()                           at start: catalog, voice words, the app
Plugin::validate(config)              pure   before a config is stored or loaded
Plugin::migrate(from_version, config) pure   when the stored config_version is older
Plugin::readiness(instance)           pure   at every load
Plugin::devices(instance)             pure   at every load, and after adoption
Plugin::refusal(instance, device, op) pure   before every operation, dry-run and live
Plugin::connect(LiveContext)          live   once per load, only when live and ready
Driver::command / read_state          live   for each operation, then the read-back
Driver::discover / health / poll_refusal     when the hub asks
Plugin::setup() -> SetupFlow                 when a person adds or sets up an instance
```

Every driver call runs in a task of its own under the hub's deadline. A panic in a plugin
becomes `DeviceError::Rejected("the plugin failed")`, and three panics in five minutes mark
the instance failed. That limits shared fate. It is not isolation: a plugin runs in the
hub's process with the hub's rights.

## Tiers: not the plugin's business

Every standard action has a tier: 0 read, 1 comfort, 2 consequential (prepare, then
confirm), 3 critical (prepare, then a passkey), S safe direction (an emergency stop). The
plugin never implements any of it. It does not ask "are you sure", keep a list of who may
do what, or treat an operation differently because of who asked. The tier of every
standard action is in [reference/capabilities.md](reference/capabilities.md).

The one place a plugin touches a tier is a **custom action** (below): it requests one, and
the hub raises it to the floors and never lowers it. Ask for the highest tier that is
honest: anything irreversible, costly, or felt by other people is consequential or
critical.

## Confirmation: report the truth, and the hub decides

`Driver::command` returns `Ok(CommandAck)` when the device **accepted** the command. That
is not success. The hub then calls `read_state` until the state shows the change (by the
rule in `Op::is_confirmed_by`) or the action's timeout ends. The run is `confirmed`,
`no_response` or `failed`, and only a read-back can make it `confirmed`.

So `read_state` must say what the device says **now**:

- A value the device does not report is `None`, never a guess.
- Some fields have no `Option` (a light's `on`, `bri`, `hue` and `color`). A reply that lacks
  one of those is unreadable: return `DeviceError::Rejected`, do not make one up.
- Never fill in the value you last sent. A state built from what was sent confirms commands
  that did nothing, and the person is told something happened that did not.
- Do not cache a reading across commands unless the device rate-limits you, and then say so
  with `Driver::poll_refusal`.

**A light in white mode.** `State::Light` always has a `hue`. When the light shows white (or
a scene, or an effect) rather than a colour, set `color: false`: then `hue` only tints the
app's card and can never confirm a colour change. Give it a tint that matches the white:
the WiZ and Hue plugins use 34 for warm white (3500 K and below), 210 for cool white
(5000 K and above), and 40 in between or when the device gives no temperature.

**A light's lowest level.** Declare `Capability::Brightness { min }` with the lowest level
the device really takes, and refuse anything below it in `refusal`; the catalog's range
starts at 1. Do not send a lower request as `min`: the confirmation rule would then wait
for a level the device never reports. (One exception is built into the rule: a request under
10 is also confirmed by 10, for lights such as WiZ whose lowest level is 10 and which send
any lower request as 10.) The catalog never sends a brightness of 0 today: a scene that turns
a light off sends `light_power` instead.

Some operations need the state from **before** the command: a feeder's portion counter
must grow, a VM's uptime must drop. For those (`needs_baseline`) the hub reads first, and
sends nothing if that read fails. A key press (`tv_key`) cannot be read back at all: the
run ends `sent`.

How each operation is confirmed is listed in [reference/ops.md](reference/ops.md).

## Refusals: pure, complete, and the same in dry-run and live

`Plugin::refusal(instance, device, op)` returns `None` when this device will take this
operation, or one plain sentence a person can act on when it will not. It is pure, so a
dry-run hub shows exactly the refusals a live one would. It must refuse:

- a `device` that is not one of this instance's (configured or adopted);
- an operation the device cannot do at all (a colour on a white-only light);
- a value the catalog allows but this device does not (a brightness under its minimum, an
  input it does not have, a portion size its feeder does not dispense);
- anything on a device whose secret slot is empty (see [Secrets](#secrets)).

The catalog has already checked the value against the action's range, so `refusal` only
has to know this device. Here is a complete one, from the mock plugin. It looks at the
config and the adopted records, and at nothing else:

<!-- quote plugins/mock/src/lib.rs "fn refusal(&self, instance" "=    }" -->
```rust
fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
    let config = match config::parse(instance.config) {
        Ok(config) => config,
        Err(problem) => return Some(format!("The config does not validate: {problem}")),
    };
    if let Some(reason) = self.disabled(instance.id, &config) {
        return Some(reason);
    }
    let adopted = instance.adopted.iter().find(|record| record.local_id == device);
    match known_device(&config, device, adopted) {
        None => Some(NOT_HERE.to_string()),
        Some(known) => op_refusal(&known, op),
    }
}
```
<!-- /quote -->

The driver applies the same rules before it sends anything, because a refusal it only
knows live would make dry-run lie. Conformance check 5 compares the two: an operation that
`refusal` accepts must not come back from the driver as `DeviceError::Disabled`. When the
device itself turns a command down, that is `DeviceError::Rejected` with its reason made
safe; it depends on the device's state, so it cannot be known in advance.

`Op`, `State` and `Capability` are `#[non_exhaustive]`: a `match` ends in a `_` arm that
refuses, so an operation added to the contract later is refused by your plugin until you
support it.

## Dry-run: nothing leaves the hub

A hub starts in dry-run, and only the exact line `live = true` in its config makes it live.
In dry-run:

- `connect` is never called. The hub stands in for the driver, answers `refusal` from your
  pure function, and reports every accepted operation as `simulated`.
- The hub has no `LiveGrant`, and every function in `swifty_plugin_api::net` that opens a
  socket needs one. So even a mistake cannot reach a device.
- A setup flow gets `SetupContext::grant == None`. A step that needs the network returns
  `SetupStep::Failed` with a sentence saying that live mode is needed. A flow made only of
  forms still works, and its result is stored untested.

The pure calls are what make this work, so they stay pure: no socket, no file, no clock, no
randomness, no secret value. Conformance check 3 puts trap listeners on every address in
your test config and fails on any connection or datagram.

## The network: one way out

All network I/O goes through `swifty_plugin_api::net`, with the `LiveGrant` and `Timeouts`
from `LiveContext` (or `SetupContext`). That module is the `swifty-net` crate, re-exported:
the first-party plugins depend on `swifty-net` directly and write `swifty_net::`, which is
the same thing. A new plugin needs only `swifty-plugin-api`.

| Helper | For |
| --- | --- |
| `net::udp_exchange(grant, peer, payload, wait)` | One datagram, one reply from that peer |
| `net::udp_broadcast(grant, peer, payload)` | One broadcast, no reply (Wake-on-LAN, a nudge) |
| `net::udp_discover(grant, broadcast, payload, window)` | One broadcast, every reply within the window |
| `net::tcp_exchange(grant, host, port, request, timeouts)` | Connect, write one request, read until a blank line (`\r\n\r\n`), the device closing the connection, or 8 kB |
| `net::tcp_open(grant, peer, timeouts)` then `TcpLink::send` and `read_exact` | A framed protocol of the device's own; you decide how many bytes to read |
| `net::Http::new(grant, base_url, timeouts, None)` and `Http::with_ca_pem(..)` | HTTP and HTTPS to one device; `url(path)` can never leave that device; `get`, `post`, `put`, then `send` |
| `net::ws_connect(grant, &url, pin, timeouts)` | A websocket; `wss` needs the SHA-256 pin of the device's certificate |
| `net::first_use_pin(grant, host, port, timeouts)` | Trust on first use: the pin of the certificate a device presents, to show a person in setup |
| `net::resolve(grant, host, port, wait)` | The addresses a host name stands for |
| `net::device_text(text)` | Text from a device made safe to show and store |

Address types (`IpAddr`, `SocketAddr`, `Ipv4Addr`, ...) come from `swifty_plugin_api::net`
too, so a plugin never names `std::net`. (`core::net` holds the very same types and opens
nothing, and `check-plugin` allows it, but take them from `swifty_plugin_api::net`, as the
template does.) Every helper applies the timeouts, follows no redirect, uses no proxy, and
returns `DeviceError`, so `?` passes it on.

**HTTP in detail.** `Http::get`, `post` and `put` return the HTTP client's request builder.
Call its methods (`.header(name, value)`, `.json(&value)`, `.body(bytes)`) without naming
its type, and hand it to `Http::send`, which sends it once and reads the whole reply:

- `Reply::status` is the status (`reply.status.as_u16()`, `reply.status.is_success()`),
  `Reply::body` the bytes, `Reply::json()` and `Reply::text()` read them, and
  `Reply::retry_after` is the seconds of a `Retry-After` header.
- The whole request may take the connect timeout plus the read timeout. Each request opens a
  fresh connection. A reply over 1 MB is refused (`Http::with_max_body` changes that).
- A request that cannot be built, such as one with a header value that cannot be sent (a
  key with a line break in it), comes back as `Disabled("the request is not valid")`:
  nothing left the machine, and it is not silence, so the engine does not send it again.
  Still check such a value once, in `connect`, and refuse it there, so the person is told
  when the integration is set up and not at every command. Any other failure to send that
  is not a timeout comes back as `Unreachable`, which the hub treats as silence.
- What a status means is your protocol's business. Map it to a fixed sentence: 401 or 403 to
  `Rejected("the key was refused")`, 429 to a `Rejected` that says to slow down, a 5xx to a
  `Rejected` that says the device is busy. Never copy the body into the error.

**Ports.** Declare the port the device listens on in `network`. A config field may let a
person (or a test) give another one: the test kit's stand-ins listen on ports of their own,
and a plugin's tests cannot bind a socket. Say so in the README.

What a plugin never does with the network:

- **Retry.** Send each command once. The hub knows whether an action is idempotent; you do
  not. A helper's `Timeout` goes straight back to the hub.
- **Run a background task** that talks to the device: no polling loop, no keep-alive, no
  listener. The hub asks when it wants to know.
- **Open anything else**: `std::net`, `tokio::net`, `reqwest`, `hyper`, `tungstenite` or any
  other client. `cargo xtask check-plugin` fails on the names.
- **Reach something it did not declare.** The manifest's `network` lists every port,
  broadcast and cloud host, and the app shows it before setup.

## Secrets

A credential (an API key, a token, a vendor account's sign-in) lives in a **secret slot**:

1. The manifest declares it: `SecretSlot { slot: "token", .. }`, or `name/{key}` for one
   per config row.
2. The setup flow asks for it in a field of type `FieldType::Secret` and returns it in
   `SetupResult::secrets`. A person can also set one with `swifty plugin secret set`.
3. The hub stores it, outside the config, and hands it to `connect` in
   `LiveContext::secrets`.
4. The pure calls see only which slots hold a value: `InstanceView::secrets_present`.

A value is a `Secret`, whose `Debug` and `Display` print `<redacted>` and which does not
serialise. Call `expose()` only on the line that sends it. Never put a secret in the config,
a URL query, an error, a log line, a `Debug` output or a `State`: conformance check 9 runs
your plugin with a canary value in every slot and searches all of those for it.

A setup flow never sees a stored value. When an instance is set up again, a secret field
left empty means "keep the one you have": leave that slot out of the result. A flow that
keeps a stored key therefore cannot read the device with it; finish without the test read
and say so in the summary, as the Hue plugin does ("The bridge address is saved, and its key
is kept."), or offer to pair again.

**One secret per row.** With a slot `key/{id}`, each config row has its own value, stored as
`key/<that row's id>`. The Tuya plugin shows the convention: `readiness` is `NeedsSetup`
while no row's slot holds a value and `Ready` once one does, and `refusal` refuses each
device whose own slot is empty ("The key for blind-2 is not set."), so the others keep
working.

## Devices and their ids

A device has a **local id** that the plugin chooses: `[a-z0-9][a-z0-9_-]{0,63}`, stable for
as long as the device exists in the instance. `Driver::command` and `read_state` receive it.
The hub gives each device a public **target** once and stores it; the plugin may suggest one
with `target_hint`, which first-party plugins use to keep the ids of earlier versions. A new
plugin leaves `target_hint` empty, and the target becomes `<instance>-<local id>`.

Devices come from three places:

1. **The config.** `Plugin::devices` names them from the instance's config (a TV at an
   address, the units of an account). They are active at once.
2. **Discovery, then adoption.** `Driver::discover` returns candidates as `DeviceDecl`s, with
   what is needed to find each again in `attrs` (an address, a MAC, a number; never a
   secret). The hub stores them, a person adopts some, and the plugin gets each adopted
   device back in `InstanceView::adopted` and `LiveContext::directory`. A device can only be
   adopted from a stored candidate, so nobody can make the hub send to an address of their
   choosing. `devices` lists the adopted ones it recognises, and the driver finds them
   through the `Directory`.
3. **Pending.** A device of a class that needs approval (`vm`, `host`, `router`) is listed and
   inert until a passkey approves it.

## Text from devices

A name, a label or a status a device sends is untrusted text. Pass it through
`net::device_text` before it goes into a `State`, a `DeviceDecl` or a setup step (a name
offered as a default, a `Choice` label): it removes control characters and bounds the
length. Conformance check 10 makes the stand-in report a name full of control characters and
10 kB long, and checks what comes back from `read_state`, `discover` and the errors. It also
runs your setup flow twice, once with the stand-in's normal names and once with the hostile
one, and fails on any text a step shows now that it did not show before and that is not
clean (the label of a `Choose` option, a summary that names a device).

Errors are the exception: an error never carries what the device sent, cleaned or not (next
section).

## Errors

| Situation | Return |
| --- | --- |
| Nothing came back in time | `DeviceError::Timeout` (the helpers do this) |
| The device cannot be reached | `DeviceError::Unreachable(..)` (the helpers do this) |
| The device answered and said no, or answered with nonsense | `DeviceError::Rejected(reason)` |
| This device or this driver will not do it, decided before sending | `DeviceError::Disabled(reason)`, and `refusal` says the same |
| `connect` cannot build a driver (a slot is empty, the config is unusable) | `PluginError(sentence)`; `Secrets::require(slot)` does it for a slot |
| A config is wrong | `ConfigProblem { field, message }`, with the field's key or path |

**Error text is fixed English plus at most a number.** It may name what the config holds (a
device's local id, a config key), because that comes from the hub, not the device. It never
holds what the device sent: map the device's error codes or statuses to sentences of your
own ("the device is busy", "the key was refused", "brightness out of range"), and when you
cannot tell what went wrong, say that ("the device refused the command"). The template
does that: whatever its stand-in says when it refuses, the error is one fixed sentence
(`protocol::REFUSED`). What the hub does with each error is in
[reference/errors.md](reference/errors.md).

## Custom actions

When no standard operation fits, declare a `CustomAction` in the manifest, with an id
`<plugin id>.<verb>`, parameters, whether a repeat is harmless (`idempotent`), and how the
state shows it worked (`Expect`). Then:

- give each device that can do it `Capability::Custom { action: "<plugin id>.<verb>" }`;
- handle `Op::Custom { action, args }` in `refusal` and in the driver;
- report a `State::Custom { fields }` from `read_state`, with the field the `Expect` looks at.
  Only a `custom` state can meet an `Expect`, so a custom action on a device of a class
  with its own state (a `light`) can only be `Expect::Unreadable`, and its runs end `sent`.

The hub raises the requested tier to its floors (at least comfort; consequential when not
idempotent; critical on `vm`, `host` and `router`), makes the action owner-only, and never
lets voice or Swifty propose it. The mock plugin's `mock.set_level` is a complete example.

## What is checked, and where

| Rule | Checked by |
| --- | --- |
| Manifest grammar, version, API version, floors | Conformance check 1 |
| Config fields and the typed config agree | Conformance check 2 |
| The pure calls are pure | Conformance check 3 |
| Dry-run reaches nothing | Conformance check 4 |
| Refusals agree between dry-run and live | Conformance check 5 |
| Exactly one exchange per operation, no retry | Conformance check 6 |
| Timeouts are kept | Conformance check 7 |
| The state confirms each operation | Conformance check 8 |
| Secrets never leak | Conformance check 9 |
| Device text is cleaned and bounded | Conformance check 10 |
| Setup reaches `Done`, and contacts nothing in dry-run | Conformance check 11 |
| Malformed replies give errors, not panics | Conformance check 12 |
| No other client, no process, no environment, no `unsafe` | `cargo xtask check-plugin` |
| Licences and advisories of dependencies | `cargo deny`, in `check-plugin` |
| Honest tiers, `idempotent` and `Expect`; no site data; nothing hostile | Review |

[testing.md](testing.md) says how to make each check run against your plugin.

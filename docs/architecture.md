# Architecture

Swifty Home ships as **one Rust binary, `swifty`, with every integration compiled in
as its own plugin crate behind one typed contract, `swifty-plugin-api`.** The
contract is written so that it can later be carried over a wire unchanged. The web
app is embedded in the binary as plain ES modules with no build step, and draws
itself from what devices can do. The HTTP API lives under `/api/v1`, with an OpenAPI
document and a Server-Sent Events stream.

```
 browser / phone ──https──▶ reverse proxy ──http, loopback──▶ swifty serve
                                                               │
            ┌──────────────────────────────────────────────────┤
            │ swifty-hub                                        │
            │   HTTP API v1, sessions, passkeys, people         │
            │   catalog ─▶ policy ─▶ tiers ─▶ action engine ─▶ audit log
            │   voice, Swifty, rooms, scenes, events            │
            │   Registry and DeviceIndex                        │
            └──────────────┬───────────────────────────────────┘
                           │ one checked Op for one device
            ┌──────────────▼───────────────────────────────────┐
            │ plugins/*: wiz, hue, sonos, webos, tuya, ...      │
            │   pure: validate, readiness, devices, refusal     │
            │   live only: connect → Driver, SetupFlow          │
            └──────────────┬───────────────────────────────────┘
                           │ only with a LiveGrant
                    swifty-net ──▶ your devices
```

## Why plugins are compiled in

1. **It is the shape that kept every behaviour and every test.** The device code
   already sat behind one trait, opened sockets in one place and had pure refusal
   rules. Turning each adapter into a crate was mostly moving files. A process
   supervisor or a WebAssembly host would be a new subsystem with its own failures,
   and neither could easily carry the VNC console relay, UDP broadcast or pinned TLS.
2. **The compiler checks the contract.** A change to the API breaks every affected
   plugin in the same build.
3. **The security model does not move.** Tiers, confirmation, passkey step-up, the
   audit chain and dry-run all stay in hub code that plugins cannot reach. A dry-run
   hub never connects a plugin.
4. **It leaves a clean path.** Every contract type is plain data, the pure calls
   take only data, and setup is a list of declarative steps. A later `bridge` plugin
   can speak the same shapes as JSON-RPC to a child process, which brings plugins
   without a rebuild and in other languages without changing the hub.

**The honest costs.** Plugins are Rust only. A plugin outside the official build
needs a rebuild. A plugin runs in the hub's process with the hub's rights: review,
lints and the conformance suite catch mistakes, not malice. See
[Security](security.md#plugins-trust-said-plainly).

Also considered and not done now: out-of-process plugins (a supervisor, framing,
sandboxing and signing are each a project; this is the roadmap), WebAssembly
components (no UDP broadcast or custom TLS verification in WASI yet), and a
framework rewrite of the web app (pure risk while everything else moved).

## The repository

| Path | What |
| --- | --- |
| `crates/swifty-plugin-api` | **The contract**: classes, capabilities, operations, states, confirmation rules, tiers, manifest, the `Plugin`, `Driver` and `SetupFlow` traits, secrets, errors |
| `crates/swifty-net` | The only code that opens a socket to a device: HTTP, UDP, TCP, WebSocket, the console link. Every function needs a `LiveGrant` |
| `crates/swifty-hub` | The hub library: configuration, store, secret store, registry, catalog, policy, action engine, audit, passkeys, people, rooms, scenes, voice, Swifty, events, HTTP API, migration, admin |
| `crates/swifty` | The binary: `swifty serve` and the command line, and `plugins.rs`, the list of compiled-in plugins |
| `crates/swifty-plugin-testkit` | Loopback mock devices, trap listeners, canary secrets and the conformance suite |
| `plugins/<id>` | One crate per integration: `README.md` for users, `DEVELOPING.md` for developers |
| `web/` | The embedded web app |
| `services/swifty-speech` | The optional Windows speech-to-text service, built on its own |
| `templates/plugin` | What `swifty plugin new` writes |
| `packaging/` | Install script, systemd units, Docker, Caddy and nginx ([packaging/README.md](../packaging/README.md)) |
| `xtask/` | Developer tasks: test counts, generated docs, OpenAPI, plugin checks, release archives |

## The contract

### Rules it enforces

1. **The hub decides, the plugin drives.** A plugin gets one validated `Op` for one
   device and says accepted or why not. It never sees a role, a tier, a session or
   the audit log.
2. **Confirmation belongs to the hub.** `Op::is_confirmed_by` and, for custom
   actions, `Expect` live in the contract crate and are evaluated by the engine on
   what `read_state` returns. A plugin cannot say "it worked".
3. **Four calls are pure**: `validate`, `readiness`, `devices` and `refusal` take the
   config, the names of the secret slots that hold a value, and the adopted devices.
   No socket, file, clock or secret value. They are all a dry-run hub ever calls.
4. **Only `connect`, the `Driver` and `SetupFlow` reach the network**, and only with
   the `LiveGrant` the hub hands over.
5. **Everything is data.** Every type that crosses the boundary serialises, and the
   manifest is static data, so the same contract can later be spoken to a child
   process.

### Classes and capabilities

A device has a **class** (light, bridge, plug, aircon, speaker, tv, feeder, litter,
printer, vm, host, router, camera, service, telemetry, other) and a list of
**capabilities** (power, brightness with its minimum, colour, volume, inputs, modes,
dispense portions, lifecycle, console, and so on). The catalog, policy, voice,
Swifty and the web app work from those, never from a vendor or the shape of a name.
The class also sets the default audience: lights, bridges, speakers, TVs, pets and
cameras are for the family; everything else starts owner-only. Computers (vm, host,
router) always start pending and need approval.

| Class | Capability | Action | Tier |
| --- | --- | --- | --- |
| any | | `state.read` | 0 |
| light | Power, Brightness, Color | `light.power`, `light.brightness`, `light.hue`; `lights.*`, `room.*`, `scene.apply` | 1 |
| speaker | Volume, Mute, NightMode, Transport | `speaker.volume`, `.mute`, `.night`, `.play`, `.pause` | 1 |
| tv | Inputs, Keys / Power | `tv.input`, `tv.key` / `tv.power`, `scene.movie` | 1 / 2 |
| plug | Power | `plug.power` | 1 |
| aircon | Power, Mode, Temperature, Fan | `ac.power`, `ac.mode`, `ac.temperature`, `ac.fan` | 1 |
| feeder, litter | Dispense, Clean | `pets.feed`, `pets.clean` | 2 |
| printer | PrintJob / EmergencyStop | `print.start`, `.pause`, `.resume` / `print.estop` | 2 / S |
| vm | Lifecycle, Console | `vm.start` / `vm.stop`, `vm.restart`, `vm.force_stop`, `console.open` | 2 / 3 |
| host | NodePower | `host.shutdown`, `host.reboot` | 3 |
| router | Tunnel | `router.wireguard_restart` | 3 |
| telemetry | AgentPairing | `agent.pair` | 3 |
| hub | | `passkey.*`, `user.*`, `machine.approve`, `machine.remove`, `plugin.add`, `plugin.reconfigure`, `plugin.remove`, ... | 3 |
| a plugin's | Custom | `<plugin>.<verb>` | at least the floor |

Older action ids (`bulb.*`, `sonos.*`) are still accepted as aliases. The generated
reference with every operation, state and timeout is in
[docs/plugins/reference](plugins/reference/capabilities.md).

**Custom actions.** When nothing fits, a plugin declares its own action in its
manifest, with a requested tier, a parameter schema, whether it is idempotent, and an
`Expect` rule saying how the reading shows it worked. The hub raises the tier to its
floors (at least comfort; consequential if not idempotent; critical on computers;
never read or safe-direction). Custom actions are owner-only and never offered by
voice or Swifty.

### The manifest

Everything the hub and a person can know about a plugin before it runs: id, name,
version, contract version, summary, **reach** (local, or a vendor cloud and whether
it is official), every **network use** (ports, broadcast, Wake-on-LAN, cloud hosts),
classes, config fields with labels and help, secret slots, custom actions, and
vocabulary for voice (mishearings and brand nouns: words only, never grammar). The
app shows it before setup, `swifty plugin info` prints it, and the developer
reference is generated from it.

### The traits

- **`Plugin`**: the manifest, the four pure calls, `migrate` for older stored
  configs, `connect` (live only, builds the `Driver`), and `setup`.
- **`Driver`**: `command` sends one operation once (never retries: only the engine
  knows whether repeating is safe) and `read_state` reads the real state. Optional:
  `discover`, `health`, `poll_refusal` (rate limits), and two extensions,
  `MachineHost` (VM lists, charts, the console, further hosts) and `AgentPairing`.
- **`SetupFlow`**: a small state machine that produces **steps that are data**:
  `form`, `wait` ("press the button on the bridge"), `choose`, `external` ("create a
  token there, then come back"), `done` and `failed`. The web wizard and
  `swifty plugin add` draw the same steps. A finished setup hands the hub a config,
  the secrets and the devices to adopt; the hub validates, stores and audits it in
  one transaction. In dry-run a setup gets no grant and can only do what needs no
  network.

### Devices and targets

A plugin describes each device with a local id, a class, a name, capabilities and
the attributes it needs to find the device again. The hub gives each device a
**target**, its public id, once, and stores it: the plugin's hint if it is free,
otherwise `<instance>-<local id>`. Targets are what the API, rooms, favourites and
the audit log name, and they never change.

Devices come from three places: named in an integration's config (active at once,
because writing a config already took the owner's passkey); discovered and then
adopted from a stored candidate (so nobody can name an address for the hub to send
to); or pending, for computers, until `machine.approve`.

### The hub side

The **Registry** loads integrations from the database. In dry-run each is a stand-in
that answers with the plugin's pure refusals. In live mode it connects the plugin,
reads its secrets with the grant, and a failure disables only that integration. The
**DeviceIndex** holds every device by target, with class, capabilities, audience
and status; it is rebuilt and swapped in whole after any change. Every driver call
runs in its own task under the engine's deadline, and a panic becomes a failure of
that call.

The command line changes integrations by writing the same database; the running hub
notices within two seconds and reloads, with no restart.

### Voice, Swifty and the words of a house

The grammar, verbs, negation handling and tier routing stay in hub code. The words
are built per request from the device index, the rooms and the manifests' words, so
a renamed device is understood by its new name, and no plugin can add a verb or make
"turn off" mean a host shutdown.

## Data

`swifty.toml` holds what the hub needs before it opens its database. SQLite holds
everything arranged in the app. The secret store holds credentials, one file per
value. See [Configuration](configuration.md).

## The API

Everything is under `/api/v1/`. Requests are strict (unknown fields are refused),
responses may grow, and within v1 nothing is removed or renamed. Every response
carries `X-Swifty-Api: 1`. `GET /api/v1/meta` tells a client what the hub supports
before it signs in, `GET /api/v1/openapi.json` is the document, and
`GET /api/v1/events` is the event stream. Details in [docs/api](api/README.md).

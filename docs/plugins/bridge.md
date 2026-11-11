# The bridge: plugins outside the binary (roadmap)

**Not built.** This page describes the next step for plugins, so that what is built now
does not close it off. Nothing here is a promise of names or dates.

Today a plugin is a Rust crate compiled into `swifty`. That keeps one binary, lets the
compiler check every plugin against the contract, and keeps the security model in hub code.
It also means a plugin is Rust only, and that using one outside the official build takes a
rebuild.

The bridge removes both limits without changing the hub's core: a first-party plugin called
`bridge` that implements `Plugin` and `Driver` by speaking the same contract to a child
process, as JSON-RPC 2.0 over the child's standard input and output.

## Why the contract already fits

- Every type that crosses the boundary is plain data with a fixed JSON form: the samples and
  their JSON are in [reference/](reference/README.md), and `cargo xtask gen-docs --check`
  keeps them true.
- The four pure calls take only the config, the names of the slots that hold a secret, and
  the adopted devices, so they can be asked of a process that has nothing else.
- Setup is a list of steps that are data, drawn by the hub, so a plugin in another language
  draws nothing.
- Confirmation, tiers and policy never were the plugin's, so none of them would cross the
  wire.

## The wire, as it would be

One JSON-RPC request per call, with the names below. Parameters and results are the JSON of
the reference pages.

| Method | Params | Result | The trait call it stands for |
| --- | --- | --- | --- |
| `manifest` | none | the manifest's JSON | `Plugin::manifest` |
| `validate` | `{ config }` | `null`, or a `ConfigProblem` as the error's data | `Plugin::validate` |
| `migrate` | `{ from_version, config }` | the config | `Plugin::migrate` |
| `readiness` | `{ instance }` | a `Readiness` | `Plugin::readiness` |
| `devices` | `{ instance }` | a list of `DeviceDecl` | `Plugin::devices` |
| `refusal` | `{ instance, device, op }` | a sentence, or `null` | `Plugin::refusal` |
| `connect` | `{ instance_id, config, secrets, timeouts }` | `null`, or a `PluginError` | `Plugin::connect` |
| `command` | `{ device, op }` | `null`, or a `DeviceError` | `Driver::command` |
| `read_state` | `{ device }` | a `State`, or a `DeviceError` | `Driver::read_state` |
| `discover` | `{ window_ms }` | a list of `DeviceDecl` | `Driver::discover` |
| `health` | none | a `Health` | `Driver::health` |
| `poll_refusal` | none | a sentence, or `null` | `Driver::poll_refusal` |
| `directory` | `{ devices }`, sent by the hub | none (a notification) | `LiveContext::directory` kept current |
| `setup.begin` | `{ instance_id, existing, live, secrets_present }` | a `SetupStep` | `SetupFlow::begin` |
| `setup.advance` | `{ input }` | a `SetupStep`, whose `done` carries its secrets as `{ slot: value }` | `SetupFlow::advance` |

`instance` is `{ id, config, secrets_present, adopted }`, the JSON of `InstanceView`.

## What would change, and what would not

- **Secrets** cross in two places only: in `connect` (the values the instance holds) and in a
  finished setup (the values a person entered). They are never logged on either side.
- **Dry-run** stays structural: a dry-run hub would start the child for the pure calls and
  setup forms only, and would never send `connect`. The child would be started without
  network access where the operating system allows it, and with it only when live.
- **The network rules** become the child's to keep, and the hub could no longer prove them
  with a lint. The manifest's `network` list would become what the hub allows the child,
  enforced by the sandbox rather than trusted.
- **The conformance suite** would run against the bridge unchanged, which is the point: a
  plugin in another language passes the same twelve checks.
- **The console relay and the other extensions** (`MachineHost`, `AgentPairing`) would come
  last, if at all: they carry live byte streams that JSON-RPC over pipes does not suit.

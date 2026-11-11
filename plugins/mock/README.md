# Mock devices (`swifty-plugin-mock`)

Pretend devices of every class that live in the hub's memory. Use it to try
Swifty Home out with no hardware (`swifty init --demo`), and in tests. It is
also the smallest complete plugin, so it is a good one to read first.

## What it controls

Nothing real. Each device is a row in the instance config with a class and,
optionally, its own list of capabilities. The driver keeps a state for each
device in memory, applies every operation the device's capabilities allow, and
reads the state back, so the hub confirms runs exactly as it does for real
devices.

| Class | Usual capabilities | Starting state |
| --- | --- | --- |
| light | power, brightness (from 10 %), colour | off, 100 %, hue 30 |
| plug | power | off |
| aircon | power, mode (auto, cool, heat, dry, fan), temperature 17 to 30 °C, fan (auto, quiet, low, medium_low, medium, medium_high, high) | off, auto, 21 °C |
| speaker | volume, mute, night mode, play and pause | volume 20, paused |
| tv | power, inputs (HDMI 1 to 4, Live TV), the remote keys | off, HDMI 1 |
| feeder | dispense 10, 20, 30, 40 or 50 g | no portions yet |
| litter | clean | no cycles yet |
| printer | print start, pause, resume, emergency stop | idle |
| vm | start, shut down, reboot, force stop, console, history | running |
| host | shut down, reboot, history | up |
| router | tunnel restart | tunnel up |
| telemetry | metrics | no readings |
| bridge, camera, service | none (read-only) | reachable |
| other | the custom action `mock.set_level` | level 0 |

The custom action `mock.set_level` sets a gadget's `level` (0 to 10). It is
there so the hub's path for plugin-declared actions is exercised: it is
confirmed by its `Expect` (`level` in the state equals the `level` argument).

Refusals follow the capabilities: an operation a device's capabilities do not
cover, or a value outside them (a brightness under the minimum, an input the TV
does not list, a portion the feeder does not dispense), is refused in dry-run
and live alike, and nothing is sent.

## Setup

1. **Choose** the demo devices to add (floor lamp, ceiling light, kettle plug,
   speaker, TV, air conditioner, cat feeder, gadget). Setting an instance up
   again keeps the devices that are not demo devices.
2. **Form**: an optional access code. It is stored in the secret store like any
   plugin's credential, and never shown.
3. **Done**: the demo devices are in the config, so the hub lists them at once.

The flow reaches nothing, so it works the same in dry-run. There is no device
to read at the end.

## Config fields

| Key | Required | What it is |
| --- | --- | --- |
| `devices` | yes | One row per device (may be empty). |
| `devices[].id` | yes | The local id, `[a-z0-9][a-z0-9_-]{0,63}`, unique in the instance. |
| `devices[].class` | yes | One of the classes above. |
| `devices[].name` | no | Its name. Left out, "Mock light" and so on. |
| `devices[].target` | no | The target to ask the hub for. Left out, the hub picks `<instance>-<id>`. |
| `devices[].capabilities` | no | Rows of `cap` plus `min`, `max`, `options` (comma-separated) or `action`, as each capability needs. Left out, the usual set; an empty list makes the device read-only. |
| `disabled` | no | A sentence. When set, the instance is disabled with it as the reason. |
| `require_code` | no, default `false` | When true, the instance needs its access code before it is ready. |

```json
{
  "devices": [
    { "id": "lamp", "class": "light", "name": "Floor lamp" },
    { "id": "tv", "class": "tv", "capabilities": [
      { "cap": "power" }, { "cap": "inputs", "options": "HDMI 1, HDMI 2" }
    ] },
    { "id": "gadget", "class": "other" }
  ]
}
```

## Secret slots

| Slot | Required | What it is |
| --- | --- | --- |
| `access_code` | no | A stand-in credential. Needed only when `require_code` is true. |

## For tests

- `PLUGIN` gives each instance a fresh world of its own when it connects.
  `MockPlugin::bound(&world)` returns a plugin whose instances all share the
  `MockDevices` world a test holds. That world has the helpers the hub's
  tests use: `set_behavior` (`Normal`, `Silent`, `Hang`, `AcceptsButIgnores`,
  `Rejects`, `Slow`), `set_disabled` (by instance), `add_state`,
  `set_collector`, `add_seen_machine`, `add_discoverable`, `state`, `commands`,
  `command_count`, `total_commands` and `total_reads`. A bound plugin also
  answers readiness and refusals from `set_disabled`.
- `legacy_house()` is today's 20-device house as mock instances: one instance
  per old adapter kind (`wiz`, `hue`, `sonos`, `webos`, `moonraker`, `plex`,
  `camera`, `petkit`, `proxmox`, `collector`, `router`, `tuya`, `toshiba`),
  today's targets as local ids and target hints, four generic VMs adopted under
  `proxmox`, the four house scenes with their per-bulb levels, and two generic
  notes. `legacy_house_with_vms` takes another list of VMs.
  `MockDevices::new()` holds the states that house starts in.
- `demo_home()` is the instance `swifty init --demo` adds.

## Limits

- State lives in memory. A restart, or a reload of the instance, starts the
  devices again from their starting states (a bound world survives reloads).
- VM consoles and further hosts are not supported; the console calls answer
  "This device cannot do that."
- The config form describes value lists as comma-separated text, because the
  manifest has no list field.

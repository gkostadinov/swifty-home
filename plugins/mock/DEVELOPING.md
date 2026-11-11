# Developing the mock plugin

## Sources and licences

There is no protocol: the devices are in memory. Nothing in this crate came
from another project. The code was moved from the hub's own
the hub's former `adapters/mock.rs` (Apache-2.0, this repository), and the
lists of TV inputs, remote keys, air-conditioner modes and fan speeds, and feeder
portions are the hub catalog's own ranges. Nothing needs a line in `NOTICE`.

## Layout

| File | What |
| --- | --- |
| `src/lib.rs` | `MANIFEST`, `PLUGIN`, `MockPlugin::bound`, `impl Plugin`, and `op_refusal`, the pure refusal the driver also applies |
| `src/config.rs` | `MockConfig`, its rows, the capability rows, the class defaults and `parse`/`check` |
| `src/driver.rs` | `MockDevices` (the world), `Behavior`, `MockDriver` with `MachineHost` and `AgentPairing` |
| `src/setup.rs` | `MockSetup`: Choose, then the access code, then Done |
| `src/house.rs` | `legacy_house`, `legacy_house_with_vms`, `demo_home`, and the plain data types they return |

There is no `protocol.rs`: there is nothing to encode.

## Things to keep in mind

- **One world, keyed by local id.** A bound world is shared by every instance of
  the bound plugin, as today's mock was shared by every adapter kind. Two
  instances that declare the same local id share that device's state.
- **Purity.** `validate`, `readiness`, `devices` and `refusal` read nothing but
  their arguments, plus, for a bound plugin, the in-memory flag that
  `MockDevices::set_disabled` sets. No socket, file, clock or secret.
- **The driver checks what the hub checks.** A command for a device that is not
  in the config or the directory, or that its capabilities refuse, is refused
  before it is recorded, so a refused op is never counted as sent.
- **Recording.** Every command that passes those checks is recorded before its
  behaviour is applied, so a `Silent` or `Hang` device still counts one command.
  Every read is recorded, history reads as `rrd:<device>`.
- **Adopted devices** keep their capabilities in the record's `attrs`
  (`{"capabilities": [...]}` in the contract's JSON); without that the class's
  usual set applies. Discovery adds it to each candidate.
- **The legacy house** reproduces what the hub's tests know: the targets,
  classes, capabilities and starting states of today's mock registry, and the
  scene levels. Its names and VMs are generic. The hub's tests that need other
  VMs pass their own list to `legacy_house_with_vms`.

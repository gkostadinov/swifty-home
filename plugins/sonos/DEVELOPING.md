# Developing the Sonos plugin

## Layout

| File | What it holds |
| --- | --- |
| `src/lib.rs` | `MANIFEST`, `PLUGIN`, the pure calls (`validate`, `readiness`, `devices`, `refusal`) and `connect` |
| `src/config.rs` | The typed config and its checks |
| `src/protocol.rs` | The SOAP envelopes and how replies are read. Pure |
| `src/driver.rs` | `SonosDriver`: one `swifty_net::Http` client per speaker |
| `src/setup.rs` | `SonosSetup`: form, test read, done |
| `tests/adapters.rs` | The hub's former adapter tests, ported under the same names, plus new ones |
| `tests/setup.rs` | The setup flow, scripted against `MockSonos` |
| `tests/plugin.rs` | The pure calls |
| `tests/conformance.rs` | The shared conformance suite |

The crate opens no socket itself: every request goes through `swifty-net`, which
needs the hub's live grant. The mock speakers are `swifty_plugin_testkit::devices::MockSonos`.

This crate was ported from the hub's former `adapters/sonos.rs`. Its code was written
for Swifty Home; nothing was copied from another project.

## Protocol sources and their licences (for `NOTICE`)

- **UPnP Device Architecture 1.0** and the **UPnP AV** service templates
  `RenderingControl:1` and `AVTransport:1` (UPnP Forum, now the Open
  Connectivity Foundation). Published specifications; the actions
  `GetVolume`/`SetVolume`, `GetMute`/`SetMute`, `Play`, `Pause`,
  `GetTransportInfo`, `GetMediaInfo` and the SOAP envelope come from them.
- **Sonos extensions** that the UPnP templates do not define: the
  `GetEQ`/`SetEQ` actions with EQ type `NightMode`, the control paths under
  `/MediaRenderer/`, and the URI prefixes `x-sonos-htastream:` (TV input) and
  `x-rincon-stream:` (line-in). Sonos does not document its local UPnP API for
  third parties. These facts are widely documented by open-source projects,
  most fully by **SoCo** (Python, MIT licence, <https://github.com/SoCo/SoCo>).
  The adapter's history does not record which sources its author consulted;
  **the owner should confirm** before `NOTICE` is written. No code or tables
  were copied from SoCo or any other project.
- That a speaker answers HTTP 400 to a request addressed by host name is as the
  hub's adapter recorded it.

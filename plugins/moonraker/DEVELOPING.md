# Developing the Moonraker plugin

## Layout

| File | What it holds |
| --- | --- |
| `src/lib.rs` | `PLUGIN`, the manifest, and the pure calls: `validate`, `readiness`, `devices`, `refusal`, then `connect` and `setup` |
| `src/config.rs` | The typed config and its check |
| `src/protocol.rs` | Pure: the endpoints, how a status reply becomes a printer state, and when a job may start |
| `src/driver.rs` | `Driver`: one request per command, state read back from the printer's objects |
| `src/setup.rs` | The guided setup |
| `tests/driver.rs` | The driver against the test kit's `MockMoonraker`, under the names the tests had in the hub's `crates/swifty/tests/adapters.rs` |
| `tests/setup.rs`, `tests/conformance.rs` | The setup, scripted; the conformance suite; refusals in dry-run against live |

## Rules this plugin keeps

- **Nothing is sent that the printer was not ready for.** Start reads the printer
  first and sends nothing unless it is idle with a job loaded.
- **Never retried.** Every command is one request. The engine decides about retries.
- **The API key is a header.** It is marked sensitive and never goes into a URL.
- **Device text is cleaned.** Moonraker's error messages are shown through
  `swifty_net::device_text`: no control characters, bounded length.

## Protocol sources

Everything this plugin says to Moonraker comes from Moonraker's own documentation
of its web API, and the printer objects it reads from Klipper's documentation of
its status reference. The calls used:

| Call | For |
| --- | --- |
| `GET /printer/objects/query?webhooks&print_stats&extruder&heater_bed&virtual_sdcard` | the state, temperatures, layers and progress |
| `POST /printer/print/start?filename=<loaded file>` | start the loaded job |
| `POST /printer/print/pause`, `/printer/print/resume` | pause and resume |
| `POST /printer/emergency_stop` | emergency stop |
| `X-Api-Key` header | Moonraker's API key, when it asks for one |
| HTTP 503 | Moonraker has lost contact with Klipper |

Moonraker and Klipper are free software under the GNU GPL, version 3. This plugin
speaks Moonraker's documented HTTP API and contains no code from either. No code,
schema or table was copied from them or from any other project. The names of the
console states (`idle`, `stopped` and so on) are this plugin's own.

For `NOTICE`: "The Moonraker plugin uses the documented Moonraker web API to reach
Klipper printers."

## Testing

`cargo test -p swifty-plugin-moonraker` runs everything against loopback stand-ins.
No test reaches a real printer.

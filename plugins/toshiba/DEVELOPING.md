# Developing the Toshiba plugin

## Layout

| File | What |
| --- | --- |
| `src/lib.rs` | `MANIFEST`, `PLUGIN`, the pure calls (`validate`, `readiness`, `devices`, `refusal`) and `connect` |
| `src/config.rs` | The typed config and its checks |
| `src/identity.rs` | How the plugin presents itself to the cloud: the user agent and the client suffix. Defaults only; both are config fields |
| `src/protocol.rs` | Pure: the compact state, the command message, the hub token, the stored sign-in, the unit list |
| `src/driver.rs` | `Cloud` (sign-in, REST calls, hub token, commands), the per-unit reading cache, `ToshibaDriver` |
| `src/setup.rs` | The setup flow |
| `tests/adapters.rs` | The hub's Toshiba adapter tests, under the same names, against the test kit's `MockToshiba` |
| `tests/setup.rs` | The setup flow against a mock built here: the test kit's `MockToshiba` has no list of units |
| `tests/plugin.rs`, `tests/conformance.rs` | The pure calls, and the conformance suite |

Run `cargo test -p swifty-plugin-toshiba`.

## Where the protocol knowledge came from

The protocol is not documented by Toshiba. What this plugin knows about it came from these
sources. This crate is a copy of the hub's Toshiba adapter, which was written from the facts
these sources describe; it is not a port of their code. No code was copied into this crate from
any of them, and no table in its original form: the mode and fan codes and the field offsets are
protocol facts, written out here from what the sources describe. The project is credited, with
its licence, in the repository's `NOTICE`.

| Source | What it gave | Licence |
| --- | --- | --- |
| `toshiba-ac` (Python, by KaSroka; on GitHub as Toshiba-AC-control) | The REST paths for sign-in, unit list and state; the registration that gives an Azure IoT Hub shared-access token; the compact state layout (field offsets, mode and fan codes, the 8 °C hold in merit A, temperature encoding); the command message `CMD_FCU_TO_AC` and its fields; the client id shape `<e-mail>_<hex>` | Apache-2.0 |
| Microsoft's Azure IoT Hub documentation | Sending a device-to-cloud message over HTTPS (`POST /devices/{id}/messages/events?api-version=…`, `iothub-app-*` application properties) instead of the library's AMQP connection | Public documentation; nothing copied |
| This project's own observation | The `Device-ID` header on every REST request (the cloud answers 429 without it, since about July 2026); the firewall that turns away clients that do not look like a browser; `Retry-After: 60 seconds` | — |

## Things to know

- **The word for a sign-in secret.** The hub's source scan (`there_are_no_passwords` in
  `crates/swifty/tests/web.rs`) refuses that word anywhere under `src/`, except as a field label
  of this plugin and Petkit's: a constant whose whole value is the word. Toshiba's sign-in JSON
  uses it as a key and the setup form as a label, so it is written there only
  (`protocol::SIGN_IN_SECRET_KEY`, `SECRET_LABEL` in `lib.rs`). Comments and messages say
  "sign-in secret".
- **Secrets** are wrapped in `Secret` by its constructor, called on a variable. The repository's
  secret scan reports the constructor given a literal, and lets any other value through.
- **Network.** Everything goes through `swifty-net`. The crate names `http` only for the header
  and status types the client takes and returns, and builds and sends each request in one
  function so that it never has to name the client's request type.
- **The client suffix** is part of the account's identity at the cloud. The default is new for
  this plugin; `swifty migrate` writes the old value into an existing install's config.

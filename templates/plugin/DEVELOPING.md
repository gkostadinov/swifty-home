# Developing the {{name}} plugin

## Where things are

| File | What it holds |
| --- | --- |
| `src/lib.rs` | `MANIFEST`, `pub static PLUGIN`, and `impl Plugin`: validate, readiness, devices, refusal, connect, setup |
| `src/config.rs` | The instance config and its checks. Pure |
| `src/protocol.rs` | What the device speaks: operations to bytes, replies to states. Pure, with unit tests |
| `src/driver.rs` | `impl Driver`: sends the bytes through `swifty_plugin_api::net` |
| `src/setup.rs` | The guided setup: a form, a test read, done |
| `tests/common/mod.rs` | A stand-in device on loopback that speaks the protocol |
| `tests/mock_device.rs` | Each operation against the stand-in: bytes sent, state read, confirmed, timeout, refusal |
| `tests/conformance.rs` | The conformance suite every plugin passes |

## Steps

1. Run the tests as they are: {{test_command}}. They pass.
2. Write down the device's protocol below, with where you learned it.
3. Change `protocol.rs` to speak it. Keep one test per operation: the bytes it sends
   and the state a reply turns into.
4. Change `answer` and `apply` in `tests/common/mod.rs` so the stand-in behaves like
   the real device, including how it refuses.
5. Change `config.rs` and `setup.rs` if a person has to enter something else.
6. Try it against the real device in a hub: see *Running it in a hub* below.
7. Fill in the TODOs in `README.md`.

## The rules

- Open sockets only through `swifty_plugin_api::net`. Never use `std::net`,
  `tokio::net`, an HTTP client of your own, a process or an environment variable.
- `validate`, `readiness`, `devices` and `refusal` are pure: no socket, file, clock or
  secret value. They are all a hub in dry-run ever calls.
- Never retry a command. The hub decides whether an operation may be sent twice.
- A secret is a slot, never part of the config, and never in an error or a log line.
- Text from the device goes through `net::device_text` before it is shown or stored.
- Confirmation belongs to the hub: it reads the state back and decides.

## Running it in a hub

{{#workspace}}
This crate is in `plugins/` of a swifty-home checkout, so the workspace builds it.
Add it to the list of compiled-in plugins with `cargo xtask add-plugin plugins/{{id}}`,
then run `cargo run -p swifty -- serve --config dev.toml`.
{{/workspace}}
{{#standalone}}
In a checkout of swifty-home: `cargo xtask add-plugin ../{{id}}`, then
`cargo run -p swifty -- serve --config dev.toml`. To ship it, open a pull request that
adds this crate as `plugins/{{id}}`.
{{/standalone}}

The hub reaches devices only with `live = true` in its configuration.

## Protocol notes

TODO: requests, replies, ports, timing, quirks.

## Sources and their licences

TODO: every document or project you learned the protocol from, with its licence.
They go into the project's NOTICE file.

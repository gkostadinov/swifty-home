# Developing the Glimdock plugin

## Where the protocol comes from

The HTTP contract in `README.md` is Glimdock's own. Glimdock nodes and Swifty Home
come from the same author, and this plugin is the hub's former built-in "collector"
adapter, moved into a plugin crate. No other project's code, tables or documentation
were used, so there is nothing to add to `NOTICE` for it.

| Source | What it gave | Licence |
| --- | --- | --- |
| The Glimdock node HTTP contract, as the hub has consumed it since the collector adapter | the snapshot endpoint, bearer authentication, the nested field names, the pairing endpoint | Apache-2.0 (this repository) |

The field names the hub reads from a snapshot (the flat readings, `sources`,
`faults`, `disks`, `storage`, `bluetooth`, and the optional threads, guests and energy
fields) are documented in `README.md`. When the hub starts reading a new field, document
it there in the same change. The threads, guests and energy fields pass through the
plugin untouched; `protocol.rs` only adds the two flat energy readings, and its tests use
a synthetic snapshot that has them all.

## Layout

| File | What |
| --- | --- |
| `src/lib.rs` | `MANIFEST`, `PLUGIN` and the pure calls: `validate`, `readiness`, `devices`, `refusal`, `connect` |
| `src/config.rs` | `GlimdockConfig` (serde, `deny_unknown_fields`) and its checks: the URL is a scheme, host and port; the paths are plain absolute paths |
| `src/protocol.rs` | Pure functions: flattening a nested snapshot (including `energy_kwh` and `energy_watts`), making every text safe, the pairing request body, the size limit |
| `src/driver.rs` | `GlimdockDriver`: `Driver` (read the snapshot, refuse every command) and `AgentPairing` |
| `src/setup.rs` | `GlimdockSetup`: Form, then one test read, then Done |
| `tests/adapters.rs` | The driver against a mock node: the collector's tests from the hub's former `tests/adapters.rs` and `tests/machines.rs`, under the same names |
| `tests/plugin.rs` | The pure calls, with every address pointing at a trap |
| `tests/setup.rs` | The setup flow, scripted against the mock, live and dry-run |
| `tests/conformance.rs` | The shared conformance suite |

## Rules this crate keeps

- No socket outside `swifty-net`. The crate names no networking, process or
  environment API of its own; the static checks of the conformance tooling look for
  them. The request builder comes from `swifty_net::Http`; the header types come
  from the `http` crate, which is the one the HTTP client uses.
- The token is a `Secret` from `LiveContext` and goes into one header marked
  sensitive. It is never in a URL, an error or a log line. `tests/adapters.rs`
  `nothing_it_says_carries_the_token` checks it.
- A path in the config can never change the destination: `Http::url` refuses one
  that would, and `a_scheme_relative_snapshot_path_cannot_send_the_token_elsewhere`
  proves it with the config check bypassed.
- One request per call, no retries. Silence is an error (`Timeout` or
  `Unreachable`), never a reading.
- The four pure calls open nothing and read no secret value.

## Testing

```sh
cargo test -p swifty-plugin-glimdock
```

The mock node is `swifty_plugin_testkit::devices::collector_handler` behind a
`MockHttp`. It answers `GET /api/snapshot` with the bearer token
`COLLECTOR_TOKEN` and `401` to anything else. The pairing tests wrap it with a
handler for `POST /api/agents/pair`.

# Developing the WiZ plugin

## Layout

| File | What |
| --- | --- |
| `src/lib.rs` | `PLUGIN`, `MANIFEST`, and the pure calls: `validate`, `readiness`, `devices`, `refusal` |
| `src/config.rs` | The typed config and its checks |
| `src/protocol.rs` | The wire format: messages, the colour conversion, reading a reply, which replies to a broadcast count |
| `src/driver.rs` | `WizDriver` (`command`, `read_state`, `discover`) and the discovery function |
| `src/setup.rs` | The setup flow |
| `tests/adapters.rs` | The driver against `MockBulb` and `MockBroadcast`. The tests with the hub's names were copied from the hub's former `crates/swifty/tests/adapters.rs` |
| `tests/setup.rs` | The setup flow driven by scripted inputs |
| `tests/conformance.rs` | The shared conformance suite |

Every socket is opened by `swifty-net` (`udp_exchange`, `udp_discover`), which needs the
`LiveGrant` only a live hub has. This crate must not name the socket modules of the
standard library or of tokio, an HTTP or websocket client, process execution or
environment reads (the static checks of `cargo xtask check-plugin` list them). Address
types (`IpAddr`, `SocketAddr`) are `swifty-net`'s re-exports, which open nothing.

## Where the protocol knowledge comes from

- **The local UDP protocol** (port 38899, JSON with `method` and `params`, the methods
  `getPilot` and `setPilot`, the fields `state`, `dimming`, `r`, `g`, `b`, `temp`,
  `sceneId` and `mac`, and the lowest dimming of 10) is not published by WiZ as a
  developer document. It is known from observing bulbs and from open-source projects
  that speak it, of which pywizlight (MIT licence) is the best known.
- This code was written for Swifty Home (first in the hub's in-tree adapter, then moved
  here). The project audit did not trace it to any one project, and no code, table or
  test data was copied from one. The colour conversion is the textbook HSV-to-RGB at
  full saturation and value.
- If the maintainers confirm that the protocol details came from pywizlight or another
  project, credit it in `NOTICE` by name and licence.

## Running the tests

```text
cargo test -p swifty-plugin-wiz
```

The mocks bind loopback only. `MockBroadcast` binds `127.0.0.255` and starts its bulbs at
`127.0.0.2` and up, all on one port, because real bulbs all listen on one port.

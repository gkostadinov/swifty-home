# Developing the Hue plugin

## Layout

| File | What |
| --- | --- |
| `src/lib.rs` | `PLUGIN`, `MANIFEST`, and the pure calls: `validate`, `readiness`, `devices`, `refusal` |
| `src/config.rs` | The typed config and its URL check; the slot name `app_key` |
| `src/protocol.rs` | The wire format: brightness and hue scales, request bodies, reading replies and the bridge's error numbers, the pairing answer |
| `src/driver.rs` | `HueDriver` (`command`, `read_state`, `discover`, `health`), the pure refusal, and `pair` |
| `src/setup.rs` | The setup flow: form, wait for the link button, choose lights |
| `tests/adapters.rs` | The driver against the testkit's mock bridge. The tests with the hub's names were copied from the hub's former `crates/swifty/tests/adapters.rs` |
| `tests/setup.rs` | The setup flow driven by scripted inputs, against a mock bridge with a link button |
| `tests/conformance.rs` | The shared conformance suite |

Every request goes through `swifty_net::Http`, which needs the `LiveGrant` only a live
hub has, applies the timeouts, follows no redirect and cannot be pointed at another host
by a path. This crate must not name the socket modules of the standard library or of
tokio, an HTTP or websocket client, process execution or environment reads (the static
checks of `cargo xtask check-plugin` list them). The `url` crate is used only to check
the configured address.

The application key is a `Secret`. It is put into a URL path in one place
(`HueDriver::path`) and checked to hold only letters, digits, `-` and `_` first, so it
can never change the path. Keep every error text fixed: the bridge's `address` and
`description` fields can repeat the path, and with it the key.

## Where the protocol knowledge comes from

- **The Hue Bridge REST API v1** (`POST /api` with a `devicetype` to pair, error 101
  for "link button not pressed", `/api/<key>/lights`, `/lights/<n>/state`,
  `/groups/0`, the `bri` scale of 1 to 254, `hue` of 0 to 65535, `sat`, `ct` and
  `colormode`) is documented by Signify on its Hue developer site, which asks for a
  free account. The behaviour of this code follows that documentation
  and observed bridges.
- This code was written for Swifty Home (first in the hub's in-tree adapter, then moved
  here). No code, table or test data was copied from another project. The conversions
  between percent and `bri`, and between degrees and `hue`, are this project's own
  rounding, chosen so every value reads back as itself.

## Running the tests

```text
cargo test -p swifty-plugin-hue
```

The mock bridge (`swifty_plugin_testkit::devices::hue`) binds loopback only and accepts
the key `HUE_KEY`. Its error replies repeat the key on purpose, so the tests can prove
that no error passes it on.

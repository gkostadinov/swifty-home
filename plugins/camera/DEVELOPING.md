# Developing the IP camera plugin

## Where the protocol comes from

The probe is plain RTSP 1.0 as the IETF specifies it. No vendor documentation,
library or other project was used.

| Source | What it gave | Licence |
| --- | --- | --- |
| RFC 2326, Real Time Streaming Protocol (RTSP), 1998 | the `OPTIONS` method, the request line, the `CSeq` header, the `RTSP/1.0` status line | IETF RFC; no code or text copied; nothing for `NOTICE` |

`OPTIONS` needs no session, every RTSP server implements it, and a camera answers
it either without authentication or with a `401` that still proves it is up. That is why the probe needs no login.

## Layout

| File | What |
| --- | --- |
| `src/lib.rs` | `MANIFEST`, `PLUGIN` and the pure calls |
| `src/config.rs` | `CameraConfig` (host and port) and the host check |
| `src/protocol.rs` | Pure functions: the `OPTIONS` request, whether a reply is RTSP |
| `src/driver.rs` | `CameraDriver`: one probe per reading through `swifty_net::tcp_exchange`, every command refused |
| `src/setup.rs` | `CameraSetup`: Form, then one probe, then Done |
| `tests/adapters.rs` | The driver against a mock camera: the camera's tests from the hub's former `tests/adapters.rs`, under the same names |
| `tests/plugin.rs` | The pure calls, with every address pointing at a trap |
| `tests/setup.rs` | The setup flow, live and dry-run |
| `tests/conformance.rs` | The shared conformance suite |

## Rules this crate keeps

- No socket outside `swifty-net`; the crate names no networking, process or
  environment API of its own.
- The host check refuses anything but letters, digits, `.`, `-` and `:`, so a
  config cannot put a path, a user or a second header line into the request.
- The login is required for the camera to be ready, and is never sent.
- One probe per reading, no retries; silence is an error, never a reading.

## Testing

```sh
cargo test -p swifty-plugin-camera
```

The mock camera is `swifty_plugin_testkit::devices::MockRtsp`. It records what it
received, answers `RTSP/1.0 200 OK` unless a test sets another reply, and can hang.

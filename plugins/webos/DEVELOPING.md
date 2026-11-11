# Developing the webOS plugin

## Layout

| File | What it holds |
| --- | --- |
| `src/lib.rs` | `MANIFEST`, `PLUGIN`, the pure calls (`validate`, `readiness`, `devices`, `refusal`) and `connect` |
| `src/config.rs` | The typed config and its checks (`url`, the `wss` pin rule, `wol_mac`, `wol_broadcast`) |
| `src/protocol.rs` | SSAP messages, the input and button tables, the permissions, the magic packet. Pure |
| `src/driver.rs` | `WebosDriver`: the body of the hub's former adapter, presenting the key from the `client_key` slot |
| `src/setup.rs` | `WebosSetup`: form, wait for the prompt, test read, done |
| `tests/adapters.rs` | The hub's former adapter tests, ported under the same names, plus new ones |
| `tests/webos_tls.rs` | The hub's TLS pin tests, ported, plus the driver on the secure port |
| `tests/setup.rs` | The pairing, scripted against mock TVs |
| `tests/plugin.rs` | The pure calls |
| `tests/conformance.rs` | The shared conformance suite |

The crate opens no socket itself: websockets, the name lookup and the
Wake-on-LAN datagram all go through `swifty-net`, which needs the hub's live
grant. Address types (`IpAddr`, `SocketAddr`) are `swifty-net`'s re-exports, which open nothing.

The pairing tests use `PairingTv` in `tests/common`, a TV on the test kit's
`MockWs` that can accept, ignore or decline the prompt. The conformance fixture
uses the test kit's `MockTv` on TLS with `accepts_pairing` set, so the suite's
scripted setup reaches `Done`, and with `powered_off_as` set to `Active Standby`,
as a TV with quick start, so a power off can be confirmed.

This crate was ported from the hub's `adapters/webos.rs` and replaces the old pairing
script, whose handshake it implements in Rust (the script was read
for its behaviour, not translated). Its code was written for Swifty Home;
nothing was copied from another project.

## Protocol sources and their licences (for `NOTICE`)

- **SSAP** (the webOS "second screen" protocol): LG does not publish it for third
  parties. What this crate relies on (the `register` message and its `manifest`
  of permissions, `pairingType: PROMPT`, `forcePairing`, the `registered` reply
  carrying `client-key`, the `ssap://` URIs for power, the launcher, the
  foreground app and the pointer input socket, and the `type:button` frames on
  that socket) is documented by open-source projects, among them **lgtv2**
  (JavaScript, MIT licence, <https://github.com/hobbyquaker/lgtv2>) and
  **PyWebOSTV** (Python, MIT licence, <https://github.com/supersaiyanmode/PyWebOSTV>).
  The adapter's history does not record which sources its author consulted;
  **the owner should confirm** them, and their licences, before `NOTICE` is
  written. No code or tables were copied from any of them.
- **Wake-on-LAN**: the Magic Packet format from AMD's white paper "Magic Packet
  Technology" (1995): six bytes of `0xFF`, then the MAC address sixteen times.
- The TV's ports (3000 plain, 3001 TLS with a self-signed certificate) and its
  answer to a key it does not know (a pairing prompt) are as the hub's adapter
  and the pairing script recorded them.

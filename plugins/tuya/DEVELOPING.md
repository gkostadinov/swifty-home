# Developing the `tuya` plugin

## Layout

| File | What |
| --- | --- |
| `src/lib.rs` | `PLUGIN`, `MANIFEST` and the pure calls: `validate`, `readiness`, `devices`, `refusal`, plus `connect` and `setup`. |
| `src/config.rs` | The typed config (`plugs` rows) and its checks: today's `[[devices.tuya.plugs]]` rules, with the key file replaced by the slot `local_key/<id>`. |
| `src/protocol.rs` | The local protocol 3.3, pure: frames, CRC, AES-128-ECB, the JSON sent, how a stored key becomes a cipher. |
| `src/driver.rs` | The live driver: one connection at a time per socket, one shared reading for a socket and its USB port. |
| `src/cloud.rs` | Tuya's cloud OpenAPI, setup only: request signing, the token, the paged device list. |
| `src/setup.rs` | The setup flow: cloud lookup or "I have the keys", then the sockets, then one read of each. |
| `tests/driver.rs` | The Tuya tests of the hub's former `tests/adapters.rs`, under the same names, against `MockTuya`. |
| `tests/setup.rs` | Scripted setup runs against `MockTuya` and a mock cloud (`tests/common/mod.rs`). |
| `tests/plugin.rs` | The pure calls, with trap listeners for purity. |
| `tests/conformance.rs` | The shared conformance suite, and the fixture's setup script run to `Done`. |

Run `cargo test -p swifty-plugin-tuya`. Everything listens on loopback.

The mock cloud in `tests/common/mod.rs` checks each request's signature with its own
copy of the recipe below, so a mistake in `cloud.rs` cannot agree with itself. The mock
socket (`swifty_plugin_testkit::devices::MockTuya`) has its own copy of the crypto for
the same reason.

## Where the protocol knowledge comes from

Neither protocol is documented by Tuya for the local network. Said plainly:

- **The local protocol 3.3** (frame layout `0x000055AA`, sequence, command, length,
  payload, CRC-32, `0x0000AA55`; AES-128-ECB with PKCS#7 padding under the device's local
  key; the "3.3" header and twelve zero bytes in front of a command; command 7 to set
  data points and 10 to query them; the JSON fields `devId`, `gwId`, `uid`, `t`, `dps`)
  was worked out by the community and is described by open-source projects, among them
  **tinytuya** (Python, MIT licence) and **tuyapi** (JavaScript, MIT licence). This crate's
  protocol code is the hub's earlier Tuya adapter, moved; it is Rust with its own
  structure, names and comments, and nothing in it is copied from either project. Whoever
  writes `NOTICE` should still confirm with the owner how that adapter was first written.
  The AES check uses the FIPS 197 test vector, and the HMAC check uses RFC 4231.
- **The cloud lookup** follows Tuya's own public developer documentation for cloud
  projects: the request signature (HMAC-SHA256 over the client id, the access token for
  business requests, the timestamp, an empty nonce and the string to sign
  `METHOD\nSHA256(body)\n\nURL`, upper-case hex), the token request
  `GET /v1.0/token?grant_type=1`, and the device list of the project's linked app accounts
  `GET /v1.0/iot-01/associated-users/devices?last_row_key=…&size=…` with `devices`,
  `has_more` and `last_row_key` in its result. The deploy step this plugin replaces
  called the cloud through tinytuya (MIT); its flow (sign in, list devices, take each
  device's `id` and `local_key`) was the model, and none of its code was used.

For `NOTICE`: credit tinytuya and tuyapi (both MIT) as sources of protocol knowledge,
with nothing copied. Tuya is a trademark of its owner (already in `NOTICE`).

## Things to know

- **Keys are stored as base64** of the 16 key bytes, the same text today's key files
  hold, so a migration can copy a key file into `local_key/<id>` unchanged. The setup
  accepts both the 16 characters Tuya shows and the base64 form (they cannot be confused:
  base64 of 16 bytes is 24 characters).
- **Purity.** `validate`, `readiness`, `devices` and `refusal` read only the config and
  the names of the filled slots. An empty slot is refused there; a malformed value can
  only be found by the driver, which disables that one socket with fixed text.
- **The cloud is setup only.** `TuyaSetup::with_cloud(base)` replaces every data center
  with a test cloud; the hub uses `TuyaSetup::new()`, which talks to the hosts declared
  in the manifest over HTTPS.
- **Not verified against the real cloud here.** The cloud path is tested against the mock
  only. Things to confirm on a real project: that `size=50` is accepted, that the device
  list's `local_key` is the 16-character key, and how Tuya answers a project with no
  linked account (the plugin shows Tuya's numeric code and fixed text).
- **No discovery.** Tuya sockets announce themselves on UDP 6666 and 6667; reading those
  announcements is not implemented, so addresses are entered.

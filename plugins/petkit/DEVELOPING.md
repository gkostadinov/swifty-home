# Developing the Petkit plugin

## Layout

| File | What |
| --- | --- |
| `src/lib.rs` | `MANIFEST`, `PLUGIN`, the pure calls (`validate`, `readiness`, `devices`, `refusal`) and `connect` |
| `src/config.rs` | The typed config and its checks |
| `src/identity.rs` | How the plugin presents itself to Petkit: app version, user agent, phone. Defaults only; each is a config field |
| `src/protocol.rs` | Pure: dates the way Petkit writes them, the headers, the login line and its MD5, the gateway check, replies and what they say |
| `src/driver.rs` | `PetkitDriver`: region server, login, session, device list, readings, the two commands |
| `src/setup.rs` | The setup flow |
| `tests/adapters.rs` | The hub's Petkit adapter tests, under the same names, against the test kit's `MockPetkit` |
| `tests/setup.rs` | The setup flow against `MockPetkit` |
| `tests/plugin.rs`, `tests/conformance.rs` | The pure calls, and the conformance suite |

Run `cargo test -p swifty-plugin-petkit`.

## Where the protocol knowledge came from

Petkit does not document its cloud. What this plugin knows about it came from these sources.
This crate is a copy of the hub's Petkit adapter, which was written from the facts these sources
describe; it is not a port of their code. No code was copied into this crate from any of them. Both projects are credited, with their licences, in the repository's `NOTICE`.

| Source | What it gave | Licence |
| --- | --- | --- |
| `pypetkitapi` (Python, by Jezza34000; on GitHub as py-petkit-api) | The passport and region-server list; the login form (`oldVersion`, `client`, `encrypt`, `region`, `username` and the MD5 of the secret); the headers the Android app sends and their values (app version, `okhttp` user agent, `X-Client`, time zone headers, `F-Session`/`X-Session`); the family list and the device roster for shared devices; `d4/device_detail`, `d4/dailyFeeds`, `d4/saveDailyFeed`, `t4/device_detail`, `t4/getDeviceRecord`, `t4/controlDevice` and their fields; error 5 for a lost session | MIT |
| The Home Assistant Petkit integration (`homeassistant_petkit`, by Jezza34000) | How the readings are used: feeds counted from the day's records, the litter box's work state and event type 5 for a finished cleaning | MIT |
| RFC 1321 | MD5, through the `md5` crate (Apache-2.0 or MIT) | — |

## Things to know

- **The word for a sign-in secret.** The hub's source scan (`there_are_no_passwords` in
  `crates/swifty/tests/web.rs`) refuses that word anywhere under `src/`, except as a field label
  of this plugin and Toshiba's: a constant whose whole value is the word. Petkit's login form uses
  it as a field name and the setup form as a label, so it is written there only
  (`protocol::LOGIN_SECRET_FIELD`, `SECRET_LABEL` in `lib.rs`). Comments and messages say
  "sign-in secret".
- **Secrets** are wrapped in `Secret` by its constructor, called on a variable. The repository's
  secret scan reports the constructor given a literal, and lets any other value through.
- **Network.** Everything goes through `swifty-net`. The crate names `http` only for the header
  types the client takes.
- **The test kit's mock** names its region server under `FR` only (and `DE` points elsewhere), so
  the tests use the region `FR`.
- **Conformance and text defaults.** The suite tries an undeclared key with its manifest default
  read as JSON, so the default `16.1` of `os_version` would arrive as a number. The fixture
  writes `os_version` out.

# Developing the Plex plugin

## Where the protocol comes from

Plex Media Server has an HTTP API that Plex does not document for third parties
as a whole. This plugin uses one fact about it: a request carrying a valid token in
the `X-Plex-Token` header is answered, and one with a wrong token gets `401`.
Plex's own support article on finding an authentication token describes the token
and the `X-Plex-Token` parameter. Nothing else was used.

| Source | What it gave | Licence |
| --- | --- | --- |
| Plex support documentation on the `X-Plex-Token` | the header name, that a token authenticates a request to the server | no code or text copied; nothing for `NOTICE` |

The plugin sends the token as a header, never as the query parameter, so it never
appears in a URL or a log of one.

## Layout

| File | What |
| --- | --- |
| `src/lib.rs` | `MANIFEST`, `PLUGIN` and the pure calls |
| `src/config.rs` | `PlexConfig` and the URL check |
| `src/driver.rs` | `PlexDriver`: one `GET /` per reading, every command refused |
| `src/setup.rs` | `PlexSetup`: Form, then one test read, then Done |
| `tests/adapters.rs` | The driver against a mock server: the Plex tests from the hub's former `tests/adapters.rs`, under the same names |
| `tests/plugin.rs` | The pure calls, with every address pointing at a trap |
| `tests/setup.rs` | The setup flow, live and dry-run |
| `tests/conformance.rs` | The shared conformance suite |

## Rules this crate keeps

- No socket outside `swifty-net`; the crate names no networking, process or
  environment API of its own. The `http` crate gives
  the header type, so the token can be marked sensitive.
- The token comes from `LiveContext`, goes into one header and nowhere else.
- One request per reading, no retries; silence is an error, never a reading.

## Testing

```sh
cargo test -p swifty-plugin-plex
```

The mock server is `swifty_plugin_testkit::devices::plex_handler` behind a
`MockHttp`: `200` for the token `PLEX_TOKEN` in `X-Plex-Token`, `401` otherwise.

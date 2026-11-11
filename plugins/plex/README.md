# Plex plugin

`swifty-plugin-plex` tells Swifty Home whether a **Plex Media Server** is up and
accepts its token. That is all it does: nothing is played, paused, listed or changed.

## The device

| | |
| --- | --- |
| Local id | `plex` (also hinted as its target, so an existing install keeps the target `plex`) |
| Class | `service` (owner-only by default) |
| Capabilities | none: reading is all it offers |
| Default name | Plex server |
| State | `{ "kind": "service", "reachable": true }` |

Every operation is refused with "Plex is read-only here."

## Setup

1. **Form**: the server URL and its token.
2. **Test read**: when the hub is live, one request to the server with that token.
   If it fails, the flow says why and offers the form again. In dry-run nothing is
   contacted and the instance is saved untested.
3. **Done**: the config is stored, the token goes into the secret slot `token`.

Setting the server up again pre-fills the URL. Leave the token empty to keep the
stored one; the flow then does not test, because it never sees a stored secret.

## Config

| Key | Required | What |
| --- | --- | --- |
| `url` | yes | The server's base URL: scheme (`http` or `https`), host and port, nothing else. Example: `http://192.0.2.20:32400` |

```json
{ "url": "http://192.0.2.20:32400" }
```

## Secret slots

| Slot | Required | What |
| --- | --- | --- |
| `token` | yes | The server's `X-Plex-Token`. Plex's own help pages explain how to find it. Sent only in the `X-Plex-Token` header, never in a URL. Until it is stored the instance reads "needs setup" |

## What goes over the wire

One request per reading: `GET /` on the server, with the headers `X-Plex-Token`
and `Accept: application/json`. No retries, no redirects, the hub's connect and
read timeouts.

| Answer | Meaning |
| --- | --- |
| `2xx` | Up: `reachable: true` |
| `401` | Refusal: "Plex did not accept the token" |
| anything else | Refusal: "Plex refused with HTTP <status>" |
| no answer, connection refused | Silence (`Timeout` or `Unreachable`), never a reading |

An answer larger than 1 MiB is refused.

## Limits

- Read-only, and only reachability. It does not show what is playing.
- A server behind a private CA is not supported; use `http` on the local network
  or a publicly trusted certificate.

See `DEVELOPING.md` for where the protocol knowledge comes from.

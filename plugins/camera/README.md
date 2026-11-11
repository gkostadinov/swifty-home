# IP camera plugin

`swifty-plugin-camera` tells Swifty Home whether an **IP camera** is up and speaking
RTSP. It never moves, configures or records the camera, and the check sends no
credentials.

## The device

| | |
| --- | --- |
| Local id | `camera` (also hinted as its target, so an existing install keeps the target `camera`) |
| Class | `camera` (the family may see it by default) |
| Capabilities | none: reading is all it offers |
| Default name | IP camera |
| State | `{ "kind": "camera", "streaming": true }` |

Every operation is refused with "The camera is view-only here."

## Setup

1. **Form**: the camera's address, its RTSP port (554 unless you changed it) and its login.
2. **Test probe**: when the hub is live, one RTSP probe. If the camera does not
   answer in RTSP, the flow says so and offers the form again. In dry-run nothing
   is contacted and the instance is saved untested.
3. **Done**: the config is stored, the login goes into the secret slot `login`.

The probe needs no login, so setting the camera up again tests it even when you
keep the stored login.

## Config

| Key | Required | What |
| --- | --- | --- |
| `host` | yes | The camera's IP address or host name: letters, digits, `.`, `-` and `:` only. Example: `192.0.2.30` |
| `port` | no | The RTSP port. Default `554` |

```json
{ "host": "192.0.2.30", "port": 554 }
```

## Secret slots

| Slot | Required | What |
| --- | --- | --- |
| `login` | yes | The camera's login, as `user:secret`. The camera stays disabled ("needs setup") until it is stored. The status probe never sends it |

## What goes over the wire

One TCP connection per reading to `host:port`, carrying one request:

```text
OPTIONS rtsp://<host>:<port>/ RTSP/1.0
CSeq: 1
User-Agent: swifty-home
```

The plugin reads up to the end of the reply's header block (at most 8 KiB).

| Answer | Meaning |
| --- | --- |
| starts with `RTSP/1.` (any status, even `401 Unauthorized`) | Up: `streaming: true` |
| anything else | Refusal: "the camera did not answer in RTSP" |
| no answer, connection refused | Silence (`Timeout` or `Unreachable`), never a reading |

## Limits

- Read-only and status only: no stream, no snapshot image, no pan or tilt.
- `streaming: true` means "answers in RTSP", not that a stream is being watched.
- RTSP over TLS (`rtsps`) is not supported.
- An IPv6 address works for the connection, but goes into the request line without brackets.

See `DEVELOPING.md` for where the protocol knowledge comes from.

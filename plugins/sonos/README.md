# Sonos plugin (`swifty-plugin-sonos`)

Controls Sonos speakers on the local network over UPnP (HTTP on port 1400). No
cloud and no Sonos account are involved.

## What it controls

Each speaker is a device of class `speaker`.

| Action | What is sent | Read back from |
| --- | --- | --- |
| `speaker.volume` | `RenderingControl#SetVolume` | `GetVolume` |
| volume cap (scenes) | `GetVolume`, then `SetVolume` only if the volume is above the cap | `GetVolume` |
| `speaker.mute` | `RenderingControl#SetMute` | `GetMute` |
| `speaker.night` | `RenderingControl#SetEQ`, EQ type `NightMode` | `GetEQ` |
| `speaker.play`, `speaker.pause` | `AVTransport#Play`, `#Pause` | `GetTransportInfo` |

A reading also reports the source (`TV`, `Line-in`, `Music` or `None`, from
`GetMediaInfo`). A speaker that has no night mode, or does not say within 1.5
seconds, is read without it, never as "off".

A speaker marked **read only** (a subwoofer, which has no AVTransport service) is
only ever asked for its volume and mute. It offers no capability, so no catalog
action admits it, and the driver refuses any command for it before anything is
sent.

## Setup

`swifty plugin add sonos`, or the web wizard:

1. **Form**: the speaker's IP address and, optionally, a subwoofer's.
2. **Test read**: when the hub is live, each speaker is read once. A speaker that
   cannot be read fails the setup with its address. In dry-run nothing is
   contacted and the config is saved as entered.
3. **Done**.

The form names two speakers, with the ids `sonos` and `sub`. More speakers are
more rows of the config. Setting the plugin up again keeps those other rows.

Give each speaker a fixed address in the router first: the config holds IP
addresses, because a Sonos speaker answers HTTP 400 to a request addressed by a
host name.

## Config

```json
{ "speakers": [
    { "id": "sonos", "url": "http://192.0.2.30:1400" },
    { "id": "sub", "url": "http://192.0.2.31:1400", "read_only": true }
] }
```

| Field | Required | Meaning |
| --- | --- | --- |
| `speakers` | yes | One row per speaker, at least one and at most 32 |
| `speakers[].id` | yes | The device's local id, `[a-z0-9][a-z0-9_-]{0,63}`, unique. It is also the target the plugin asks the hub for |
| `speakers[].url` | yes | `http://<IP address>:1400`: a scheme, host and port, nothing else |
| `speakers[].read_only` | no, `false` | Only read this speaker, never command it |

## Secret slots

None. The speakers take no credentials.

## Limits

- One speaker per row; groups and zones are not managed. A command goes to the
  speaker it names and nowhere else.
- No discovery: speakers are named by address.
- No track, queue or favourite control. Play and pause only.
- Each call sends exactly one SOAP action, with the hub's connect and read
  timeouts, and is never retried by the plugin.

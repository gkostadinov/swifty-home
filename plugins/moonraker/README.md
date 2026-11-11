# Moonraker plugin (Klipper 3D printers)

`swifty-plugin-moonraker` connects Swifty Home to a Klipper 3D printer through
Moonraker's HTTP API. It stays on the local network.

## What it controls

One device per instance:

| Device | Local id (and the target it asks for) | Class | What it can do |
| --- | --- | --- | --- |
| The printer | `printer` | `printer` | Start the job Klipper has loaded, pause, resume, emergency stop; its state, nozzle and bed temperatures, layer and progress |

- **Start** prints the file Klipper already has loaded, and only when the printer is
  idle. The printer is read first; when it cannot be read, is busy or has no job
  loaded, nothing is sent. It never uploads or picks a file.
- **Emergency stop** is Klipper's own `emergency_stop`. The printer then reads as
  `stopped` until Klipper is restarted at the printer.
- A Moonraker that has lost contact with Klipper is reported as silence, never as a
  reading.

States read back: `printing`, `paused`, `idle` (standby, complete or cancelled),
`stopped`, `starting`, `error`, `unknown`.

## Setup

The guided setup asks for Moonraker's address (`http://` and the printer's address
with port 7125) and, only when Moonraker asks for one, its API key. It then reads the
printer once and says what state it is in. The read needs a live hub. When the setup
is run again and the stored key is kept, it saves the address without asking the
printer.

## Config

| Key | Required | Meaning |
| --- | --- | --- |
| `url` | yes | Moonraker's base URL: the scheme (`http` or `https`), the address and the port, nothing after it |

```toml
[[instance]]
id = "moonraker"
plugin = "moonraker"
[instance.config]
url = "http://192.0.2.30:7125"
```

## Secret slots

| Slot | Required | Holds |
| --- | --- | --- |
| `api_key` | no | Moonraker's API key, for a Moonraker that asks for one. It is sent as the `X-Api-Key` header, never in a URL |

## Coming from a version 1 config

`[devices.moonraker] url` becomes `url`; the content of `api_key_file`, when there
was one, goes into the `api_key` slot.

## Limits

- One printer per instance. A second printer is a second instance.
- No file management, temperature setting, G-code or macros. None of these can be
  sent from here.
- A Moonraker that trusts its clients by address needs no key; whoever runs the hub
  should know the printer then trusts every client on that network.

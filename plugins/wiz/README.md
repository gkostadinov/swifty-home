# WiZ (`swifty-plugin-wiz`)

Controls WiZ smart bulbs on the local network: power, brightness and colour. Every
command is one JSON datagram over UDP port 38899, and every result is read back from
the bulb with `getPilot` before the hub calls it done. WiZ bulbs take no key and no
account, and the plugin never talks to the WiZ cloud.

## What it controls

| Device | Local id (and the target it asks for) | Class | Capabilities |
| --- | --- | --- | --- |
| A bulb named in the config | its `id`, such as `bulb1` | `light` | power, brightness (10 to 100 %), colour |
| A bulb found by discovery and adopted | `wiz-<mac>`, the MAC in lower-case hex | `light` | power, brightness (10 to 100 %), colour |

A configured bulb gets a generic name ("WiZ light 1"); the owner renames it in the app.
A discovered bulb is called "WiZ light" and the end of its MAC until it is renamed.

## Setup

`swifty plugin add wiz`, or Integrations in the app, runs this flow:

1. **Form.** A broadcast address (prefilled with `255.255.255.255:38899`, which reaches
   every bulb on the hub's own link) and, if you like, bulbs by address
   (`desk=192.0.2.21, 192.0.2.22`). Bulbs without an id get `bulb1`, `bulb2`, and so on.
2. **Discovery.** The hub broadcasts `getPilot` and listens for up to three seconds.
   Only a reply from the broadcast's own network, from port 38899, with a twelve-digit
   MAC counts. Discovery changes nothing on a bulb.
3. **Choose** the bulbs to add.
4. **Done**, after one reading of every chosen bulb. A bulb that does not answer stops
   the setup with its name, and you can try again.

Discovery needs live mode. In dry-run the flow still finishes when you list bulbs by
address and leave the broadcast empty, and nothing is sent.

Later, the hub's discovery (`Driver::discover`) finds new bulbs the same way. A bulb is
only adopted from what a discovery found, so nobody can name an address for the hub
to send to.

## Config fields

| Key | Kind | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `bulbs` | rows of `id`, `address` | no | none | Bulbs by address. `id` is `[a-z0-9][a-z0-9_-]{0,63}` and may not start with `wiz-`. `address` is an IP address, with `:port` if the bulb does not use `port` |
| `port` | port | no | `38899` | The port of a bulb whose address names none |
| `broadcast` | IPv4 address:port | no | none | Where discovery sends its question. Discovery is refused until this is set |

```json
{ "bulbs": [{ "id": "bulb1", "address": "192.0.2.21" }], "broadcast": "192.0.2.255:38899" }
```

## Secret slots

None. WiZ bulbs accept commands from anyone on the network without a key.

## Limits

- A bulb's lowest brightness is 10 %. A lower request is sent as 10 %, and reading 10 %
  back confirms it.
- A bulb in white or scene mode reports a colour temperature or a scene, not a hue. It
  is shown as a tint, never as a colour, so it can never confirm a colour change.
- Anyone on the network can answer a broadcast with any MAC. An adopted bulb stays at
  the address it was adopted with until the owner accepts a new one in the app.
- Every command is sent once. A bulb that does not answer within the read timeout is
  reported as silence, and the hub decides whether to try again.
- The bulbs' cloud features (schedules, rhythms, the WiZ app's scenes) are not used.

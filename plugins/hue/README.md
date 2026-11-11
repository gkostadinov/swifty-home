# Philips Hue (`swifty-plugin-hue`)

Controls the lights on a Philips Hue Bridge through the bridge's local REST API (v1):
power, brightness and colour. The plugin talks to the bridge on your network and never
to the Hue cloud. Every change is read back from the bridge before the hub calls it done.

## What it controls

| Device | Local id (and the target it asks for) | Class | Capabilities |
| --- | --- | --- | --- |
| The bridge | `hue` | `bridge` | none: it only reports that it can be reached and that its key works |
| A light on the bridge, once adopted | `hue-<n>`, `n` being the bridge's number for it | `light` | power, brightness (1 to 100 %; 0 is off), and colour when the light has it |

A second bridge is a second instance. Its devices ask for the same ids, find them
taken, and are named after the instance (`hue2-hue`, `hue2-hue-3`).

## Setup

`swifty plugin add hue`, or Integrations in the app, runs this flow:

1. **Form.** The bridge's address, such as `http://192.0.2.10`.
2. **Wait.** Press the round link button on the bridge. The hub asks the bridge for a key
   every two seconds, for up to 90 seconds. The bridge agrees for 30 seconds after a
   press. The request names this hub `swifty_home#hub`, so the bridge lists it under
   that name.
3. **Choose** the lights to add. The list comes from the bridge.
4. **Done**, after one reading of every chosen light. A light that cannot be read stops
   the setup with its name; the key is kept, so trying again does not pair again.

Pairing needs live mode. In dry-run the flow stops at the form and sends nothing.

Setting an instance up again that already holds a key keeps that key, unless you tick
**Pair again**. A key made under an older name keeps working; the bridge keeps the old
name for it.

Later, the hub's discovery (`Driver::discover`) lists the bridge's lights again, so new
ones can be adopted.

## Config fields

| Key | Kind | Required | Meaning |
| --- | --- | --- | --- |
| `url` | URL, `http` or `https` | yes | The bridge: a scheme, a host and a port, with no path, query, fragment or user |

```json
{ "url": "http://192.0.2.10" }
```

## Secret slots

| Slot | Required | Meaning |
| --- | --- | --- |
| `app_key` | yes | The application key the bridge gives while its link button is pressed. Setup fills it; nobody types it |

Until `app_key` holds a value the instance needs setup, and its devices are listed and
disabled.

## Limits

- The key travels in the URL path, as the v1 API requires, so plain `http` carries it
  across the network in clear. Error messages are fixed text and the bridge's error
  number; nothing the bridge writes is repeated, so the key never reaches a log, an
  error, a result or an audit row.
- The bridge itself takes no commands.
- A colour on a light without colour is refused before anything is sent.
- A light the bridge cannot reach is reported as silence, never as a reading.
- Scenes, groups, sensors, entertainment areas and the v2 API are not used.
- Every command is sent once. The hub decides whether to try again.

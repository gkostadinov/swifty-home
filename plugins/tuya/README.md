# Tuya smart sockets (`tuya`)

Switches smart sockets built on Tuya's platform (many brands sell them under their own
name) on your own network, with Tuya's local protocol version 3.3 on TCP port 6668.
A socket with a USB port that switches on its own gets a second switch for that port.

- **Class:** `plug`. **Capability:** `Power`. **Actions:** `plug.power` (on, off) and `state.read`.
- **Reach:** local. The sockets are never switched through Tuya's cloud.
- **Cloud, setup only:** the setup can look the sockets' local keys up in your Tuya cloud
  project, once. The access secret is used to sign those requests and is not stored.

## Setup

The setup starts with a choice.

**Look the keys up in my Tuya cloud project** (needs a live hub):

1. On the Tuya developer platform, create a cloud project in the data
   center of your Tuya app account, with the IoT Core and Authorization services, and
   link your Tuya or Smart Life app account to it (Devices, Link App Account).
2. In Swifty Home, pick the data center and enter the project's access id and access secret.
3. Tick the sockets to add. Sockets and power strips are ticked already; devices
   without a local key (gateways, sub-devices) are not listed.
4. Enter each socket's address on your network (your router's device list shows it; a
   fixed address in the router keeps it from changing), and, if you like, a name and
   the USB data point.
5. Each socket is read once. If one does not answer, the setup says which and why.

**I have the keys:** enter each socket's device id, address, port and local key, one
form per socket, then tick the ones to keep. This path is a form only, so it also works
in dry-run, where nothing is contacted.

Setting the instance up again prefills what is stored. A stored key is kept unless a new
one is entered, and only new keys are handed to the hub.

## Config fields

The instance config is a list of sockets:

```json
{ "plugs": [
  { "id": "bf0123456789abcdef", "address": "192.0.2.40" },
  { "id": "bf9876543210fedcba", "address": "192.0.2.41", "port": 6668, "name": "Desk lamp", "usb_dp": 7 }
] }
```

| Field | Required | Meaning |
| --- | --- | --- |
| `plugs[].id` | yes | The Tuya device id: 10 to 32 lower-case letters and digits. |
| `plugs[].address` | yes | The socket's IPv4 address. Not a name, not a broadcast or multicast address. |
| `plugs[].port` | no, `6668` | The local protocol's port. |
| `plugs[].name` | no | 1 to 64 plain characters. Without one: "Socket 1", "Socket 2" and so on, in this order. |
| `plugs[].usb_dp` | no | The data point (2 to 255, usually 7) of a USB port that switches on its own. |

At most 32 sockets, each id and each address and port once. Unknown fields are refused.

## Secret slots

| Slot | Holds |
| --- | --- |
| `local_key/<id>` | The socket's local key, base64 of its 16 bytes. The setup accepts the key as Tuya shows it (16 characters) or already in base64, and stores base64. |

## Devices

| Local id (and target hint) | What |
| --- | --- |
| `plug-<id>` | The socket. |
| `plug-<id>-usb` | Its USB port, when `usb_dp` is set. |

These are the targets earlier versions used, so rooms, favourites and the audit log keep working.

## Limits

- A socket takes very few connections at once. Each exchange connects, does one thing
  and closes, one at a time per socket, and the socket and its USB port share one reading.
  The Tuya app on a phone can hold the only connection; close it if a socket stops answering.
- Only protocol 3.3 is spoken. Sockets whose firmware uses 3.4 or 3.5 are not supported yet.
- A socket whose key is missing or malformed is refused on its own; the others still work.
  An empty slot is known to the pure `refusal` already (dry-run says so too); a malformed
  value only shows when a live hub tries the socket.
- Addresses are not discovered. The cloud knows each socket's public address only.
- A command is never retried here. A socket can answer a command with nothing, so a
  command is confirmed by reading the socket afterwards.

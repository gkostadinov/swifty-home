# Toshiba air conditioners (`toshiba`)

Switches Toshiba air conditioners on Wi-Fi on and off, and sets their mode, temperature and
fan, through Toshiba's Home AC Control cloud. It reads each unit's power, mode, setpoint, fan,
and the indoor and outdoor temperatures.

> **Unofficial integration.** Toshiba does not offer its cloud to other software. This plugin
> speaks the protocol of Toshiba's own app, as the open-source `toshiba-ac` library does. It may
> stop working whenever Toshiba changes its cloud, without warning, and Toshiba may not allow
> this use. The manifest says so (`reach = cloud`, `official = false`), and the setup starts
> with this notice.

The Wi-Fi adapter in these units talks only to Toshiba's cloud, so this plugin needs the
internet: `mobileapi.toshibahomeaccontrols.com` for signing in and reading, and Toshiba's Azure
IoT hub `toshibasmaciothubprod.azure-devices.net` for commands.

## Setup

1. **Before you start.** The notice above. Nothing is sent until it is accepted.
2. **Your Toshiba account.** The e-mail address and sign-in secret of the account you use in
   Toshiba's app. The hub signs in once and lists the units on the account.
3. **Air conditioners.** Pick the units to add, up to 8. Each chosen unit is read once; the
   setup fails with the reason if one cannot be read.

In dry-run (`live = false`) the forms work, and the step that signs in says that live mode is
needed. Nothing is contacted.

## Devices

One device per unit, class `aircon`, with the capabilities `power`, `mode` (auto, cool, heat,
dry, fan), `temperature` (17 to 30 °C) and `fan` (auto, quiet, low, medium_low, medium,
medium_high, high). Its local id, and the target the hub gives it, is `ac-` and the unit's id
in lower case. Without a name of its own it is called "Air conditioner", numbered when there
are several.

## Config

| Key | Default | Meaning |
| --- | --- | --- |
| `units` | required | One row per unit, 1 to 8: `id` (the cloud's `Id`, 8 to 64 letters, digits and hyphens), `unique_id` (the cloud's `DeviceUniqueId`), optional `name` |
| `client_suffix` | `73776674686f6d65` | Hex digits joined to the account's e-mail to name this hub at the cloud (the `Device-ID` header, and the id registered for commands). An install set up before this plugin existed keeps the value it registered; do not change it on a working install |
| `user_agent` | a desktop Chrome user agent | What the hub says it is. The cloud turns away clients that do not look like a browser |
| `api_url` | `https://mobileapi.toshibahomeaccontrols.com` | Only a test changes this. https unless it is this machine |
| `hub_url` | `https://toshibasmaciothubprod.azure-devices.net` | The hub that takes commands. A token for any other hub is refused. Only a test changes this |
| `timeout_ms` | `10000` | How long one request to the cloud may take to answer, 1 to 30000; never longer than the hub's own read timeout |

The setup writes `client_suffix` into the config, so a later release with another default
does not change the id this hub has at the cloud.

## Secret slots

| Slot | Holds |
| --- | --- |
| `login` (required) | One line: base64 of the JSON that signs in to the account, exactly as the cloud takes it. The setup writes it. It is only ever sent to Toshiba's sign-in |

## Limits and behaviour

- **Rationed.** A reading is shared for five minutes. After a command the unit is looked at
  again at once, then after 3, 6 and 12 seconds. Polls cost no extra requests in between.
- **Every request names this client.** Every request to the REST API carries a `Device-ID`
  header. Since about July 2026 the cloud answers 429 to a sign-in without one.
- **Turned away means left alone.** A 403 or 429 stops all requests for at least as long as the
  cloud asked (`Retry-After`), and each further one doubles the wait, up to 15 minutes. A
  refused sign-in is not tried again for 15 minutes, so the account is not locked.
- **The last good reading stands for an hour** while the cloud rests (it asked for fewer
  requests), so a unit does not vanish from the page. A cloud that does not answer, or answers
  with something that cannot be read, is said at once, and no reading from before it is shown
  as current until the cloud answers again. A refused sign-in is said at once.
- **No command is sent twice**, except when the cloud answered 401, which means it did not take
  it: then it signs in, or fetches a new hub token, and sends once more.
- **A unit holding 8 °C** (frost protection) refuses a new temperature: turn that off on its
  remote or in Toshiba's app first. Turning on a unit that is cleaning itself calls the
  cleaning off.
- **Nothing private is logged.** The log says which request the cloud turned away, its status,
  how long it asked to be left alone, and Toshiba's one-word code for a refusal. Errors are fixed
  text and at most a status number.
- Toshiba's cloud does not push changes to this hub, so a change made on the remote shows up at
  the next reading.

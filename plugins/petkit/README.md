# Petkit feeder and litter box (`petkit`)

Reads and commands a Petkit D4 feeder and a Petkit T4 litter box through Petkit's cloud: feed
10, 20, 30, 40 or 50 g now, and start a clean cycle. It reads how many portions the feeder has
given today and when the last one was, and whether the litter box is busy, how many cleanings
finished today and when the last one did.

> **Unofficial integration.** Petkit does not offer its cloud to other software. This plugin
> presents itself as Petkit's Android app and speaks its protocol, as the open-source
> `pypetkitapi` client does. It may stop working whenever Petkit changes its servers, without
> warning, and Petkit's terms do not clearly allow this use. The manifest says so
> (`reach = cloud`, `official = false`), and the setup starts with this notice.

**Use a second Petkit account** that the devices are shared to. A Petkit account is signed in
on one device at a time, so using your main account here signs the phone app out.

**Commands start switched off** (`commands = false`). Check that the readings match the real
devices, then turn `commands` on in the instance's settings.

The plugin needs the internet: Petkit's passport (`passport.petkt.com`), which names the
server for the account's region, and that server (`api.petkt.com`, `api.eu-pet.com`,
`api.petktasia.com`, or `api.petkit.cn` for China). A server the passport names elsewhere is
never logged in to.

## Setup

1. **Before you start.** The notice above. Nothing is sent until it is accepted.
2. **The Petkit account.** The region (your country as the Petkit app has it, such as `DE`, or
   `china`), the account's e-mail or phone number, its sign-in secret, the time zone, and the
   minutes east of UTC. The secret is turned into its MD5 at once, which is all Petkit's login
   sends; the secret itself is neither kept nor stored. The hub logs in once and lists the
   feeders and litter boxes on the account and shared to it.
3. **Devices.** Pick the feeder, the litter box, or both (one of each at most). Each chosen
   device is read once; the setup fails with the reason if one cannot be read. Nothing is fed or
   cleaned.

In dry-run (`live = false`) the forms work, and the step that logs in says that live mode is
needed. Nothing is contacted.

## Devices

| Local id and target | Class | Capabilities | Name |
| --- | --- | --- | --- |
| `feeder` | `feeder` | `dispense` (10, 20, 30, 40, 50 g) | Petkit D4 feeder |
| `litter` | `litter` | `clean` | Petkit T4 litter box |

## Config

| Key | Default | Meaning |
| --- | --- | --- |
| `region` | required | Your country as the Petkit app has it, or `china` |
| `timezone` | `UTC` | An IANA name, sent to Petkit as the account's time zone |
| `utc_offset_minutes` | `0` | Minutes east of UTC for that zone, now, -840 to 840. Days and the times shown use it. It does not follow daylight saving time by itself |
| `commands` | `false` | Feeding and cleaning are refused until this is true |
| `feeder_id`, `litter_id` | none | Only needed when the account has more than one. The setup fills them in |
| `poll_secs` | `60` | How long a reading is kept when nothing was just commanded, 15 to 600 |
| `passport_url` | `https://passport.petkt.com/` | Only a test changes this. https unless it is this machine |
| `app_version` | `13.2.1` | The version of Petkit's app the hub says it is |
| `user_agent` | `okhttp/3.14.9` | The app's HTTP client |
| `os_version`, `phone_model`, `phone_brand` | `16.1`, `23127PN0CG`, `Xiaomi` | The phone the hub says the app runs on |

The last five are the client identity. They are what Petkit's servers accepted when this was
written; change them only if Petkit starts refusing the defaults.

## Secret slots

| Slot | Holds |
| --- | --- |
| `login` (required) | One line: the account, a colon, and the MD5 of its sign-in secret in hex. The setup writes it |

## Limits and behaviour

- **Nothing is sent twice that could act twice.** A feed or a clean that gets no answer is
  reported as silence; the hub's engine never repeats it. A session Petkit has signed out is
  replaced before a reading is tried again, but a command that finds it signed out is not resent.
- **A refused login is not tried again** until the instance is loaded again (after the setup is
  run again), so the account is never locked out. Any other failed login waits half a minute.
- **Rationed.** A reading is kept for `poll_secs`, or four seconds while a command is being
  watched (90 seconds after it). A call that gets no answer, or an answer that cannot be read,
  drops what was kept, so an old reading is never shown as current. A device that is not on the
  account is looked for again after ten minutes.
- **The hub's timeouts.** Every request waits as long as the hub's device timeouts allow, no
  longer.
- **The D4 feeds only 10, 20, 30, 40 or 50 g.** Other amounts are refused before anything is sent.
- **Nothing private is said.** Every error is fixed text and at most Petkit's error number; the
  account, the hash, the session and Petkit's own words never reach a log or a result.

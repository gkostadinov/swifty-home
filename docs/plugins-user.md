# Integrations

An **integration** is one use of a **plugin**: your Hue Bridge, your Proxmox host,
your Sonos speakers. The plugins are compiled into Swifty Home; you add as many
integrations as you need, and remove them again, without restarting anything.

## Adding one

In the app: **Integrations → Add**, then pick the plugin. Before anything else, the
app shows what the plugin will reach on your network or on the internet, and marks
cloud plugins that are not official.

In the terminal, the same guided steps:

```sh
sudo swifty plugin list --available
sudo swifty plugin info hue
sudo swifty plugin add hue
```

A setup asks only what it needs. It may ask you to do something physical ("press the
button on the bridge", "accept the prompt on the TV"), offer what it found to tick,
or send you somewhere first ("create an API token, then come back"). Credentials go
straight into the secret store; they are never shown again, logged or written to
the audit log. Adding, setting up again and removing an integration are critical
actions, so the app asks for your passkey.

Many setups talk to the device (to pair, to find lights, to check that it answers).
In dry-run they stop at that point and say live mode is needed; setups that only
need a form, such as the demo home, finish in dry-run too.

A setup that nobody touches for 15 minutes is forgotten, and nothing it held is stored.
At most four can be open at once. In the terminal, a setup that runs out of answers
(standard input ended before it asked everything) is abandoned the same way, and
`--yes` or `--set` give it answers ahead; a step that waits for a button shows how long
is left, and Ctrl+C stops it.

## Looking after them

```sh
sudo swifty plugin list
sudo swifty plugin setup hue
sudo swifty plugin remove hue
```

- `plugin list` shows each integration's readiness (ready, needs setup, or disabled
  and why) and its devices. It does not show health: the command never talks to a device,
  and only the running hub checks it. The **Integrations** page shows readiness and health,
  and `plugin list --json` prints what the API answers.
- `plugin setup` runs the setup again: after a new pairing, a changed address or a
  new token. Leaving a secret field empty keeps the stored one. It takes `--set key=value`
  to answer a question ahead and `--yes` to take what is offered, as `plugin add` does.
- `plugin info <plugin>` says what a plugin reaches, what it asks for and which actions it
  adds; `--json` prints its manifest.
- `plugin remove` first lists everything that goes with it (its devices, their places
  in rooms, favourites, its secrets), then removes it. The audit log keeps every row.

If a setup cannot store a credential the way you have it, store it directly, from
standard input:

```sh
sudo swifty plugin secret set hue app_key < hue-key.txt
```

### Answering in the terminal

Each question of a setup is one line, ended with Enter. A secret is typed without being
shown. A field that takes several lines (a certificate, say) reads lines until an empty
one. A list to choose from is numbered: type the numbers (`2`, `1 3`, `2-4`), `all` or
`none`, or press Enter to keep what is ticked. When a value is refused, only that
question is asked again.

With standard input from a file or a pipe, each answer is one line of it, in order, and
a secret is read the same way. A setup that reaches the end of its input before it has
asked everything is abandoned and stores nothing. For `plugin secret set`, and for
`speech set-token` and `assistant set-key`, a terminal gets one line, ended with Enter,
and a file or pipe is taken whole, without its last line ending.

## Devices, found and approved

Some devices are named in the integration's settings (a TV, a printer, sockets).
Others are **found**: lights on a bridge, bulbs answering a broadcast, VMs on a host.
A found device is only offered; you choose which to add. The hub only ever sends to
an address it found itself, so nobody can make it send somewhere else by naming an
address.

Computers are different: a VM or a host that a scan finds is listed as **pending**
and is never contacted until the owner approves it with a passkey.

```sh
sudo swifty device list
```

## The plugins

Each plugin's own README in [`plugins/`](../plugins) has its settings, its secrets
and its limits. In short:

### WiZ lights (`wiz`)

On and off, brightness (from 10%, the bulbs' own floor) and colour, over UDP port
38899 on your network. Setup: the broadcast address of your network, then a search
that offers every bulb that answers; or a list of bulb addresses. Discovery changes
nothing on a bulb. [plugins/wiz](../plugins/wiz/README.md)

### Philips Hue (`hue`)

The lights on a Hue Bridge: on and off, brightness, colour where the light has it.
Setup: the bridge's address, then **press the link button** on the bridge (the setup
waits up to 90 seconds), then pick the lights. The bridge's key is kept in the secret store.
[plugins/hue](../plugins/hue/README.md)

### Sonos (`sonos`)

Volume, mute, night mode, play and pause on Sonos speakers, over UPnP on port 1400.
A subwoofer can be added read-only. Setup: the speakers' addresses.
[plugins/sonos](../plugins/sonos/README.md)

### LG webOS TV (`webos`)

On, off, inputs and remote keys. Turning a TV on uses Wake-on-LAN, so the setup asks
for its MAC address and the TV must allow it. Setup: the TV's address, then **accept
the prompt on the TV**. On the secure port, the setup shows the fingerprint of the
TV's certificate for you to confirm the first time, and from then on trusts only
that certificate.
[plugins/webos](../plugins/webos/README.md)

### Tuya smart sockets (`tuya`)

On and off, and the USB port where a socket has one, over Tuya's local protocol 3.3
on port 6668. Each socket needs its *local key*. Setup: either type the id, address
and key of each socket, or let the setup look them up once in your Tuya developer
cloud project (an access id and secret, used for that lookup and not stored).
Protocols 3.4 and 3.5 are not supported yet. [plugins/tuya](../plugins/tuya/README.md)

### Toshiba air conditioners (`toshiba`): unofficial cloud

Power, mode, target temperature and fan, through Toshiba's Home AC Control cloud.
There is no local control. Setup: accept the disclaimer, sign in with the account of
the Toshiba app, pick the units. The cloud is slow and counts requests, so readings
are shared for a few minutes and a change may take up to half a minute to confirm.
[plugins/toshiba](../plugins/toshiba/README.md)

### Petkit feeder and litter box (`petkit`): unofficial cloud

Feed a portion (10 to 50 g) on a D4 feeder, start a cleaning on a T4 litter box, and
read the day's feeds and cleanings, through Petkit's cloud. Setup: accept the
disclaimer, choose the region, sign in, pick the devices. Use a **second Petkit
account** that the devices are shared to: an account is signed in on one device at a
time, so the main one would sign your phone app out. **Commands start switched
off**, so you can compare the readings with the real devices first; turn them on in
the integration's settings. [plugins/petkit](../plugins/petkit/README.md)

### Klipper printers (`moonraker`)

Start the loaded job, pause, resume and emergency stop, through Moonraker's web API.
Setup: Moonraker's address, and its API key if it asks for one.
[plugins/moonraker](../plugins/moonraker/README.md)

### Proxmox VE (`proxmox`)

Start, shut down and restart VMs, reboot or shut down the host, stored charts, and a
VNC console in the browser for the VMs you choose. Setup: how to create an API token
with just the rights it needs, then the host's address, node, token and certificate,
then which VMs to approve and which get a console. Every computer action is
critical: it asks for your passkey. Force-stop is never done.
[plugins/proxmox](../plugins/proxmox/README.md)

### Plex (`plex`) and IP cameras (`camera`)

Read-only: is the Plex server up, does the camera answer RTSP. The camera probe never
sends the camera's login. [plugins/plex](../plugins/plex/README.md),
[plugins/camera](../plugins/camera/README.md)

### Glimdock nodes (`glimdock`)

Read-only telemetry and the health page from a [Glimdock](https://github.com/gkostadinov/glimdock) node, and pairing of its
agents. [plugins/glimdock](../plugins/glimdock/README.md)

What a node reports shows on the host's page, for the owner:

- **Threads by computer.** The load on each thread of the host. When the node says
  which threads each computer is pinned to (the `affinity` of a Proxmox VM), each run of
  threads is drawn in that computer's colour, under a rule with its name, and the
  performance and efficiency threads of a hybrid processor are named under the chart.
  Computers that are not pinned are listed under it as running on any thread. A computer
  is named as you named it in the app, by its VM number, and as the node names it
  otherwise. Nothing is drawn that the node did not report.
- **Memory by computer.** One bar for the host's memory: the host memory each computer
  takes, the host's own use, and what is free, with each computer's use of what it was
  given.
- **Energy and its cost.** The kilowatt-hours the node has counted since its counter
  began, and what they cost. A node counts CPU and GPU only, so the page says that this
  is not the whole machine at the wall. Under it, the owner sets the electricity tariff:
  the price of a kWh and its currency (a three-letter code such as `EUR`), and for a
  two-rate meter, **Cheaper at night** with the night price and the hours night runs
  (such as 22:00 to 06:00), and if the supplier moves them in summer, **Different hours in
  summer time**, used while the clocks are on summer time. The hours are read in the time
  zone of the phone or computer the tariff was first set from, and the page says which.
  The hub adds up the cost about once a minute: each minute's energy at the price for that
  minute, so a reading that spans 22:00 is charged partly at each price, and the page shows
  the cost by day and at night. A tariff change counts from then on and never changes what
  was already counted. The first tariff also charges what was counted before it, once, at
  its day price, so the cost covers the whole counter. Changing the currency starts the
  cost again in the new one, because the two cannot be added. The cost is an estimate,
  shown with two decimals. With no tariff set, only the kilowatt-hours show.

### The demo home (`mock`)

Pretend devices of every kind, kept in memory. It reaches nothing.
[plugins/mock](../plugins/mock/README.md)

## About the unofficial cloud plugins

Toshiba and Petkit do not document their clouds for third parties. The `toshiba`
and `petkit` plugins work the way each vendor's own app does. That may be against
the vendor's terms, and it can break whenever the vendor changes something. The app
marks them **Cloud, unofficial**, and their setup begins with this warning. What
they learned about each protocol, and from whom, is credited in
[NOTICE](../NOTICE).

## A device that is not here

Plugins are Rust crates compiled into the hub. To use one that is not in the
official build, you build your own binary with it. To write one, start with the
[plugin developer kit](plugins/README.md), which includes a prompt to give a coding
assistant along with your device's documentation.

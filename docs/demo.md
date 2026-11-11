# The demo home in pictures

This page shows Swifty Home as it looks, using the **demo home**: eight pretend
devices that exist only inside the hub and reach nothing. Everything below is
from it. To try it yourself, see [Make the demo home yourself](#make-the-demo-home-yourself).

## A one-minute tour

[![A looping excerpt of the tour: unlock with a passkey, change the lights, run a scene, and confirm feeding the cat](media/swifty-home-tour.gif)](media/swifty-home-tour.mp4)

The picture above is a short loop. [The whole tour](media/swifty-home-tour.mp4) is
71 seconds, with no sound:

| From | What it shows |
| --- | --- |
| 0:02 | Unlocking with a passkey, Home, a light changed, a scene |
| 0:12 | An action that asks you to confirm first |
| 0:22 | Integrations, and what each one reaches |
| 0:30 | Adding the demo integration with the setup wizard, and its devices appearing |
| 0:51 | Swifty's memory |
| 0:59 | History, and the check of its chain |
| 1:09 | How to start |

## The screens

<img src="media/lock-screen.png" alt="The lock screen: a small 3D house in a round button, the word Sign in and the line Touch to use your passkey" width="390">

*The lock screen. The house is drawn in 3D by the page itself; one touch of your passkey opens it.*

<img src="media/home-phone-dark.png" alt="Home on a phone in the dark theme: Lights at 40 percent, Music and TV, the TV remote, a kettle socket that is on, the air conditioner, Pets and Gadget" width="320">

*Home on a phone, in the dark theme: lights, music and TV, sockets, air conditioning and pets.*

<img src="media/home-desktop-light.png" alt="Home on a desktop in the light theme with the Evening scene selected, lights at 40 percent and the TV, kettle and air conditioner below" width="720">

*Home on a desktop, in the light theme, with the Evening scene running.*

<img src="media/home-dry-run.png" alt="Home in dry-run: the heading says Dry-run, no device is contacted, and the tiles say not reporting" width="720">

*The same Home right after `swifty init --demo`, in dry-run: nothing is contacted, so the tiles say "not reporting".*

<img src="media/device-card.png" alt="The air conditioner's card: power on, mode Cool, a temperature of 22 degrees, fan speeds, a name and a room" width="720">

*A device card: the air conditioner, with its own power, mode, temperature and fan controls.*

<img src="media/confirm-sheet.png" alt="The Pets sheet asking Feed now or Cancel, with the line that food cannot be taken back" width="320">

*A consequential action asks first: feeding the cat cannot be taken back.*

<img src="media/step-up.png" alt="The set-up page of the Mock devices integration, asking to touch the passkey and then hold to continue" width="720">

*A critical action asks for your passkey again, then a hold: here, starting an integration's setup.*

<img src="media/integrations.png" alt="The list of integrations to add, with Local, Cloud and Unofficial badges" width="720">

*Adding an integration: each plugin says what it reaches, and the two cloud ones are marked Unofficial.*

<img src="media/setup-wizard.png" alt="The setup wizard's first step: eight demo devices to pick from, all switched on, and a Continue button" width="720">

*The setup wizard adding the demo integration: pick which pretend devices to bring in.*

<img src="media/memory.png" alt="Swifty's memory: one note about the person and one about the home, each with a Remove button" width="720">

*Swifty's memory: notes about you and about the home. They are notes, never permissions.*

<img src="media/history.png" alt="History: rows for the first passkey, sign-ins, a scene, feeding the pets and setting up an integration, and a button that checks the log" width="720">

*History: every action written down, and a button that checks the log's chain.*

<img src="media/cli-plugin-list.png" alt="A terminal showing swifty plugin list and swifty plugin list --available, run on the demo home" width="720">

*The terminal: `swifty plugin list` on the demo home, and every plugin in the build.*

## Make the demo home yourself

The demo home takes a few minutes. From a checkout of this repository, with Rust
installed:

```sh
cargo build --release
./target/release/swifty --config ./demo/swifty.toml init --yes --demo --data-dir ./demo/data
./target/release/swifty --config ./demo/swifty.toml serve
```

`init` prints a link with a one-time token; open it, create your passkey, and look
around. [First run](first-run.md) goes through it slowly, and
[Installing](install.md) has the install script and Docker routes.

A hub starts in **dry-run**: it shows what would happen and touches no device, so
the demo home's tiles say "not reporting" and a tap answers "simulated" (the
dry-run picture above). The other pictures show the hub in **live** mode. That is
safe here, because the demo home's only integration is the `mock` one, which has
no connection to anything. To see the same, stop the hub, then:

```sh
./target/release/swifty --config ./demo/swifty.toml go-live
./target/release/swifty --config ./demo/swifty.toml serve
```

`go-dry` takes you back. Do not do this on a hub that has real integrations until
you mean it; see [First run](first-run.md#8-real-devices-then-live-mode).

## How these were made

The pictures and the video were recorded from the demo home in a scripted, headless
Microsoft Edge on one computer, on `localhost`, and nothing else was involved: no real
device, account or data. A virtual passkey stood in for a touch, and the pointer
drawn on the video marks where the script clicked. Swifty's memory page appears only
when a model is configured, so the hub was pointed at a tiny pretend model on
loopback that answers every question with one fixed sentence. The terminal picture
is the real output of the two commands, set in a page. Before the wizard part, the
demo integration was removed with `swifty plugin remove mock`, so that adding it
shows its devices appearing in an empty home (History shows that row, marked `cli`).
The recording scripts are not part of this repository.

# First run

You have installed Swifty Home ([Installing](install.md)) and `swifty init` has
run. This page takes you from there to a home your family can use.

## 1. The address, chosen once

`swifty init` asked which address people will use to open the app:

- `http://localhost:8790` works in a browser **on the same machine** and needs
  nothing else. It is the right choice for a first look.
- A name with `https://`, such as `https://home.example.org`, works from phones and
  other computers. It needs a reverse proxy with a certificate: see [TLS](tls.md).

Passkeys are bound to that address for good. If you change it later, everyone adds
their passkeys again. So if you know you will use the hub from your phone, it is
worth setting up the name before you invite anyone.

## 2. Start the hub

If the install script started it, it is already running. Otherwise:

```sh
sudo systemctl start swifty-home
```

or, for a hub kept in a folder of its own, `swifty --config ./demo/swifty.toml serve`.

## 3. Create the owner

`swifty init` printed a link with a one-time token. It looks like
`<address>/#enroll=<token>`: the token is after the `#`, which a browser never sends to
a server or a proxy. Open the link and the app goes straight to making your passkey, with
the token filled in. Or open the plain address and paste the token. The token works once
and for 15 minutes. If it has expired, make a new one on the hub's machine:

```sh
sudo swifty enroll-token
```

It prints the token alone on its standard output and, on its standard error, the same
kind of link.

Your browser or phone asks to create a passkey for Swifty Home. That passkey is the
owner: the one person who can add devices, invite people and approve anything
critical. There is no password and no "forgot password": the passkey is the key.

## 4. Add a recovery passkey, today

If the owner's only passkey is lost, nothing on the page can let you back in. So,
before anything else, open **Passkeys**, add a second passkey on another device (a
second phone, a hardware security key) and mark it **recovery**. Keep it somewhere
safe. Swifty Home refuses to remove your last recovery passkey, and refuses to
remove the passkey you are signed in with.

Adding a passkey is a *critical* action, so it asks you to confirm with the passkey
you already have. You will see that pattern again.

## 5. Look around

If you chose the demo home, it is already furnished: a floor lamp, a ceiling light,
a kettle on a smart socket, a speaker, a TV, an air conditioner, a cat feeder and a
"gadget" with a custom action ([pictures of it](demo.md)). If you started empty, add the demo devices any time.
Without `--yes` the command asks which devices to add and for an optional access code,
one answer per line:

```sh
sudo swifty plugin add mock --yes
```

Things to try:

- **Tap a light.** It switches at once. Comfort actions like lights, music and the
  air conditioner just happen.
- **Feed the cat.** The app asks you to confirm first, because a feed cannot be
  taken back. That is a *consequential* action.
- **Ask Swifty.** Open the chat and type "turn the floor lamp off" or hold the
  microphone and say it. Swifty proposes; the same rules apply as for a tap.
- **Read the audit log.** Under **More → History**, every action is written down: who,
  what, on which device, through which channel, and how it ended.

While the hub is in dry-run, the results say **simulated**: the hub did everything
except reach the device. That is how you can try anything safely.

## 6. Rooms, names and favourites

Under **Rooms**, make rooms, put devices in them and give devices names your family
uses. Rooms bring their own scenes and let you say "living room off". Each person
has their own favourites and pinned rooms.

## 7. Invite your family

Under **People**, **Invite**. You confirm with your passkey, and the app shows a
one-time invitation, good for 24 hours and shown only once. Your family member opens
it on their phone and creates their own passkey.

Family members can use the lights, the music, the TV, the pets and see the camera.
Everything else (computers, the printer, sockets, people, passkeys, the audit log,
adding devices) is the owner's alone, and family members do not even see it.

## 8. Real devices, then live mode

1. Add your integrations: **Integrations → Add** in the app, or
   `sudo swifty plugin add <plugin>`. See [Integrations](plugins-user.md).
2. Check the hub: `sudo swifty doctor`.
3. When you are ready for commands to reach real devices:

   ```sh
   sudo swifty go-live
   sudo systemctl restart swifty-home
   ```

4. Check each device once while you watch it. The hub only reports success when the
   device's own reading agrees, so a device that does not answer shows as such,
   never as done.

`sudo swifty go-dry` and a restart bring you back to dry-run at any time.

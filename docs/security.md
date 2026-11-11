# Security

Swifty Home can open a door's worth of trouble: it turns things on and off, it
shuts down computers, it feeds the cat. This page says what protects each of those,
and, just as plainly, what does not. To report a problem, see
[SECURITY.md](../SECURITY.md).

## The rules everything else follows

1. **The hub decides; a plugin only drives.** A plugin is told one checked operation
   for one device and says "accepted" or why not. It never sees who asked, which tier
   the action has, a session or the audit log. Whether something worked is decided by
   the hub, from the device's own reading.
2. **Passkeys are the only way in.** There are no passwords anywhere.
3. **Dry-run is the default** and is built so that it cannot reach a device.
4. **Every action is written down** before any device is touched, in a log that
   cannot be edited without it showing.
5. **Secrets stay put.** Credentials live in the secret store and are never logged,
   returned by the API, written to the configuration or to the audit log.

## Passkeys, people and sessions

- **The first passkey** needs a one-time token from `swifty enroll-token`, run on the
  hub's machine: shown once, stored hashed, valid 15 minutes, refused once any
  passkey exists. That passkey makes the one owner.
- **More passkeys** are a critical action, confirmed with a passkey you already have.
  Every ceremony requires user verification (a fingerprint, a face, a PIN on the
  authenticator).
- **You cannot lock yourself out by accident.** The hub refuses to revoke the last
  owner passkey, the last recovery passkey, and the passkey the current session
  signed in with. Revoking a passkey ends its sessions.
- **Family** join by a one-time invitation (critical, shown once, good for 24 hours).
  They may use lights, music, the TV, the pets and see the camera. Everything else
  is owner-only, and a new kind of action starts out owner-only until it is
  deliberately opened to family. Family are not shown what they cannot use.
- **Sessions** live on the server, behind an opaque random id stored hashed, in a
  `__Host-` cookie that is `HttpOnly`, `Secure` and `SameSite=Strict`. A session
  lasts at most 8 hours and locks after 30 idle minutes (configurable, never longer).
  Passive refreshes do not count as activity, so an open tab still locks.
- **Origin.** Every request that changes something must carry exactly the configured
  `Origin`. There is no CORS.

## Tiers

Every action is in the hub's catalog with a tier, allowed targets, a parameter
schema with ranges, a plain description and its consequences. Plugins receive only
typed operations from a closed list: there is no raw command, URL, shell or
passthrough, and there never will be.

| Tier | For | What it takes |
| --- | --- | --- |
| 0, read | reading a state | a session |
| 1, comfort | lights, scenes, music, TV inputs and keys, sockets, air conditioning | a session; it just happens |
| S, safe direction | the printer's emergency stop | a session; by voice only when named |
| 2, consequential | TV power, the Movie scene, feeding, cleaning the litter box, printing, starting a VM | prepare, then confirm within 30 seconds |
| 3, critical | stopping or restarting VMs, host power, the console, approving machines, passkeys, people, adding or removing an integration | prepare, then a fresh passkey assertion, then confirm |

- A tier 2 confirmation is a single-use token bound to the action, target,
  parameters and session.
- A tier 3 step-up is a fresh WebAuthn assertion whose challenge is derived from a
  hash of exactly that action, target, parameters, session and prepared id. It is
  valid 60 seconds and works once. A signature for one action cannot approve
  another.
- A plugin's own custom actions are owner-only, never offered by voice or Swifty,
  and get at least the tier the hub's floors require: never read or safe-direction,
  at least consequential if repeating it is not harmless, critical on computers.
  The plugin asks; the hub decides.
- Things that cannot be undone are sent once. Repeats are suppressed by single-use
  tokens, an optional request id, an in-flight check and a duplicate window.
- **A result is only "confirmed" when the device's own reading agrees.** Silence is
  `no_response`, a refusal is `failed`, dry-run is `simulated`, a TV key press is
  `sent`. A device that does not answer shows as not available, never as a guess.

## Voice and Swifty

Voice and Swifty go through the same catalog, roles and tiers as a tap. Spoken
comfort actions run; consequential ones come back as a prepared action to confirm
on screen; critical ones can only be prepared. Nothing runs on a negation ("do not"),
an "or", low confidence, or more than one target or value, and a spoken "yes" is
ignored. Swifty proposes from a fixed list of home controls; computers, the console,
passkeys, people, integrations and custom actions are never offered. Swifty's memory
never grants anything, and refuses lines that look like a secret.

## Dry-run

With `live = false`, the default, nothing can reach a device. Five layers make sure:

1. **No grant.** Every function that opens a socket to a device needs a `LiveGrant`,
   which only a hub configured with `live = true` can make, in one place.
2. **No driver.** In dry-run the hub never connects a plugin. Each integration is a
   stand-in that answers with the plugin's own pure refusal rules, so dry-run
   predicts what live would refuse.
3. **No credentials.** Reading a secret's value needs the same grant.
4. **The registry returns early**: commands are `simulated`, readings are empty, and
   discovery, the console, charts and pairing do nothing.
5. **Tests that fail the build**: a source scan that allows socket code only in the
   network crate; a run of the whole catalog against every first-party plugin with
   trap listeners that must see no traffic; and the same trap test in the conformance
   suite every plugin must pass.

Only the exact line `live = true` in `swifty.toml` changes the mode, and
`swifty go-live` is the way to write it.

## The audit log

An append-only table in the database. Each row has the time, the action, the
target, the checked parameters, the person, the credential and session, the channel
(app, voice, Swifty, the command line), whether a step-up was used, the result, the
round-trip time and the run. The "commanded" row is committed before any device is
touched. Rows are hash-chained, and database triggers refuse to update or delete
them. Refused attempts are written too, using only catalog ids and checked values,
with a cap on the ones that need no secret to trigger.

Check it with `swifty audit verify`, or under **More → History**, with its **Check** button. What a check can prove
depends on what it is given:

| Given | Catches | Does not catch |
| --- | --- | --- |
| nothing | edits and accidents that do not also recompute the chain | someone who can write the database cutting the tail and repairing the head, or rebuilding the chain |
| the audit key (`[audit] key_file`, made by `swifty init`) | the above, because the head is signed with a key the database does not hold | someone who can read the key file; an older copy of the database restored together with its older signature |
| a checkpoint, `--expect-head SEQ:HASH` | anything that removes or replaces a row the checkpoint covers, including a full rebuild or a rollback | changes after the checkpoint |

So: the log shows that a row was changed or removed, not who did it, and it cannot
stop someone with root on the hub's machine. To catch even that, carry checkpoints
off the machine regularly. The hub logs an `audit checkpoint` line every five
minutes when it moves; `swifty audit verify` prints the current one.

```sh
sudo swifty audit verify
```

## Plugins: trust, said plainly

A plugin is Rust code compiled into the hub. It runs in the same process, with the
hub's rights. The contract, the reviews, the lints and the conformance suite catch
mistakes: a plugin that retries, leaks a credential into an error, opens a socket in
a pure function, or panics on a short reply. **They do not stop a hostile plugin.**
Only use plugins you trust as much as the hub itself: the official build has been
reviewed; a plugin you add to your own build is your call.

A plugin declares everything it reaches on the network, and the app shows that
before setup, but the declaration is not enforced. A panic in a plugin is contained
to that call, and an integration that keeps panicking is switched off, which limits
shared fate. It is not isolation.

Out-of-process plugins, which can be isolated, are on the [roadmap](roadmap.md).

## Threat model

Who and what Swifty Home defends against, and where it relies on you:

| Threat | What protects you | What you must do |
| --- | --- | --- |
| Someone on the internet | The hub listens on loopback only; the proxy terminates TLS; passkeys cannot be phished or guessed; sign-ins are capped | Keep `listen` on `127.0.0.1`, keep the proxy and the system updated, rate-limit `/api/v1/auth/` |
| Someone on your home network | Same, plus the `Origin` check and `__Host-` cookies. Found devices are only ever reached at addresses the hub found itself | Treat the LAN as shared: many devices (Hue, Plex, WiZ, Tuya, Moonraker) speak plain HTTP or UDP on it, as they always do. Use `https` in a device URL where the device offers it |
| A family member, or a stolen phone of one | Roles: family see and do only the family list. Critical actions need the owner's passkey | Revoke a lost passkey from **Passkeys**; it ends that person's sessions |
| A stolen unlocked owner phone | Sessions lock after 30 idle minutes and end after 8 hours. Critical actions still need the passkey again, with user verification | Revoke it from another passkey, ideally your recovery one |
| The speech link | The voice route refuses a speech service that is not on loopback, before any audio or token is sent | **The speech link is plain HTTP.** Keep the speech service on the hub's machine, or reach it through a tunnel (SSH, WireGuard) that ends on loopback. A forwarder on loopback still carries the audio and token off the machine in the clear, so the tunnel must encrypt |
| The assistant's model and agent | Swifty sends the model no secret; its actions go through the catalog and tiers; the agent is reached only on loopback | Run the model and agent on machines you trust. Anything you type to Swifty may reach them |
| A vendor cloud (Toshiba, Petkit) | Those plugins send only what their app would send, and their credentials stay in the secret store | Use a separate account where you can (Petkit needs one). These clouds are unofficial; see [NOTICE](../NOTICE) |
| Root on the hub's machine, or a stolen disk | Nothing in the hub. The secret store is plain files with tight permissions, because encrypting them with a key kept beside them would add nothing | Protect the machine: disk encryption, updates, SSH keys. Keep backups encrypted and off the machine; they contain the secrets ([Backups](backup.md)) |
| A hostile plugin | Nothing in the hub (see above) | Build only plugins you trust |

What Swifty Home does not do yet: per-request rate limiting beyond the cap on open
sign-ins and on anonymous audit rows (use the proxy), encrypted secrets with
`systemd-creds`, and isolated plugins. All three are on the [roadmap](roadmap.md).

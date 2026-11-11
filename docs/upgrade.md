# Upgrading

## To a new version of Swifty Home

Take a backup first; it takes a second ([Backups](backup.md)):

```sh
sudo swifty backup --out /var/backups/swifty-home
```

Then install the new version the way you installed the old one.

- **Install script:** run it again. It replaces `/usr/local/bin/swifty`, keeps
  `/etc/swifty-home/swifty.toml` and your data as they are, and restarts the service.

  ```sh
  curl -fsSL https://github.com/gkostadinov/swifty-home/releases/latest/download/install.sh | sh
  ```

- **Docker:** `docker compose pull`, then `docker compose up -d`.
- **From source:** `git pull`, `cargo build --release`, copy the binary, restart.

The new version updates the database by itself the first time it opens it, in one
transaction. Then check:

```sh
sudo swifty doctor
sudo swifty audit verify
```

Read [CHANGELOG.md](../CHANGELOG.md) before a minor version: within API v1 nothing is
removed or renamed, but the changelog says what is new and what is deprecated.

To go back, stop the service, put the old binary back and restore the backup taken
before the upgrade. Do not point an older version at a database a newer one has
updated.

## From glimdock-home

Swifty Home grew out of a private hub called glimdock-home. If you run one, a single
command moves it over, and leaves the old install untouched so you can go back.

What changes: the program is `swifty`, the service `swifty-home.service`, the user
`swifty-home`, the configuration `/etc/swifty-home/swifty.toml` (version 2), the
data `/var/lib/swifty-home`. Device credentials move from files named in the old
configuration into the secret store. The web address, the passkeys, the audit log,
the people, rooms, favourites, names, scenes and device targets stay the same.

**1. Install the program without starting it.** `swifty migrate` fills in a new install;
it does not make one. The install script puts the program, the service unit, the
`swifty-home` user and the empty directories in place and starts nothing, and asks
nothing, so it never gets in the way of the migration:

```sh
curl -fsSL https://github.com/gkostadinov/swifty-home/releases/latest/download/install.sh | sh -s -- --no-start --no-init
```

Without `--no-init` the script would also run `swifty init`, which makes a database,
and the migration refuses a place that already holds one.

**2. Look first.** `--dry-run` checks the old install, verifies its audit chain and
prints what the migration would do, and changes nothing:

```sh
sudo swifty migrate --from /etc/glimdock-home/config.toml --dry-run
```

A hub that was glimdock-home compiled in some identifiers that a vendor's cloud or an
agent already knows it by (Toshiba's client suffix, the agent's session prefix and
client header). This program does not guess them: a **site file** has to pin them, and
the migration refuses an old install that had Toshiba or an agent without them, saying
what to add. In the site file, give the `toshiba` instance `client_suffix`, and add

```toml
[hub.assistant.agent]
session_prefix = "..."
client_header = "..."     # the header's name, in lower case
client_name = "..."
```

with the values the old install used. The same file can keep the house's device names,
notes and scene levels. Pass it with `--site site.toml`, with its `[hub]` table. A key
the site file gives replaces the converted one, and a key it leaves out keeps the
converted value, so it can add and change but never remove: a key the old configuration
had (an agent's `pairing_path`, say) stays in the new instance until you export the site
file afterwards (`swifty config export`), delete the key and import it again.

The dry-run refuses while something still answers on the old address, as the real run
does. On a machine where step 1 has not been done yet, it says the new files would have
no owner to go to: there is no `swifty-home` user yet.

By default the new data goes to `/var/lib/swifty-home`; `--data-dir` says another place,
and `--config` where the new `swifty.toml` goes.

**3. Migrate.** Stop the old service, run the migration, start the new one:

```sh
sudo systemctl stop glimdock-home
sudo swifty migrate --from /etc/glimdock-home/config.toml --site site.toml
sudo systemctl start swifty-home
```

The migration:

1. refuses if the old hub still answers, if the new directories already hold a
   database, or without enough rights;
2. verifies the old audit chain with the old key and prints a checkpoint;
3. creates the new directories (under the user the installer made), and copies the
   database, the audit key and the other key files with their modes;
4. updates the copy to the new schema and turns each old device section into an
   integration, each credential file into a secret;
5. writes `swifty.toml` with the same address, `rp_id` and limits, and pins the few
   identifiers a vendor or an agent already knows the hub by;
6. applies the site file, if given;
7. ends every open session (people sign in again with the same passkey) and writes
   one audit row, `system.migrate`;
8. verifies the chain again, including the checkpoint from step 2, and prints a
   report with the commands to switch services and to roll back.

Nothing under `/etc/glimdock-home` or `/var/lib/glimdock-home` is changed.

**4. Check.**

```sh
sudo swifty doctor
sudo swifty audit verify
sudo swifty device list
```

Sign in with your existing passkey, look at your rooms and favourites, and try one
light, one confirmation and one step-up.

**Rolling back** is stopping `swifty-home` and starting `glimdock-home` again: the
old files were never touched. Keep them for a while before you remove them.

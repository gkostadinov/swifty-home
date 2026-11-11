# Backups

Everything a hub knows is in three places: `swifty.toml` (and the key files next to
it), the database, and the secret store. One command copies all three while the hub
keeps running:

```sh
sudo swifty backup --out /var/backups/swifty-home --keep-days 14
```

Each backup is a directory of its own inside the one given with `--out`, named by
the time it was taken, in UTC:

```text
/var/backups/swifty-home/
  swifty-home-20261110T043012Z/        mode 0700
    home.db                            the database, a consistent online copy
    config/
      swifty.toml                      the configuration
      audit.key, speech.token, ...     the key files it names, with their file names
      hosts/                           the files of further Proxmox hosts, if any
    secrets.tar                        the secret store, mode 0600
```

- `home.db` is taken with SQLite's backup, so it is consistent even while the hub
  writes, and it is checked before the backup is kept;
- `secrets.tar` is a plain tar archive of the secret store: one file per value;
- a backup is written under a hidden name and renamed when it is whole, so a backup that
  was interrupted never looks finished.

`--keep-days 14` removes backups in that directory older than 14 days, by the time in
their names. It touches only directories named like the ones above, never anything else
that happens to be in `--out`. Give it a number from 1 to 36500.

## Every night, by itself

The install script enables `swifty-home-backup.timer`, which runs exactly that
command at about 04:30 every night as the `swifty-home` user, with no network
access. Check it with:

```sh
systemctl list-timers swifty-home-backup.timer
```

## Keep a copy elsewhere

A backup on the same disk does not survive the disk. Copy
`/var/backups/swifty-home` somewhere else regularly: another machine, a NAS, an
encrypted cloud folder.

**A backup holds every credential of every integration**, readable by anyone who
can read the archive. Treat it like the keys to the house: store it encrypted, and
somewhere only you can read.

It is also a good moment to carry the audit log's checkpoint off the machine (see
[Security](security.md#the-audit-log)):

```sh
sudo swifty audit verify
```

## Restoring

On a fresh install (or after stopping the service):

1. Stop the hub: `sudo systemctl stop swifty-home`.
2. Put the files back where they came from, from one backup directory: the contents
   of `config/` into `/etc/swifty-home`, `home.db` as `/var/lib/swifty-home/home.db`, and
   the secret store unpacked with
   `sudo tar -xf secrets.tar -C /var/lib/swifty-home/secrets`. Keep the owners and modes
   as [Installing](install.md#what-goes-where) lists them.
3. Check and start:

   ```sh
   sudo swifty doctor
   sudo systemctl start swifty-home
   ```

Passkeys are bound to the address in `swifty.toml`, so restore onto a hub that is
reached at the same address. Sessions that were open when the backup was taken may
be valid again until they expire, eight hours at most.

## Docker

The configuration and data are the volumes `config` and `data`. Run the backup in a
container that mounts a host folder for `--out`, or back up the volumes with your
usual tool while the hub runs.

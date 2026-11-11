# Installing Swifty Home

Swifty Home is one program, `swifty`. It needs no database server, no runtime and
no internet connection to run. Pick whichever way below suits your machine. Each
ends the same way: `swifty init` has written a configuration and printed a
one-time token for your first passkey. Then go on to [First run](first-run.md).

**What you need**

- A machine that stays on: a small Linux server, a Raspberry Pi 4 or 5, a virtual
  machine, or any computer for a first look. 64-bit x86 or ARM.
- A browser with passkeys: any current Chrome, Edge, Safari or Firefox, and a phone
  or laptop that can hold a passkey.
- For use from a phone: a name with `https://`. See [TLS](tls.md). On the machine
  itself, `http://localhost:8790` works with nothing else.

## With the install script (Linux)

```sh
curl -fsSL https://github.com/gkostadinov/swifty-home/releases/latest/download/install.sh | sh
```

The script:

1. finds your architecture and downloads the matching release archive;
2. checks the archive against the release's signed list of checksums, and installs
   nothing that fails;
3. shows what it is about to change and asks before changing anything;
4. installs `/usr/local/bin/swifty`, creates the system user `swifty-home`, the
   directories `/etc/swifty-home`, `/var/lib/swifty-home` and
   `/var/backups/swifty-home`, and installs the service `swifty-home.service` and
   a nightly backup timer;
5. runs `swifty init`, which asks for the address people will use and whether to
   start with the demo home;
6. starts the service and prints what to do next.

It never overwrites an existing `/etc/swifty-home/swifty.toml`, so running it again
upgrades the program and leaves your setup alone.

Options, given after `sh -s --`:

| Option | Does |
| --- | --- |
| `--version VERSION` | install that version instead of the latest |
| `--origin URL` | the address people will use, passed to `swifty init` |
| `--demo`, `--no-demo` | start with the demo home, or empty, without asking |
| `--yes` | ask nothing; take the plan and every default (an empty home) |
| `--no-start` | install and configure, but do not start the service |
| `--no-init` | do not run `swifty init` (for example before `swifty migrate`) |
| `--dry-run` | download and check, then print every command instead of running it |
| `--archive FILE` | install from a release archive you already have |
| `--uninstall` | remove the program and the service; keep configuration and data |
| `--purge` | with `--uninstall`: also delete configuration, data, backups and the user |

For example, an unattended install for a phone-ready name:

```sh
curl -fsSL https://github.com/gkostadinov/swifty-home/releases/latest/download/install.sh | sh -s -- --yes --origin https://home.example.org
```

The hub starts in **dry-run**: it shows what would happen and reaches no device.
You switch it to live mode yourself, later, with `sudo swifty go-live`.

### Checking it

```sh
sudo systemctl status swifty-home
sudo swifty doctor
```

`swifty doctor` checks the configuration, the files and their permissions, the
database, the audit log and the secret store, and says how to fix anything it
finds. Logs are in the journal: `journalctl -u swifty-home`.

## With Docker

The image is small and runs as an unprivileged user. It uses **host networking**,
because discovering bulbs and waking a TV use broadcast packets that do not cross
Docker's network address translation. The hub still listens only on
`127.0.0.1:8790` of the host.

From a checkout of the repository:

```sh
cd packaging/docker
docker compose run --rm swifty-home init --yes --demo --origin http://localhost:8790
docker compose up -d
```

For a phone-ready name, use `--origin https://home.example.org` (your name) and
start Caddy as well, after editing `packaging/caddy/Caddyfile`:

```sh
docker compose --profile caddy up -d
```

Every other command runs the same way, for example
`docker compose run --rm swifty-home doctor`. More in
[packaging/README.md](../packaging/README.md).

## From source

You need Rust (`rustup` installs the pinned version by itself) and a C compiler for
the bundled SQLite. In a checkout of the repository:

```sh
cargo build --release
./target/release/swifty --config ./demo/swifty.toml init --yes --demo --data-dir ./demo/data
./target/release/swifty --config ./demo/swifty.toml serve
```

This keeps everything in `./demo` and needs no administrator rights. It is tested
on Linux and Windows; macOS should work but is not tested.

To install your own build as a service on Linux, build a release archive and point
the install script at it. `cargo xtask dist` needs the musl target
(`rustup target add x86_64-unknown-linux-musl`); `--without-notices` skips the
third-party notices, which need cargo-about:

```sh
cargo xtask dist --arch x86_64 --without-notices
sh packaging/install.sh --archive target/dist/swifty-home-0.1.0-x86_64-linux.tar.gz
```

### Without the script: the user and the directories

The script makes the `swifty-home` user and its directories (step 4 above). If you
install the program some other way and run it as a service, make them yourself, once,
before `swifty init`:

```sh
sudo useradd --system --user-group --home-dir /var/lib/swifty-home --no-create-home --shell /usr/sbin/nologin swifty-home
sudo install -d -m 0750 -o root -g swifty-home /etc/swifty-home
sudo install -d -m 0700 -o swifty-home -g swifty-home /var/lib/swifty-home /var/backups/swifty-home
```

`swifty`, run as root, changes to the user that owns the data directory before it
touches anything, so a command never leaves a file in it that the service cannot read.
The Docker image runs as an unprivileged user of its own and needs none of this; a hub
kept in a folder of your own (the `./demo` example) runs as you.

## What goes where

| Path | Holds | Owner, mode |
| --- | --- | --- |
| `/usr/local/bin/swifty` | the program | root, 0755 |
| `/etc/swifty-home/swifty.toml` | the [configuration](configuration.md) | root:swifty-home, 0640 |
| `/etc/swifty-home/audit.key` | the key that signs the audit log | swifty-home, 0600 |
| `/var/lib/swifty-home/home.db` | rooms, devices, people, passkeys, the audit log | swifty-home, 0700 directory |
| `/var/lib/swifty-home/secrets/` | the integrations' credentials, one file each | swifty-home, 0700 |
| `/var/backups/swifty-home/` | nightly [backups](backup.md) | swifty-home, 0700 |

## Uninstalling

```sh
curl -fsSL https://github.com/gkostadinov/swifty-home/releases/latest/download/install.sh | sh -s -- --uninstall
```

removes the program and the service and keeps your configuration and data, so a
later install picks up where you left off. `--uninstall --purge` deletes those as
well, and the `swifty-home` user. It asks you to type `delete` first, because it
cannot be undone.

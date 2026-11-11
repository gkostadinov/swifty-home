# Configuration

Swifty Home keeps what it knows in four places, each for its own kind of thing:

| Place | Holds | Written by |
| --- | --- | --- |
| `swifty.toml` (`/etc/swifty-home/swifty.toml`) | what the hub needs before it can open its database, and your policy: live or dry-run, the address, limits, network guards, paths | `swifty init`, `swifty go-live`/`go-dry`, or you, by hand |
| the database (`/var/lib/swifty-home/home.db`) | everything you arrange in the app: integrations and their settings, devices, rooms, scenes, names, notes, people, passkeys, sessions, the audit log, Swifty's memory | the hub and the `swifty` command, each change audited |
| the secret store (`/var/lib/swifty-home/secrets/`) | the integrations' credentials: one file per value, readable by the hub's user only | the setup of an integration, or `swifty plugin secret set` |
| key files named in `swifty.toml` | the audit key, the speech token, the assistant key | `swifty init`, `swifty speech set-token`, `swifty assistant set-key` |

Nothing is read from environment variables, and no secret ever goes into
`swifty.toml`, a log, an API answer or the audit log.

## `swifty.toml`

`swifty init` writes this file. You rarely need to touch it; when you do, the hub
refuses any key it does not know, so a typo or a secret pasted in by mistake stops
it with a clear message instead of being ignored. The full example, with every
optional section, is `packaging/swifty.example.toml`.

```toml
config_version = 2

# Dry-run is the default: no plugin is connected and no socket is opened.
# Only the exact line `live = true` lets a command reach a device.
live = false

# Keep this on loopback, behind a TLS reverse proxy.
listen = "127.0.0.1:8790"

# Everything the hub writes lives under here.
data_dir = "/var/lib/swifty-home"

# The device this hub itself runs on, if it is one of its own devices (a VM on a
# Proxmox host it controls). Stopping it then needs an extra acknowledgement.
# runs_on = "vm102"

# Required. Passkeys are bound to these for good.
[webauthn]
origin  = "https://home.example.org"
rp_id   = "home.example.org"
rp_name = "Swifty Home"

[session]
absolute_secs = 28800   # 8 hours, the most the hub allows
idle_secs     = 1800

[network]
connect_timeout_ms = 1500
read_timeout_ms    = 2500
# A further Proxmox host must have an address inside one of these.
allowed_host_networks = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"]

[audit]
key_file = "/etc/swifty-home/audit.key"
```

| Key | Means |
| --- | --- |
| `live` | `false`: dry-run, nothing reaches a device. Change it with `swifty go-live` and `swifty go-dry` |
| `listen` | where the hub listens. Keep it on `127.0.0.1` and put a proxy in front ([TLS](tls.md)); `swifty doctor` warns otherwise |
| `data_dir` | the database and the secret store. `database` and `secrets_dir` can point elsewhere |
| `runs_on` | the target of the device the hub runs on, if it controls it |
| `[webauthn]` | the address of the app. Required, with no default |
| `[session]` | how long a sign-in lasts, and after how long idle it locks |
| `[timing]` | how often the hub reads a device while it waits for a confirmation, and the window in which a repeated request is suppressed |
| `[network]` | timeouts for every device connection, and the networks a further Proxmox host may be in |
| `[audit]` | the key that signs the audit log. See [Security](security.md#the-audit-log) |
| `[speech]` | the optional local speech-to-text service, for voice |
| `[assistant]`, `[assistant.agent]`, `[assistant.weather]` | Swifty's models: a local model for quick chats, and optionally an external agent with an HTTP API on loopback for everything that is not about the home |
| `[health]` | an optional CPU canary file for the health page |

`swifty config check` reads the file and says exactly what is wrong, and
`swifty config show` prints what is in effect, with paths but never a secret.

```sh
sudo swifty config check
sudo swifty config show
```

### Voice and Swifty

Voice needs a speech-to-text service with an HTTP API. The optional Windows service
in `services/swifty-speech` is one. Its link is plain HTTP, so keep it on loopback
or inside a tunnel (see [Security](security.md#threat-model)). Put the token in a
file for a moment, store it, and delete the file:

```sh
sudo swifty speech set-token < speech-token.txt
```

Swifty's quick replies use a model behind an OpenAI-compatible API, such as a local
model server. Store its key, if it needs one, the same way:

```sh
sudo swifty assistant set-key < assistant-key.txt
```

At a terminal, both ask for the value on one line, typed or pasted without being shown,
and it ends with Enter. From a file or a pipe, all of standard input is the value.
Neither command ever prints it, and each writes one audit row that says which key was
set, never its value. A running hub reads these keys when it starts, so restart it.

## The site file

Everything that describes one house (integrations and their settings, device
names, notes, scene levels) can be written out as one TOML file, kept in a private
repository, and applied again. It never contains a secret.

```sh
sudo swifty config export --out site.toml
sudo swifty config import site.toml
```

`import` checks every integration with its plugin first and refuses a plugin this
build does not have. An integration it changes is connected anew by a running hub within
a second or two. `--prune` also removes the integrations, names and notes the file does
not list; it never removes scenes. A `[hub]` table in the file is only compared with
`swifty.toml`, never written: `import` says where they differ.

## Every `swifty` command

`swifty` is the hub and its command line in one program. Every command takes
`--config FILE` (default `/etc/swifty-home/swifty.toml`). When run as root, it
switches to the user that owns the data directory before it touches anything. It
never controls a device: that always goes through the app, with a passkey.

| Command | Does |
| --- | --- |
| `swifty init [--origin URL] [--listen ADDR] [--data-dir DIR] [--live] [--demo] [--yes]` | first run: writes `swifty.toml`, the directories, the audit key and the database, and prints a one-time token for the first passkey |
| `swifty serve` | runs the hub |
| `swifty enroll-token` | a new one-time token for the first passkey. The token is printed alone on standard output; the link that opens the app with it filled in (`<address>/#enroll=<token>`) is on standard error |
| `swifty go-live`, `swifty go-dry` | switch between live mode and dry-run; restart the service afterwards |
| `swifty plugin list [--available] [--json]` | the integrations, with their readiness and devices; it cannot show health, which only the running hub checks (the **Integrations** page shows it). `--available`: every plugin in this build. `--json`: what `GET /api/v1/plugins` answers |
| `swifty plugin info <plugin> [--json]` | what a plugin reaches on the network, its settings, its secrets and its actions. `--json`: its manifest, as `GET /api/v1/plugins/{plugin}` answers |
| `swifty plugin add <plugin> [--instance ID] [--set key=value]… [--yes]` | adds an integration with the plugin's guided setup, in the terminal. `--set` gives an answer ahead (never for a secret, because command lines are visible to other users); `--yes` takes every prefilled value and ticked choice and asks nothing, so it cannot answer a question that has no default |
| `swifty plugin setup <instance> [--set key=value]… [--yes]` | runs the setup again: a new pairing, a changed address. `--set` and `--yes` work as for `plugin add` |
| `swifty plugin remove <instance> [--yes]` | lists what goes (devices, room places, favourites, secrets), then removes it. Audit rows stay |
| `swifty plugin secret set <instance> <slot>` | stores one credential, read from standard input |
| `swifty plugin new <id> --class <class> [--name NAME] [--cloud] [--dir DIR]` | starts a new plugin from the template ([developer kit](plugins/README.md)) |
| `swifty device list [--json]` | every device: target, name, class, integration, status |
| `swifty config check [--file FILE]` | checks a configuration file; recognises a glimdock-home file and says what to run |
| `swifty config show` | the whole configuration in effect with every default written out, then each file and directory it names and whether it is there (and its mode). Never a secret |
| `swifty config export [--out FILE]`, `swifty config import FILE [--prune]` | the site file |
| `swifty doctor [--json]` | checks everything, each problem with its fix. Exits 1 on any error |
| `swifty audit verify [--expect-head SEQ:HASH] [--allow-empty]` | checks the audit log's chain and signature |
| `swifty backup --out DIR [--keep-days N]` | an online copy of the database, the configuration and the secrets ([Backups](backup.md)) |
| `swifty migrate --from FILE [--site FILE] [--data-dir DIR] [--dry-run]` | moves a glimdock-home install over ([Upgrading](upgrade.md)) |
| `swifty speech set-token`, `swifty assistant set-key` | store the speech token or the assistant key, read from standard input |
| `swifty openapi` | prints the OpenAPI document of the HTTP API |
| `swifty version [--plugins]` | the version, the API versions, and the plugins in this build |

A few to try on any hub:

```sh
swifty version --plugins
sudo swifty plugin list --available
sudo swifty device list
sudo swifty audit verify
```

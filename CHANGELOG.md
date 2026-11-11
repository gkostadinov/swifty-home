# Changelog

Every notable change to Swifty Home, newest first. Versions follow
[Semantic Versioning](https://semver.org/). Within API v1, nothing is removed or
renamed; a deprecated field or route is marked here, in the OpenAPI document and
with a `Deprecation` header for at least two minor releases.

## 0.1.0 (unreleased)

The first public release. Swifty Home grew out of a private hub, glimdock-home;
this is that hub, rebuilt around plugins and made ready for any house.

### Added

- **Plugins.** Every integration is its own crate behind one typed contract,
  `swifty-plugin-api` (plugin API 1), compiled into the binary: `wiz`, `hue`,
  `sonos`, `webos`, `tuya`, `toshiba`, `petkit`, `moonraker`, `proxmox`, `plex`,
  `camera`, `glimdock`, and `mock` for the demo home.
- **Guided setup** for every plugin, the same steps in the web app's new
  Integrations and Setup pages and in `swifty plugin add`: Hue pairing with the link
  button, LG TV pairing with the on-screen prompt, WiZ discovery, the Tuya local-key
  lookup, Proxmox token instructions and VM approval. They replace the old
  hand-edited files and helper scripts.
- **Devices by capability.** A device has a class and capabilities, and the catalog,
  policy, voice, Swifty and the app work from those. Plugins can declare custom
  actions; the hub applies tier floors to them.
- **The `swifty` command line**: `init` with an optional demo home, `doctor`,
  `go-live` and `go-dry`, `plugin list|info|add|setup|remove|secret set|new`,
  `device list`, `config check|show|export|import`, `backup`, `migrate`,
  `audit verify`, `openapi`, `version`.
- **Configuration version 2** (`swifty.toml`), a secret store for credentials, and a
  site file for the description of one house.
- **API v1** under `/api/v1`, with `GET /api/v1/meta`, an OpenAPI document, the
  `X-Swifty-Api` header and a Server-Sent Events stream at `/api/v1/events`.
- **Packaging**: static Linux binaries for x86_64 and aarch64, an install script with
  signed checksums, hardened systemd units with a nightly backup timer, a Docker
  image, and Caddy and nginx recipes.
- **A plugin developer kit**: generated reference, a template, the conformance
  suite, and a prompt for coding assistants.
- **`swifty migrate`**, which moves a glimdock-home install over in one command and
  leaves the old one untouched.
- **Threads by computer, memory by computer, energy and its cost** on the host's page.
  From a Glimdock node's snapshot, the load on each thread is grouped by the computer
  pinned to it (and the performance and efficiency threads are named), the host's memory
  is split by computer, and the energy counter shows with its cost at the owner's
  electricity tariff: one price, or a day and a night price with the night hours (and
  other hours in summer time, if the supplier moves them), each minute charged at its own
  price. The hub keeps a cost ledger per telemetry device in the database (schema 5, added
  to an existing database when the hub starts; nothing in it is changed). New routes
  `GET /api/v1/energy` and `PUT /api/v1/energy/tariff`, owner only; the new audit action
  is `energy.tariff`.

### Changed

- The web app is split into ES modules with no build step, under a stricter content
  security policy, and chooses each card and control from a device's class and
  capabilities.
- Action ids are generic: `bulb.*` became `light.*` and `sonos.*` became
  `speaker.*`. The old ids are still accepted as aliases for the life of API v1, and
  old audit rows keep them.
- State kinds `bulb`, `hue` and `plex` became `light`, `bridge` and `service`.
- The session cookie is `__Host-swifty-session`. Upgrading ends open sessions once;
  passkeys keep working.
- The speech service is `swifty-speech` (Windows service `SwiftySpeech`), with its
  own default port, 8791.

### Removed

- The unversioned `/api/*` routes. Everything is under `/api/v1/*`.
- Everything that described one particular house: device names, VM descriptions,
  fixed machine lists and hardware names left the code. They now come from the
  database and the site file.

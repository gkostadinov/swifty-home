# Packaging

What each file here is for. How to install is in [docs/install.md](../docs/install.md);
how to put a certificate in front is in [docs/tls.md](../docs/tls.md).

| File | What it is |
| --- | --- |
| `install.sh` | The installer for Linux (x86_64, aarch64). POSIX `sh`, so `curl … \| sh` works with dash. Checks the release's signed checksums, asks before changing anything, never overwrites `swifty.toml`. `--help` lists its options |
| `swifty.example.toml` | The configuration with every optional section, commented. `swifty init` writes the same shape |
| `systemd/swifty-home.service` | The hub's unit: runs `swifty serve` as `swifty-home`, with the file system read-only except its state directory, no capabilities, `MemoryDenyWriteExecute`, a system call filter and only the address families it needs |
| `systemd/swifty-home-backup.service`, `.timer` | `swifty backup` every night around 04:30, with no network access |
| `docker/Dockerfile` | Two stages: a static musl build, then a distroless image that runs as a non-root user |
| `docker/docker-compose.yml` | The hub with host networking (broadcast discovery and Wake-on-LAN need it), named volumes for configuration and data, a read-only root, and Caddy behind the `caddy` profile |
| `caddy/Caddyfile` | Caddy in front of the hub: a public certificate, or `tls internal` for a home network only |
| `nginx/swifty-home.conf` | A generic nginx template: HTTPS redirect, no buffering for the event stream, the console's WebSocket upgrade, larger bodies for voice, a rate limit on sign-in |
| `notices/about.toml`, `about.hbs` | How the release builds `THIRD-PARTY-NOTICES.txt` with cargo-about |

## Release archives

`cargo xtask dist` builds `swifty-home-<version>-<arch>-linux.tar.gz` with the
binary, the three units, `swifty.example.toml`, `LICENSE`, `NOTICE` and
`THIRD-PARTY-NOTICES.txt`, and a `SHA256SUMS` beside it. The release workflow
(`.github/workflows/release.yml`) signs `SHA256SUMS`, fills in the install script's
repository, version and public key, and publishes the archives and the image.

Try the installer against a local archive, without a release (`--without-notices`
skips cargo-about, for local use only):

```sh
cargo xtask dist --arch x86_64 --without-notices
sudo sh packaging/install.sh --archive target/dist/swifty-home-0.1.0-x86_64-linux.tar.gz --dry-run
```

`--dry-run` prints every command it would run and changes nothing.

## Docker

```sh
cd packaging/docker
docker compose run --rm swifty-home init --yes --origin https://home.example.org
docker compose up -d
docker compose run --rm swifty-home doctor
```

The image name in `docker-compose.yml` is the one the project publishes with each release,
`ghcr.io/gkostadinov/swifty-home`; `docker compose build` builds it from the checkout instead.

## Release signing

Each release's `SHA256SUMS` is signed with an Ed25519 key in the namespace
`swifty-home-release` (see the top of `release.yml`). Its public half is
`packaging/release-signing.pub`, which the install script checks the signature against; its
private half is the repository secret `RELEASE_SIGNING_KEY`. The release stops without
either. Fingerprint: `SHA256:dOx4mcjXKFViGwLvVj4hTcHlFeiK45d6eIxtpBNCles`.

To check a release by hand:

```sh
printf 'swifty-home-release namespaces="swifty-home-release" %s
' "$(cat packaging/release-signing.pub)" > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I swifty-home-release -n swifty-home-release -s SHA256SUMS.sig < SHA256SUMS
```

A new key means a new `release-signing.pub` and a new secret; releases signed before it
keep verifying against the key their own install script names.

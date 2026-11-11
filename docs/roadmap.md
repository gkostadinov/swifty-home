# Roadmap

What 0.1.0 ships, and what is planned next. Nothing here is promised by a date;
the order is roughly the order of the rows.

| Area | In 0.1.0 | Next |
| --- | --- | --- |
| Plugins | 13 first-party plugins, compiled in, each with guided setup | a `bridge` plugin: plugins in their own process, over JSON-RPC on standard input and output, installable without a rebuild and in any language; a community plugin registry |
| Setup | the web wizard and `swifty plugin add`, one step engine | passive "found on your network" (mDNS and SSDP), Sonos discovery |
| Devices | today's operations, named by capability, plus plugin-declared custom actions | merging the class-specific power operations, per-device audience overrides, a generic read-only switch, Tuya protocols 3.4 and 3.5 |
| API | `/api/v1`, OpenAPI, Server-Sent Events, `meta` | bearer tokens for paired native apps ([native apps](roadmap/native-apps.md)), a server-side poller, push from devices |
| App | embedded, modular, drawn from capabilities, with Integrations and Setup pages | a scene editor; a desktop shell around the same page if it is ever needed |
| Security | passkeys, tiers, the audit chain, dry-run | secrets wrapped with `systemd-creds`, isolated out-of-process plugins, rate limits in the hub |
| Packaging | static Linux binaries for x86_64 and aarch64, the install script, systemd units, Docker, Caddy and nginx recipes | a `.deb` and an apt repository, built-in certificates (ACME) |
| Developer kit | contract docs, generated reference, template, `swifty plugin new`, an AI prompt, the conformance suite | publishing the contract crates on crates.io, a review process for community plugins |

Not planned: WebAssembly plugins, until WASI can do UDP broadcast and custom TLS
verification; a framework rewrite of the web app.

Ideas and votes are welcome as issues; see [CONTRIBUTING.md](../CONTRIBUTING.md).

# Security policy

Swifty Home runs a home's lights, locks-adjacent devices, machines and cameras, so
security reports come first.

## Reporting a vulnerability

Please do not open a public issue for a security problem. Use GitHub's private
vulnerability reporting on this repository ("Security" tab, "Report a
vulnerability"). Include what you found, how to reproduce it, and which version or
commit you used.

You can expect an acknowledgement within 7 days and a first assessment within 14.
Once a fix is released, you are credited in the release notes unless you ask not
to be.

## What is in scope

- The hub (`swifty-hub`): authentication, sessions, the action tiers and their
  confirmations, the audit log, the API and the web app.
- The plugin host and the official plugins in this repository.
- The installer and the packaged service configuration.

Third-party plugins are their authors' responsibility; report problems in them to
their authors, and tell us if a plugin abuses an API we provide.

## Design rules a fix must keep

- Passkeys are the only way in. There are no passwords.
- The hub, never a plugin, decides an action's tier and what it needs: a session,
  a confirmation, or a fresh passkey.
- Dry-run never opens a network connection to a device.
- Secrets are never logged, never returned by the API and never written to the
  configuration file.
- The audit log is append-only and hash-chained.

See [docs/security.md](docs/security.md) for the full model.

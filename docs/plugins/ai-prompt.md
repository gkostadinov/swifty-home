# A prompt for a coding assistant

Paste the text below into a coding assistant that can read and write files in a checkout of
the swifty-home repository, together with the device's documentation. Fill in the six lines
under "What I want" first. The prompt sends the assistant to the pages of this directory and
to the template, and holds it to the rules the conformance suite and review check.

It was written for a newcomer's first plugin. It works best when the assistant can run
`cargo` in the checkout, so it can make the tests pass rather than only write them.

```text
You are writing an integration plugin for Swifty Home, in Rust, against the crate
`swifty-plugin-api` (plugin API 1). Swifty Home is a self-hosted smart-home hub with a strict
security model. The hub decides what may happen. A plugin only knows how to talk to one kind of device.

Before you write anything, read these files in the swifty-home repository. They are the only
source of truth for the API. Do not invent types, methods or capabilities that are not in them.
  docs/plugins/contract.md
  docs/plugins/how-to.md
  docs/plugins/reference/capabilities.md   (generated: classes, capabilities, ops, tiers)
  docs/plugins/reference/setup-steps.md    (generated: step and field types)
  docs/plugins/reference/manifest.md       (generated: manifest fields)
  templates/plugin/                        (start from this: `cargo run -p swifty -- plugin new <id> --class <class>`)
  plugins/wiz/ and plugins/hue/            (a small local plugin, and one with pairing)

What I want:
  Device or service:      <NAME AND MODEL>
  Plugin id:              <lowercase-id>
  Device classes:         <light | plug | aircon | speaker | tv | feeder | litter | printer | vm |
                           host | router | camera | service | telemetry | other>
  What it should do:      <for example: on and off, brightness, read power use>
  How it is reached:      <LAN protocol and port, or the vendor cloud and its host names>
  Protocol documentation, captures, or an existing open-source client and its licence:
                          <PASTE OR LINK>

Rules. Each one is checked by the conformance suite or by review.

 1. The hub decides, the plugin drives. Never implement tiers, roles, confirmation, retries or
    policy. `command` sends one operation once and returns as soon as the device has accepted it.
    The hub then calls `read_state` and decides whether it worked.
 2. Map the device onto the existing classes, capabilities and `Op` variants in capabilities.md.
    Only when nothing fits, declare a custom action in the manifest. Ask for the highest tier that
    is honest: anything irreversible, costly, or felt by other people is Consequential or Critical.
    Say truthfully whether repeating it is harmless (`idempotent`), and how `read_state` shows that
    it worked (`Expect`). There is no raw command, URL, path or payload passthrough, and you must
    not add one.
 3. `validate`, `readiness`, `devices` and `refusal` are pure: no sockets, no files, no clock, no
    secret values. `refusal` must refuse everything the device would not accept (an unknown device,
    a value out of range, a feature it lacks) in one plain sentence a person can act on. Dry-run
    shows the same refusals.
 4. All network I/O goes through `swifty_plugin_api::net`, with the `LiveGrant` and `Timeouts` from
    `LiveContext`. Do not depend on or name `std::net`, `tokio::net`, `reqwest`, `hyper`,
    `tungstenite` or any other client. Address types (`IpAddr`, `SocketAddr`) come from
    `swifty_plugin_api::net` too. No retries, no redirects, no proxies. No background task
    that talks to the device.
 5. `read_state` reports what the device says now, in the `State` variant for its class. A value
    the device does not report is `None`, never a guess and never the last thing you sent.
 6. Secrets (keys, tokens, the sign-in for a vendor account) are declared as secret slots in the
    manifest, collected by the `SetupFlow` in `Secret` fields, returned in `SetupResult.secrets`,
    and received in `LiveContext.secrets`. Never put one in config, a URL query, an error, a log
    line, a `Debug` output or a `State`. Error text is fixed English plus at most a number. Never
    echo what the device sent. Pass device-supplied names through `net::device_text`.
 7. Nothing about one particular home: no addresses, names, accounts or time zones in the code.
    Everything site-specific comes from config fields or setup steps. Examples use 192.0.2.x and
    example.org.
 8. Setup uses only the documented step types and asks for as little as possible. If the device
    needs a button press or an on-screen prompt, use a `Wait` step. Offer what was found with a
    `Choose` step. When `SetupContext.grant` is present, end by reading each chosen device once and
    fail with a helpful reason if that read fails. When it is `None` (dry-run), do what needs no
    network, and otherwise return `Failed` saying that live mode is needed.
 9. Declare in the manifest exactly what the plugin reaches: each port, broadcast, and each cloud
    host name. If it uses a vendor cloud that the vendor does not document for third parties, set
    `official: false`, say so in the README, and make the first setup step say it too.
10. `#![forbid(unsafe_code)]`. No `unwrap` or `expect` on anything that comes from a device, a file
    or the network. No blocking calls in async code. No process execution, no environment
    variables, no file access.

Tests you must write and make pass:
  - unit tests for every frame you encode and decode, built from the documentation, including a
    malformed reply;
  - tests/mock_device.rs: a loopback mock of the device built with `swifty_plugin_testkit`, and for
    each supported op: the bytes sent, the state read back, `op.is_confirmed_by(&state, baseline)`,
    a timeout against a silent mock, and a refusal;
  - tests/conformance.rs: `swifty_plugin_testkit::conformance::run_every_check(&PLUGIN, fixture().await)`;
  - `cargo test` and `cargo xtask check-plugin <path>` pass with no warnings.

Deliver every file of the crate in full, including README.md (what it supports, how setup goes,
what it reaches on the network, what was and was not tested on real hardware) and DEVELOPING.md
(protocol notes, quirks, sources and their licences). Finish with a list of every assumption the
documentation did not settle, and what a person must check on the real device before relying on it.
```

## Why the prompt says what it says

Four parts of the prompt follow the code closely, so a change there means a change here:

- in a checkout the command is run through cargo (`cargo run -p swifty -- plugin new`),
  because the `swifty` binary is not installed there;
- rule 4 says where the address types come from, because `swifty_plugin_api::net`
  re-exports them so that a plugin never names `std::net`;
- the conformance fixture is built with `.await`, because the test kit's stand-ins start
  asynchronously;
- the test calls `run_every_check`, which also fails when a check could not run with the
  fixture, so that a stand-in that cannot spoil its replies or report a name is noticed.

## After the assistant answers

1. Read its list of assumptions. Each one is a question for the device, not for the code.
2. Run `cargo xtask check-plugin <id>` yourself. It must pass with no warnings.
3. Read `refusal`, the tiers it asked for, `idempotent` and `Expect` with the device's
   documentation open: those are where a plugin that passes every check can still be wrong.
4. Try it on the real device as [how-to.md](how-to.md#11-run-it-in-a-hub) describes, first in
   dry-run.

# Writing a plugin for Swifty Home

A **plugin** teaches Swifty Home to talk to one kind of device or service: a make of
light, a speaker system, a printer's API, a vendor's cloud. It is a small Rust crate that
implements the traits of [`swifty-plugin-api`](../../crates/swifty-plugin-api) and is
compiled into the `swifty` binary.

The one idea to keep in mind: **the hub decides, the plugin drives.** The hub owns
everything that is about people and risk: who may do what, which actions need a
confirmation or a passkey, retries, the audit log, and whether a command really worked.
A plugin only knows how to turn one operation into bytes for its device, and how to read
the device's state back. It never sees a person, a tier or a session.

## The pages

| Page | Read it when |
| --- | --- |
| [how-to.md](how-to.md) | You are writing a plugin: from `swifty plugin new` to a merged pull request, step by step |
| [contract.md](contract.md) | You want the rules, with the reasons: tiers, confirmation, dry-run, secrets, refusals |
| [testing.md](testing.md) | You are writing tests: the test kit, mock devices and the conformance suite |
| [ai-prompt.md](ai-prompt.md) | You want a coding assistant to write the plugin from the device's documentation |
| [reference/](reference/README.md) | You need the exact shape of something: every class, capability, action, operation, state, manifest field, setup step and error, with its JSON. Generated from the code |
| [bridge.md](bridge.md) | You wonder about plugins in other languages: the roadmap |

The API itself is documented in its rustdoc: `cargo doc -p swifty-plugin-api --open`.

## A plugin's life in one picture

```text
                      the plugin's crate                         the hub
                      ------------------                         -------
 compile time         MANIFEST (static data)  ---------------->  catalog, voice words,
                                                                 app, `swifty plugin info`

 setup                SetupFlow: Form, Wait, Choose, External    draws each step (web or
                      -> Done { config, secrets, adopt }  ---->  terminal), stores config,
                                                                 secrets and devices

 every load           validate(config)              pure         dry-run calls only these
 (dry-run and live)   readiness(instance)           pure         four, so dry-run predicts
                      devices(instance)             pure         exactly what live would
                      refusal(instance, device, op) pure         refuse

 live only            connect(LiveContext) -> Driver             gives the LiveGrant,
                                                                 the timeouts, the secrets

 each action          (hub: catalog, policy, tier, confirm or passkey, audit)
                      refusal(...)            -> None
                      Driver::command(device, op)  -> sent once
                      Driver::read_state(device)   -> State      hub: is_confirmed_by?
                                                                 confirmed / no_response /
                                                                 failed
```

## What a plugin may do, and what it may not

A plugin **may**:

- describe its devices: a class each (`light`, `plug`, `tv`, ...) and what each can do
  (`power`, `brightness`, ...), from the instance's config or from devices the hub has
  adopted;
- send one operation to one device when asked, through `swifty_plugin_api::net`, with the
  grant and the timeouts the hub hands it;
- read a device's state and report exactly what the device said;
- look for devices that could be added (`Driver::discover`), without adding them;
- guide a person through setup with steps that are data;
- declare a custom action when no standard operation fits, with a requested tier that the
  hub may raise.

A plugin **may not**:

- decide whether something is allowed, how risky it is, or whether it worked;
- retry a command, keep a background task talking to a device, or poll on its own;
- open a socket any other way than `swifty_plugin_api::net`, run a process, read an
  environment variable or a file;
- touch the network in `validate`, `readiness`, `devices` or `refusal`;
- put a secret anywhere but a declared secret slot, or let one reach an error, a log line,
  a URL query or a state;
- offer a way to send a raw command, URL, path or payload to a device;
- hold anything about one particular home: addresses, names, accounts, time zones.

The conformance suite checks most of this on every plugin, and review checks the rest.

## What you should know before you start

A plugin is Rust, is compiled into the hub, and **runs with the hub's rights**. The rules
above, the lints and the conformance suite catch mistakes. They do not stop a plugin that
sets out to do harm, so getting into the official build means review. Using a plugin that is
not in the official build means building your own binary (`cargo xtask add-plugin`, then
`cargo build --release`). Installing a plugin without a rebuild, and plugins in other
languages, arrive with the bridge ([bridge.md](bridge.md)).

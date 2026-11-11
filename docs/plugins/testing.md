# Testing a plugin

A plugin is tested without its device: a stand-in on loopback plays the device, trap
listeners prove that nothing else was contacted, and the conformance suite checks the rules
of [contract.md](contract.md) against your plugin. Every test runs in `cargo test`.

## The three layers

| Layer | Where | What it proves |
| --- | --- | --- |
| Unit tests | `src/protocol.rs`, `src/config.rs` | Each frame encodes and decodes as the documentation says; a malformed reply is an error; each config rule refuses what it should |
| Mock device tests | `tests/mock_device.rs`, with the stand-in in `tests/common/mod.rs` | For each operation: the bytes sent, the state read back, `op.is_confirmed_by(&state, baseline)`, a timeout against a silent device, a refusal that sends nothing; the setup flow reaches `Done` |
| Conformance | `tests/conformance.rs` | The twelve checks every plugin passes |

`swifty plugin new` writes all three for the template's stand-in protocol. When you change
the protocol, change the stand-in to match, keep every test, and add the ones your device
needs.

## The test kit: `swifty-plugin-testkit`

A dev-dependency of every plugin. Tests use it instead of sockets of their own:
`cargo xtask check-plugin` refuses `std::net` and `tokio::net` in `tests/` as well as in
`src/`.

| Module | What is in it |
| --- | --- |
| `devices` | Generic stand-ins: `MockUdp`, `MockTcp`, `MockWs`, `MockHttp`; TLS ones in `devices::tls` (`PrivateCa`, `MockTlsHttp`, `MockTlsWs`); and a stand-in for each first-party device (`MockBulb`, `MockTv`, `MockSonos`, ...), which show how far a stand-in can go |
| `trap` | `Trap` and `Traps`: loopback addresses that count every connection and datagram and answer nothing |
| `conformance` | `Fixture`, `MockDevice`, `run`, `run_every_check`, `report`, `Rigged`, `Switches`, `Fault`, `TIMEOUTS` |

## Writing the stand-in

The stand-in answers like your device, from a handler you write. The template's keeps one
set of fields per device, stores what it is sent, and answers a read with the fields:

<!-- quote templates/plugin/tests/common/mod.rs "fn answer(states" "=}" -->
```rust
fn answer(states: &Mutex<Devices>, sets: &Mutex<Changes>, switches: &Switches, body: &[u8]) -> Value {
    let Ok(request) = serde_json::from_slice::<Value>(body) else {
        return json!({ "ok": false, "error": "not JSON" });
    };
    let Some(id) = request.get("id").and_then(Value::as_str) else {
        return json!({ "ok": false, "error": "no id" });
    };
    let mut states = states.lock().unwrap();
    let Some(state) = states.get_mut(id) else {
        return json!({ "ok": false, "error": "no such device" });
    };
    if let Some(set) = request.get("set").and_then(Value::as_object) {
        sets.lock().unwrap().push((id.to_string(), set.clone()));
        apply(state, set);
        return json!({ "ok": true });
    }
    json!({ "ok": true, "state": reported(state, switches) })
}
```
<!-- /quote -->

Make yours behave like the device, including how it refuses: the conformance suite and your
own tests can only find what the stand-in can show. Pick the generic stand-in for your
transport:

| Transport | Stand-in | Its handler | Silence | Notes |
| --- | --- | --- | --- | --- |
| UDP | `MockUdp::start(handler)` | `Fn(&[u8]) -> Option<Vec<u8>>`: one datagram in, at most one out | `device.silent.store(true, ..)` | `addr` is where it listens; `datagrams` holds what arrived. A reply over 2 kB does not fit the helpers' buffer |
| TCP | `MockTcp::start(handler)` | `Fn(&[u8]) -> Option<Vec<u8>>`, for each chunk read on a connection | `device.hang.store(true, ..)` | It keeps every connection open after a reply, as a device that serves several requests does. For a device that answers and then hangs up, start it with `MockTcp::start_closing(handler)` (or set `device.close_after_reply`): the client reads the answer and then the end of the stream. A driver that reads until the device closes will time out against the plain one: end the stand-in's replies the way the device does (`\r\n\r\n` for `net::tcp_exchange`), or read a known length with `TcpLink::read_exact` |
| HTTP | `MockHttp::start(handler)` | `Fn(&Seen) -> (u16, String)`: status and body | `device.hang.store(true, ..)`, or `hang_posts` for POST only | `url` is `http://127.0.0.1:<port>`, without a slash; when your config wants a host and a port, the port is the number after its last `:`. `Seen` has `method`, `path`, `query`, `headers` (`seen.header("name")`) and `body`. Faults are built in (`switches`) |
| Websocket | `MockWs::start(handler)` | `Fn(&str) -> Option<String>`: one text frame in, at most one out | `device.hang.store(true, ..)` | `url` is `ws://127.0.0.1:<port>` |
| HTTPS, `wss` | `devices::tls::{MockTlsHttp, MockTlsWs, PrivateCa}` | fixed answers | | For a plugin that checks a CA or pins a certificate |

Every stand-in counts what reached it (`traffic()`), which is how a test proves that a
refusal sent nothing.

For a test that needs a port where **nothing listens**, start a stand-in, note its port, and
drop it. Never bind a socket yourself.

## The conformance suite

```rust
swifty_plugin_testkit::conformance::run(&PLUGIN, fixture).await;
```

`run` executes the twelve checks and panics once with every problem, each labelled
`[check N name]`. `run_every_check` does the same and also fails when a check could not run
with your fixture; the first-party plugins use it. `report` hands back the problems and the
"not run" list instead of panicking, which helps while you work:

```rust
let report = swifty_plugin_testkit::conformance::report(&PLUGIN, fixture).await;
println!("{report}");
```

A real one, from the WiZ plugin:

<!-- quote plugins/wiz/tests/conformance.rs "async fn conformance()" "=}" -->
```rust
async fn conformance() {
    let bulb = MockBulb::start().await;
    let address = bulb.addr.to_string();
    let addresses: BTreeMap<String, Scalar> = [
        ("broadcast".to_string(), Scalar::Text(String::new())),
        ("addresses".to_string(), Scalar::Text(format!("bulb1={address}"))),
    ]
    .into();
    let setup = vec![SetupInput::Values { values: addresses }];
    assert_reaches_done(&setup).await;
    let fixture = Fixture {
        config: json!({ "bulbs": [{ "id": "bulb1", "address": address }] }),
        device: Box::new(bulb),
        ops: vec![
            ("bulb1".to_string(), Op::LightPower { on: false }),
            ("bulb1".to_string(), Op::LightBrightness { percent: 40 }),
            ("bulb1".to_string(), Op::LightHue { hue: 120 }),
            ("bulb1".to_string(), Op::LightSet { percent: 70, hue: 200 }),
        ],
        setup,
        ..Fixture::empty()
    };
    run_every_check(&PLUGIN, fixture).await;
}
```
<!-- /quote -->

### The fixture

| Field | What to put in it |
| --- | --- |
| `config` | An instance config that passes `validate` and points every device at the stand-in |
| `secrets` | `(slot, value)` for every slot the instance needs. Use values of at least 4 characters that appear nowhere else: the suite searches every output for them |
| `adopted` | `DeviceRecord`s, when your plugin has devices that come from adoption |
| `device` | The stand-in, boxed. Any of the test kit's stand-ins, or one wrapped in `Rigged` |
| `ops` | `(local id, Op)` for each operation the plugin supports, in an order that works on the stand-in (a confirmation must be possible after each) |
| `setup` | The `SetupInput`s that drive your setup flow from its first step to `Done` against the stand-in. The flow is begun as the fixture's instance set up again (`existing` is `config`, so endpoints come from the fixture). It runs once with `secrets_present` empty, so it takes the path that asks for every secret, and, when `secrets` is filled, once more with those slots stored, as when the owner sets the integration up again |

`Fixture::empty()` fills the rest, for struct update syntax. The stand-ins start with `.await`, so a helper that builds the fixture is `async fn fixture() -> Fixture`, called as `run(&PLUGIN, fixture().await)`.

### What each check needs

The live checks give the driver `conformance::TIMEOUTS` (500 ms to connect, 500 ms per
reply), and a call against a silent device may take connect plus read plus 1.5 s.

| Check | What it does | What it needs from you |
| --- | --- | --- |
| 1 manifest | Grammar of ids, keys, slots, words; `version` is the crate's; `api` is `API_VERSION`; custom actions | A correct `MANIFEST` |
| 2 config | The fixture validates; a missing required key and an unknown key are refused with the key named; every declared key parses with a value of its kind; `migrate` leaves a current config alone | `deny_unknown_fields`, and `ConfigProblem`s that name the key |
| 3 purity | Puts traps on every loopback address in the config and adopted `attrs`, calls the four pure calls twice, and fails on any traffic, panic or different answer | Pure calls |
| 4 dry-run | Drives every catalog operation, at its edges, at every declared device, as a dry-run hub would | `readiness` is `Ready` for the fixture; device ids, hints and names are well formed; no panic, no empty refusal |
| 5 refusals agree | Live, `refusal` gives the dry-run answer and sends nothing; an operation it accepts is not answered `Disabled`; a device not in the directory is refused by the driver without sending | The driver applies the same rules as `refusal` |
| 6 exactly once | Each fixture operation is one command exchange at the stand-in; a silent stand-in is asked at most once | No retries. For HTTP, by default every request that is not `GET`, `HEAD` or `OPTIONS` counts as a command; `MockHttp::count_commands(rule)` says otherwise |
| 7 timeouts | Against a silent stand-in, `command`, `read_state`, `discover` and `health` end in time with `Timeout` or `Unreachable` | The helpers with `ctx.timeouts`; a stand-in that can go silent |
| 8 confirmation | After each fixture operation, a read satisfies `is_confirmed_by` (or the custom action's `Expect`) within five reads | A stand-in whose state changes as the device's would, and an honest `read_state` |
| 9 secrets | No secret value in an error, a `Debug` output, a state, a setup step, a `tracing` log line, or a URL query the stand-in saw | Secrets used only where they are sent |
| 10 device text | With hostile text (control characters, 10 kB) as every name the device reports, everything that comes back is clean and at most 256 characters | `net::device_text` on device text; a stand-in that reports text through `Switches::device_text`. It looks at states, discovery, errors and the setup steps: a string a step shows with the hostile name that it did not show with the normal one must be clean |
| 11 setup | The fixture's inputs reach `Done`, also when the fixture's secrets are already stored; the config validates; secrets only in `secrets`, in declared slots; no secret field prefilled, stored or not; with no grant, nothing contacted | The `setup` inputs |
| 12 robustness | With malformed, truncated and oversized replies, calls give errors, never a panic or a state | Defensive parsing; a stand-in that can spoil its replies |

A check that needs something the stand-in cannot do (go silent, spoil its replies, report a
name) is listed as **not run** rather than failed. To give a UDP, TCP or websocket stand-in
those switches, share a `Switches` with its handler and wrap it in `Rigged`:

```rust
use swifty_plugin_testkit::conformance::{Rigged, Switches};
use swifty_plugin_testkit::devices::MockUdp;

let switches = Switches::default();
let inside = switches.clone();
let device = MockUdp::start(move |request| {
    let name = inside.device_text("Blind 1"); // the suite's hostile text while check 10 runs
    Some(inside.garble(answer(request, &name))) // a spoiled reply while check 12 runs
})
.await;
let device = Rigged::new(device, switches);
```

`MockHttp` has its `switches` built in for faults; start it with `MockHttp::start_rigged`
to report text through them.

## When a check fails

Read the label first. In

```text
[check 5 refusals agree] the driver accepts {"op":"light_power","on":true} for `conformance-unknown-device`, which is not in its directory
```

the number and the name say which rule, and the rest says which call and which operation. The table above says what
the check wants. Two common ones:

- **Check 4 or 5 on a device you did not expect**: `refusal` must refuse a local id the
  instance does not have, and the driver must refuse it too, before sending.
- **Check 8**: the stand-in does not change its state the way the device would, or
  `read_state` reports something other than what the device said.

## Then the real device

The tests prove the plugin keeps the rules against a stand-in that behaves as you believe the
device does. Only the device can tell you whether that belief is right. Run it in a hub
([how-to.md](how-to.md#11-run-it-in-a-hub)), first in dry-run and then live, and write in
`README.md` what you tried it on.

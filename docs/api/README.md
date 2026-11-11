# The Swifty Home API

This is for anyone building a client of a Swifty Home hub: a desktop or mobile app,
a script, a dashboard. The web app the hub serves uses the same API and nothing else.

- **[openapi.v1.json](openapi.v1.json)** is the OpenAPI 3.1 document of API v1, every
  route, body and answer in it. A hub serves the same document at
  `GET /api/v1/openapi.json`, and `swifty openapi` prints it.
- **[untyped.md](untyped.md)** lists what the document still describes as a free
  JSON object.
- This page has the rules, how to sign in, and a worked example of the three ways an
  action runs.

## The rules

- **Everything is under `/api/v1/`.** Outside it there are only the web app (`GET /`,
  `GET /app/...`) and `GET /healthz`. There is no unversioned `/api/` path.
- **JSON in and out.** Times are milliseconds since the epoch. Ids are opaque strings:
  compare them, never parse them.
- **Requests are strict.** A body with a field the route does not know is refused
  with `422 invalid_body`, as is a missing or mistyped one.
- **Responses may grow.** Ignore fields you do not know. Within v1 nothing is removed
  or renamed; a field or route is only added. A change that would break a client is
  `/api/v2`, served beside v1.
- **Every response carries `X-Swifty-Api: 1`**, errors and the web app's files
  included, so a client can tell which API answered.
- **A deprecated field or route** is marked `deprecated` in the document and, for a
  route, answers with a `Deprecation` header (RFC 9745). It keeps working for at least
  two minor releases. Today the `/api/v1/lights...` routes are deprecated: use
  plugins, instances, discovery and `POST /api/v1/devices/adopt` instead.
- **Old action ids** (`bulb.power`, `sonos.volume`, ...) are accepted on input for the
  life of v1 and answered with the new id (`light.power`, `speaker.volume`). The
  catalog lists them as each action's `aliases`.
- **Errors** are always one envelope, with a stable `code` to act on and a `message`
  for a person:

  ```json
  { "error": { "code": "owner_only", "message": "Only the owner can do this." } }
  ```

  Some codes add `detail` (`setup_open` names the open setup, `device_exists` the
  target). The guards' answers are the same everywhere: `401` without a session
  (`not_signed_in`, or why it ended), `423 session_locked` for a locked session,
  `403 owner_only` for someone other than the owner, `403 bad_origin` for a request
  that changes something and does not come from the hub's own origin.
- **Who may call what** is in the document: an operation with no `security` is open
  to anyone; one with the `session` scheme needs a signed-in session; the scope
  `owner` means the owner only. A family member's request for what only the owner may
  do is refused and recorded in the audit log.

## Before signing in

`GET /api/v1/meta` is open to anyone and names nothing of the house:

```json
{
  "product": "Swifty Home",
  "version": "0.1.0",
  "api": { "major": 1, "minor": 0 },
  "plugin_api": 1,
  "live": true,
  "needs_owner": false,
  "auth": ["passkey"],
  "events": ["sse"]
}
```

`needs_owner` is true until the first passkey is enrolled. `live` is false in dry-run,
where no command reaches a device.

## Signing in

There are no passwords. A person signs in with a passkey, and the hub sets a session
cookie (`__Host-swifty-session`, `HttpOnly`, `Secure`, `SameSite=Strict`) that every
later request sends:

1. `POST /api/v1/auth/login/start` answers `{ ceremony_id, options }`. Hand `options`
   to `navigator.credentials.get` (or your platform's WebAuthn API).
2. `POST /api/v1/auth/login/finish` with `{ "ceremony_id": "...", "assertion": {...} }`
   answers `{ "session": {...} }` and sets the cookie.

Every request that changes something (anything but `GET`) must carry the hub's own
`Origin`, the one in its configuration. Requests from another origin are refused
with `403 bad_origin`, and there is no CORS: the API is for the hub's own origin.
Native apps are on the roadmap: [native apps](../roadmap/native-apps.md) describes the
pairing and bearer tokens planned for them.

Signing out is `POST /api/v1/auth/logout`; `POST /api/v1/session/lock` locks the
session until the next passkey sign-in.

## Running an action: three flows

Everything that changes a device, and much that changes the house (adding a person, a
passkey, an integration), is an **action** of the catalog. `GET /api/v1/catalog`
lists the actions this person may run, each with its `tier`, its `flow`, its
parameters and the targets they may use now:

```json
{
  "id": "light.power",
  "aliases": ["bulb.power"],
  "label": "Switch a light",
  "tier": 1,
  "tier_name": "comfort",
  "flow": "direct",
  "target_kind": "capable",
  "class": "light",
  "requires": "power",
  "targets": ["bulb1", "bulb2", "bulb3", "bulb4"],
  "params": [
    { "name": "on", "required": true, "help": "true for on, false for off.", "schema": { "type": "boolean" } }
  ],
  "description": "Turns one light on or off.",
  "consequences": [],
  "idempotent": true
}
```

The `flow` says how to run it. The hub decides the flow from the tier; a client
cannot choose another.

| `flow` | Tiers | How |
| --- | --- | --- |
| `direct` | 0 read, 1 comfort, `S` toward safety | `POST /actions/execute` |
| `prepare_confirm` | 2 consequential | `POST /actions/prepare`, show it, then `POST /actions/confirm` with its `confirmation` |
| `prepare_step_up` | 3 critical | `POST /actions/prepare`, show it, `POST /auth/step-up/start`, a passkey assertion, then `POST /actions/confirm` with the `assertion` |

Each run answers with the run: `200` when its result is final, `202` while the device
has not answered yet (`phase: "commanded"`). Follow a `202` with
`GET /api/v1/actions/runs/{run_id}` or the `run` event. A run's `phase` is
`confirmed` (the device's own reading shows it happened), `no_response`, `failed`,
`simulated` (dry-run: nothing was sent) or, for a TV key, `sent`.

### Direct: switch a light on

```http
POST /api/v1/actions/execute
Origin: https://home.example.org
Content-Type: application/json

{ "action": "light.power", "target": "bulb1", "params": { "on": true }, "request_id": "a1b2c3" }
```

```json
{
  "run_id": "6d99cd07c39657f16d1cc4fb",
  "action": "light.power",
  "target": "bulb1",
  "params": { "on": true },
  "channel": "panel",
  "step_up": "none",
  "phase": "confirmed",
  "simulated": false,
  "detail": null,
  "steps": [{ "target": "bulb1", "phase": "confirmed", "detail": null }],
  "started_at": 1900000000000,
  "finished_at": 1900000000000,
  "rtt_ms": 0,
  "duplicate": false
}
```

`request_id` is the client's own: send the same one again (after a dropped
connection, say) and the hub answers with the same run instead of running it twice
(`duplicate: true`). An action of tier 2 or 3 sent here is refused with
`403 confirmation_required` or `403 step_up_required`, and nothing is sent.

### Confirm: pause a print

First prepare. Nothing runs yet:

```http
POST /api/v1/actions/prepare

{ "action": "print.pause", "target": "printer", "params": {} }
```

```json
{
  "prepared_id": "22ab51fa92f7a996559d01ce",
  "action": "print.pause",
  "target": "printer",
  "params": {},
  "tier": 2,
  "tier_name": "consequential",
  "label": "Pause print",
  "target_name": "3D printer",
  "what": "Pauses the current print.",
  "consequences": ["Heaters stay on."],
  "affected": [],
  "warning": null,
  "channel": "panel",
  "needs": "tap",
  "confirmation": "64djfaxGgrLaZ2cbPNN8j7E2Gdb1TBoGAhoypuFWdlc",
  "expires_at": 1900000030000
}
```

Show the person `label`, `what`, `consequences` and `warning`, and a button. When they
press it, send the same action back with the `confirmation`:

```http
POST /api/v1/actions/confirm

{
  "prepared_id": "22ab51fa92f7a996559d01ce",
  "action": "print.pause",
  "target": "printer",
  "params": {},
  "confirmation": "64djfaxGgrLaZ2cbPNN8j7E2Gdb1TBoGAhoypuFWdlc"
}
```

The answer is the run, with `"step_up": "confirm"`. A preparation is used once
(`409 already_used`), belongs to the session that made it (`403 wrong_session`) and
expires (`410 expired`). If the action, target or params sent back differ from what
was prepared, the hub sends nothing, cancels the preparation and answers
`409 binding_mismatch`.

### Step-up: stop a VM

A critical action needs a fresh passkey assertion that is bound to that one action.
Prepare it as before:

```http
POST /api/v1/actions/prepare

{ "action": "vm.stop", "target": "vm100", "params": {} }
```

```json
{
  "prepared_id": "b40bb02e1f294cae03ce527b",
  "action": "vm.stop",
  "target": "vm100",
  "params": {},
  "tier": 3,
  "tier_name": "critical",
  "label": "Stop alpha · VM 100",
  "target_name": "alpha · VM 100",
  "what": "Its host asks the VM to shut down cleanly.",
  "consequences": ["alpha · VM 100 shuts down.", "The shared folders go offline while this is down."],
  "affected": ["alpha · VM 100"],
  "warning": null,
  "channel": "panel",
  "needs": "passkey",
  "confirmation": null,
  "expires_at": 1900000120000
}
```

When the person approves, start the step-up with the prepared action, as prepared:

```http
POST /api/v1/auth/step-up/start

{ "prepared_id": "b40bb02e1f294cae03ce527b", "action": "vm.stop", "target": "vm100", "params": {} }
```

```json
{
  "prepared_id": "b40bb02e1f294cae03ce527b",
  "options": {
    "publicKey": {
      "challenge": "k327GwmOR9HdrEATPOQYX-wf4SaGYV_mI5LtUgM0Adg",
      "rpId": "home.example.org",
      "allowCredentials": [{ "id": "p7gRCNl_uu4fDTKhYLFtPQNJ3ZJ1vDWX7dyp0L_Jymo", "type": "public-key" }],
      "timeout": 60000,
      "userVerification": "required"
    }
  },
  "expires_at": 1900000060000
}
```

The challenge is derived from the action, its target, its params and the session, so
the assertion approves this action and nothing else. Hand `options` to
`navigator.credentials.get`, then confirm with the assertion it gives back:

```http
POST /api/v1/actions/confirm

{
  "prepared_id": "b40bb02e1f294cae03ce527b",
  "action": "vm.stop",
  "target": "vm100",
  "params": {},
  "assertion": {
    "id": "p7gRCNl_uu4fDTKhYLFtPQNJ3ZJ1vDWX7dyp0L_Jymo",
    "rawId": "p7gRCNl_uu4fDTKhYLFtPQNJ3ZJ1vDWX7dyp0L_Jymo",
    "type": "public-key",
    "response": { "authenticatorData": "...", "clientDataJSON": "...", "signature": "...", "userHandle": null },
    "extensions": {}
  }
}
```

```json
{
  "run_id": "faa25bf388f3eaf7e32cf466",
  "action": "vm.stop",
  "target": "vm100",
  "params": {},
  "channel": "panel",
  "step_up": "passkey",
  "phase": "confirmed",
  "simulated": false,
  "detail": null,
  "steps": [{ "target": "vm100", "phase": "confirmed", "detail": null }],
  "started_at": 1900000000000,
  "finished_at": 1900000000000,
  "rtt_ms": 0,
  "duplicate": false
}
```

Without an assertion the confirm is refused with `428 step_up_required`. Some
critical actions carry more in their run: `passkey.register` its `registration`
ceremony, `user.invite` the `invitation` with its token (shown once), `console.open`
its `console` ticket, and `plugin.add` and `plugin.reconfigure` the `setup` they
opened, whose next steps are `GET` and `POST /api/v1/setup/{id}`.

Every attempt, refused or not, is in the audit log (`GET /api/v1/audit`, owner only),
with who, from which session and passkey, and how it ended.

## Energy and its cost

A telemetry node counts the energy its machine has used. The owner sets the house's
electricity tariff, and the hub keeps a ledger for each telemetry device. Both routes
are for the owner only.

`GET /api/v1/energy` answers the tariff and each device's counter and cost:

```json
{
  "tariff": {
    "currency": "EUR",
    "day_price_per_kwh": 0.31,
    "night_price_per_kwh": 0.15,
    "night": { "start": "22:00", "end": "06:00" },
    "summer_night": null,
    "time_zone": "Europe/Berlin",
    "set_at": 1900000000000,
    "band_now": "day"
  },
  "targets": [
    {
      "target": "collector",
      "kwh": 12.3456,
      "since": 1794380000000,
      "cost": 3.7078,
      "cost_day": 3.596,
      "cost_night": 0.1118,
      "kwh_day": 11.6,
      "kwh_night": 0.7456,
      "priced_since": 1900000000000,
      "backfilled_kwh": 11.4,
      "updated_at": 1900000060000
    }
  ]
}
```

`tariff` is `null` until a tariff is set, and then every `cost`, `cost_day`, `cost_night`,
`kwh_day`, `kwh_night`, `priced_since` and `backfilled_kwh` is `null` too: kWh show, cost
does not. A one-rate tariff has `null` for `night_price_per_kwh`, `night`, `summer_night`
and `time_zone`. `band_now` says which price applies at this moment. `kwh` is the node's
counter and `since` is when it began. `cost` is unrounded, in the tariff's currency; round
it for display. Each reading's energy is spread over the minutes since the reading before,
and each minute is charged at the price for that local minute, so `cost_day` and
`cost_night` add up to `cost`. It covers the whole counter: the `backfilled_kwh` that had
been counted before the first tariff were charged once at its day price, and a later change
never reaches back. A counter that restarts (a new `since`, or a total that went down) is
followed by its new total.

`PUT /api/v1/energy/tariff` sets the tariff and answers the same document:

```http
PUT /api/v1/energy/tariff
Origin: https://home.example.org
Content-Type: application/json

{
  "currency": "EUR",
  "day_price_per_kwh": 0.31,
  "night_price_per_kwh": 0.15,
  "night": { "start": "22:00", "end": "06:00" },
  "summer_night": { "start": "23:00", "end": "07:00" },
  "time_zone": "Europe/Berlin"
}
```

For one price all day, send only `currency` and `day_price_per_kwh`. Prices are more than 0
and at most 1000000; a night price needs `night` and `time_zone`, `summer_night` needs
`night`, and night must start and end at different times (`422 invalid_tariff`). Times are
`HH:MM` on a 24-hour clock; `night` may run across midnight. `summer_night` is used while
daylight saving time is in effect in `time_zone`, an IANA name such as `Europe/Berlin`. The
currency is an ISO 4217 code of three letters, accepted in any case and kept in capitals
(`422 invalid_currency`). A tariff in a new currency charges what has been counted once
more, in that currency. Each change is audited as `energy.tariff` with the prices, hours,
time zone and currency.

## Following changes

`GET /api/v1/events` is a Server-Sent Events stream for the signed-in session. Each
event has an `id`, a name and JSON data:

```text
id: 1042
event: run
data: {"run_id":"6d99cd07c39657f16d1cc4fb","action":"light.power","target":"bulb1","phase":"confirmed"}
```

| Event | Data | Sent when, and to whom |
| --- | --- | --- |
| `run` | `run_id`, `action`, `target`, `phase` | a run changes phase: the person who started it, and the owner |
| `state` | `target`, `state` | a read of a device differs from the last: anyone who may see it |
| `devices` | `{}` | the device index was rebuilt, or a scene or a device's notes changed: everyone; fetch again |
| `plugin` | `instance`, `readiness`, `health` | an integration's readiness or health changed: the owner |
| `setup` | `setup_id` | a waiting setup step moved on: the session that owns the setup |
| `assistant` | `job_id` | a Swifty job has news: its person |
| `resync` | `{}` | you missed events: fetch everything again |

Send `Last-Event-ID` when you reconnect; the hub keeps the last 1024 events. A comment
line every 20 seconds keeps the connection open. The stream ends when the session
ends or is locked. A session may hold four streams. The events only make a client
refresh sooner: the routes stay the source of truth.

## Keeping the document current

The document is made from the router: every route is registered together with its
entry (`utoipa-axum`), so a route cannot exist without an entry or the other way
round. In the repository:

```sh
cargo xtask openapi           # writes docs/api/openapi.v1.json
cargo xtask openapi --check   # fails when the committed file differs (CI runs it)
```

The tests compare the router's routes with the document's paths in both directions,
check who may call each operation against its `security`, and check the hub's
answers against the schemas, so a typed answer cannot drift from what is written
here.

# What the document does not type yet

Every route of API v1 is in [the OpenAPI document](openapi.v1.json), and the
resources an app needs are typed there field by field: sessions, people, devices and
their state, the catalog, prepared actions and runs, audit rows, rooms and scenes,
plugins and instances, setup steps, events and errors. A test checks the hub's
answers against those schemas, so a typed response cannot drift from its document.

What follows is still documented as a free JSON object (or a free field inside a
typed one), with its description and, where it helps, an example. These are not
promises of shape: read them defensively, and ignore what you do not know, as with
every v1 response. Typing one later only adds to v1; it removes nothing.

## Answers

| Route | What it is |
| --- | --- |
| `GET /api/v1/plugins/{plugin}` | A plugin's manifest in full, as `docs/plugins/reference/manifest.md` describes it, plus `tier_effective` on each custom action, `reach_text`, `setup`, `instances`, `can_add`, `why_not` (as in the list's `PluginSummary`) and `dropped_words: [{word, why}]`. |
| `GET /api/v1/health` | The health page's report: every reading is a value from a source that is reporting, or `not_reporting`. |
| `POST /api/v1/assistant` | Swifty's answer: a reply, an action to run or confirm, or a job its agent works on. |
| `GET /api/v1/assistant/jobs/{id}` | What a job of the agent has written so far, with its version. |
| `POST /api/v1/assistant/jobs/{id}/stop` | The stopped job. |
| `POST /api/v1/assistant/jobs/{id}/answer` | The job after the person's answer to its question. |
| `POST /api/v1/assistant/resume` | The answer still being written in a chat. |
| `POST /api/v1/voice` | The words, or the result of acting on them. |
| `GET /api/v1/openapi.json` | The OpenAPI document itself. |

## Fields

| Field | What it holds |
| --- | --- |
| `params` of `ActionRequest`, `StepUpRequest`, `ConfirmRequest`, `PreparedView`, `RunView`, `Learned` and audit `Row` | An action's parameters: an object whose keys and kinds the action's catalog entry (`params`) gives. In an audit row, what the action recorded. |
| `RunView.state` | For `state.read`: each target's reading (as `DeviceReading`), by target, and `metric` when one was asked for. |
| `RunView.console` | For `console.open`: how to reach the console just opened, with its single-use ticket. Never stored. |
| `RunView.invitation` | For `user.invite`: the invitation, with its token. Shown once, never stored. |
| `RunView.pairing` | For an agent pairing: what the collector said. Never stored. |
| `ErrorDetail.detail` | More about an error, for the codes that have more to say. |
| `FoundDevice.attrs`, `DeviceDecl.attrs` | What a plugin needs to find a device again (its address, its MAC). Never a secret. Its keys are the plugin's own. |
| `Instance.config`, `SetupResult.config` | An instance's config. Its keys are the plugin's own and are listed in its manifest's `config`. Never a secret. |
| `Scene.extras` | What a scene does besides the lights. Only Movie has any: `{"tv_input": "HDMI 1", "volume_cap": 30}`. |
| `History.series`, `History.points` | Chart data: the stored series by metric, or a host's or VM's points, oldest first. |
| `data` of a `State` of kind `telemetry` | A telemetry source's readings, as the source sent them. |
| WebAuthn `options`, `credential` and `assertion` | The WebAuthn objects, as `navigator.credentials.create` and `get` take and give them (with binary fields as base64url text). They are the WebAuthn standard's, not Swifty's. |

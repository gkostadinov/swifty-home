//! Setup sessions: the steps of a plugin's guided setup.
//!
//! A setup is opened by `plugin.add` or `plugin.reconfigure`, which answer with its
//! id and first step. From then on only the owner, in the session that opened it,
//! can see it, send it an input or abandon it:
//!
//! - `GET /setup/{id}`: the step as it stands (after a `setup` event, for instance);
//! - `POST /setup/{id}` with `{"input": "values", "values": {...}}`,
//!   `{"input": "chosen", "ids": [...]}`, `{"input": "poll"}` or `{"input": "back"}`:
//!   the next step;
//! - `DELETE /setup/{id}`: abandons it. Nothing was stored before `done`, so an
//!   abandoned setup leaves nothing behind.
//!
//! Every answer is `{ setup_id, plugin, plugin_name, instance, purpose, step, retry,
//! finished, expires_at }`. `step` is a step as the plugin API serialises it,
//! except that `done` carries its summary only. With a `failed` step that can be
//! tried again, `retry` is the step to show again. A request body is never logged.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use serde_json::Value;
use swifty_plugin_api::{SetupInput, SetupStep};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::{Body, Owner};
use super::{actor, RouterState};
use crate::app::App;
use crate::error::ApiError;
use crate::setup::{step_json, Purpose, SetupView};

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().routes(routes!(setup_get, setup_post, setup_delete))
}

/// A setup as its owner sees it.
#[derive(Debug, Serialize, ToSchema)]
pub struct Setup {
    pub setup_id: String,
    /// The plugin's id.
    pub plugin: String,
    /// The plugin's name.
    pub plugin_name: String,
    /// The instance it adds or sets up again.
    pub instance: String,
    pub purpose: Purpose,
    /// The step to show.
    #[schema(value_type = ClientStep)]
    pub step: Value,
    /// With a `failed` step that can be tried again: the step to show again, to which
    /// the same kind of input is sent. Null otherwise.
    #[schema(value_type = Option<ClientStep>)]
    pub retry: Option<Value>,
    /// Done, failed for good, or abandoned: no input is taken any more.
    pub finished: bool,
    /// When it is forgotten unless it is touched again: 15 minutes after the last touch.
    pub expires_at: i64,
}

impl From<&SetupView> for Setup {
    fn from(view: &SetupView) -> Self {
        Self {
            setup_id: view.setup_id.clone(),
            plugin: view.plugin.clone(),
            plugin_name: view.plugin_name.clone(),
            instance: view.instance.clone(),
            purpose: view.purpose,
            step: step_json(&view.step),
            retry: view.retry.as_ref().map(step_json),
            finished: view.finished,
            expires_at: view.expires_at,
        }
    }
}

/// A step as a client is sent it: a step of the plugin's flow, except that `done`
/// carries its summary only.
#[derive(Serialize, ToSchema)]
#[serde(untagged)]
pub enum ClientStep {
    /// `form`, `wait`, `choose`, `external` or `failed`.
    Step(SetupStep),
    /// `done`.
    Done(StepDone),
}

/// The last step of a setup that worked: what was stored, in one line.
#[derive(Serialize, ToSchema)]
pub struct StepDone {
    /// Always `done`.
    pub step: &'static str,
    /// One line for the person: "Paired. 6 lights added."
    pub summary: String,
}

/// Whether the setup was still open when it was abandoned.
#[derive(Serialize, ToSchema)]
pub struct Abandoned {
    pub abandoned: bool,
}

/// The step a setup stands at.
///
/// After a `setup` event, say. Only in the session that opened it: another session
/// gets `403 wrong_session`, an unknown or forgotten id `404 unknown_setup`.
#[utoipa::path(
    get,
    path = "/setup/{id}",
    tag = "setup",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The setup's id, from the run of `plugin.add` or `plugin.reconfigure`.")),
    responses((status = 200, body = Setup))
)]
async fn setup_get(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
) -> Result<Json<Setup>, ApiError> {
    Ok(Json(Setup::from(&app.setup.view(&app, &actor(&session), &id)?)))
}

/// Answers the step a setup stands at, and gets the next one.
///
/// Send exactly one of `{"input": "values", "values": {...}}` (a `form` or
/// `external` step), `{"input": "chosen", "ids": [...]}` (a `choose` step),
/// `{"input": "poll"}` (ask a `wait` step now) or `{"input": "back"}`. Anything else
/// is `422 invalid_input`; each value is checked against the step's field before the
/// plugin sees it. Input after the end is `409 setup_finished`.
#[utoipa::path(
    post,
    path = "/setup/{id}",
    tag = "setup",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The setup's id.")),
    request_body = SetupInput,
    responses((status = 200, body = Setup))
)]
async fn setup_post(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
    Body(body): Body<Value>,
) -> Result<Json<Setup>, ApiError> {
    let input = parse_input(body)?;
    Ok(Json(Setup::from(&app.setup.advance(&app, &actor(&session), &id, input).await?)))
}

/// Abandons a setup. Nothing was stored before `done`, so it leaves nothing behind.
#[utoipa::path(
    delete,
    path = "/setup/{id}",
    tag = "setup",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The setup's id.")),
    responses((status = 200, body = Abandoned))
)]
async fn setup_delete(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
) -> Result<Json<Abandoned>, ApiError> {
    let abandoned = app.setup.abandon(&app, &actor(&session), &id).await?;
    Ok(Json(Abandoned { abandoned }))
}

fn bad_input() -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_input",
        "Send {\"input\": \"values\", \"values\": {...}}, {\"input\": \"chosen\", \"ids\": [...]}, \
         {\"input\": \"poll\"} or {\"input\": \"back\"}.",
    )
}

/// The input, refused with anything it does not know: requests are strict.
pub(crate) fn parse_input(body: Value) -> Result<SetupInput, ApiError> {
    let Value::Object(map) = &body else {
        return Err(bad_input());
    };
    let allowed: &[&str] = match map.get("input").and_then(Value::as_str) {
        Some("values") => &["input", "values"],
        Some("chosen") => &["input", "ids"],
        Some("poll" | "back") => &["input"],
        _ => return Err(bad_input()),
    };
    if map.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(bad_input());
    }
    serde_json::from_value(body).map_err(|_| bad_input())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use swifty_plugin_api::{Choice, SetupResult};

    use super::*;

    /// The typed answer is the JSON the setup engine has always made of a view.
    #[test]
    fn a_setup_answer_is_what_the_engine_makes_of_its_view() {
        let wait =
            SetupStep::Wait { title: "Press".into(), text: "Press the button.".into(), poll_ms: 1000, timeout_s: 30 };
        let choose = SetupStep::Choose {
            title: "Lights".into(),
            text: "Pick them.".into(),
            options: vec![Choice { id: "hue-1".into(), label: "Lamp".into(), detail: "".into(), selected: true }],
            multiple: true,
            min: 0,
        };
        let done = SetupStep::Done(SetupResult {
            config: json!({ "url": "http://192.0.2.10" }),
            secrets: Vec::new(),
            adopt: Vec::new(),
            summary: "Paired.".into(),
        });
        let failed = SetupStep::Failed { reason: "Nothing happened in time.".into(), retry: true };
        for (step, retry, purpose) in [
            (wait.clone(), None, Purpose::Add),
            (choose, None, Purpose::Reconfigure),
            (done, None, Purpose::Add),
            (failed, Some(wait), Purpose::Add),
        ] {
            let view = SetupView {
                setup_id: "s1".into(),
                plugin: "hue".into(),
                plugin_name: "Philips Hue".into(),
                instance: "hue2".into(),
                purpose,
                step,
                retry,
                finished: false,
                expires_at: 1_900_000_000_000,
            };
            assert_eq!(serde_json::to_value(Setup::from(&view)).unwrap(), view.to_json());
        }
    }

    #[test]
    fn an_input_is_one_of_four_shapes_and_nothing_more() {
        for good in [
            json!({ "input": "values", "values": { "url": "http://192.0.2.10", "port": 80, "on": true } }),
            json!({ "input": "chosen", "ids": ["hue-1"] }),
            json!({ "input": "poll" }),
            json!({ "input": "back" }),
        ] {
            assert!(parse_input(good.clone()).is_ok(), "{good}");
        }
        for bad in [
            json!([]),
            json!({}),
            json!({ "input": "skip" }),
            json!({ "input": "poll", "extra": 1 }),
            json!({ "input": "values" }),
            json!({ "input": "values", "values": { "nested": { "a": 1 } } }),
            json!({ "input": "chosen", "ids": "hue-1" }),
            json!({ "input": "values", "values": {}, "ids": [] }),
        ] {
            assert_eq!(parse_input(bad.clone()).unwrap_err().code, "invalid_input", "{bad}");
        }
    }
}

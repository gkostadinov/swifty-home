//! The OpenAPI document of API v1, and `GET /api/v1/openapi.json`.
//!
//! The document is made from the router itself. Every route of the API is
//! registered with `utoipa_axum`'s `routes!`, which adds the handler to the router
//! and its `#[utoipa::path]` entry to the document in one call, so a route cannot
//! exist without an entry, nor an entry without a route. [`document`] takes what
//! that collected and adds what is the same for every operation, from what each
//! one declares:
//!
//! - every response carries `X-Swifty-Api` ([`super::API_HEADER`]);
//! - every operation answers errors in the one envelope, [`ErrorEnvelope`], and the
//!   statuses the guards give are listed where they apply: `401` where a session
//!   is needed, `403 owner_only` where the owner is (the scope `owner` on the
//!   `session` scheme), `403 bad_origin` for anything that is not a read, `422
//!   invalid_body` where there is a JSON body;
//! - the routes in [`DEPRECATED`] are marked deprecated, and [`deprecation`] sends
//!   their `Deprecation` header.
//!
//! `swifty openapi` prints [`document_json`]; `docs/api/openapi.v1.json` is that
//! text, committed, and `cargo xtask openapi --check` fails when they differ.

use std::sync::OnceLock;

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use serde::Serialize;
use serde_json::{json, Map, Value};
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::{RouterState, API_MAJOR, API_MINOR, API_PREFIX};
use crate::sessions::COOKIE_NAME;

/// The routes that are kept for v1 but should not be used by a new client, below
/// [`API_PREFIX`]. Each is marked deprecated in the document, and answers with a
/// [`DEPRECATION`] header. They are kept for the life of v1.
pub const DEPRECATED: &[&str] = &["/lights", "/lights/discover", "/lights/hue/pair", "/lights/{target}/address"];

/// The `Deprecation` header of a deprecated route (RFC 9745): the date it was
/// deprecated, 10 November 2026, as seconds since the epoch.
pub const DEPRECATION: &str = "@1794268800";

/// Adds the `Deprecation` header to a response of a deprecated route.
pub(crate) async fn deprecation(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert("deprecation", HeaderValue::from_static(DEPRECATION));
    response
}

/// Every error, whatever the route: a code a client can act on, a sentence for a
/// person, and sometimes more about it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(example = json!({ "error": { "code": "owner_only", "message": "Only the owner can do this." } }))]
pub struct ErrorEnvelope {
    pub error: ErrorDetail,
}

/// What went wrong.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable within v1, such as `not_signed_in`, `owner_only`, `invalid_body` or
    /// `unknown_device`. A client acts on this, never on the message.
    pub code: String,
    /// One sentence for the person. It may change.
    pub message: String,
    /// More about it, when a code has more to say (`setup_open` names the setup,
    /// `instance_exists` the instance, `device_exists` the target).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

/// What the document says before the routes are added.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Swifty Home API",
        version = "1",
        description = "API v1 of a Swifty Home hub. Everything is under `/api/v1`; JSON in and out; times are \
milliseconds since the epoch; ids are opaque strings. Requests are strict (an unknown field is refused), \
responses may grow (ignore what you do not know). Every response carries `X-Swifty-Api: 1`. Signing in is \
with a passkey, which sets the session cookie. `docs/api/README.md` in the repository explains the rules and \
walks through the three ways an action runs: direct, confirm, and step-up.",
    ),
    tags(
        (name = "meta", description = "What the hub is and this document. Open to anyone."),
        (name = "auth", description = "Enrolling, joining from an invitation, signing in and out, and step-up."),
        (name = "sessions", description = "The current session and the active ones."),
        (name = "people", description = "Each person's favourites and pinned rooms, the people, and invitations."),
        (name = "passkeys", description = "The passkeys. Owner only."),
        (name = "actions", description = "The catalog, running actions in their three flows, and the runs."),
        (name = "audit", description = "The audit log and its check. Owner only."),
        (name = "devices", description = "Devices, their state, names and notes, and generic discovery and adoption."),
        (name = "lights", description = "Lights by their older routes. Deprecated: use the plugins, instances and devices routes."),
        (name = "rooms", description = "Rooms, their devices, order and scenes."),
        (name = "scenes", description = "The house scenes."),
        (name = "machines", description = "Machines and hosts, their charts and health, and the console. Owner only."),
        (name = "energy", description = "The electricity price, and the energy and cost of each telemetry node. Owner only."),
        (name = "memory", description = "What Swifty remembers and has learned."),
        (name = "assistant", description = "The chat with Swifty, its jobs, and voice."),
        (name = "plugins", description = "The compiled-in plugins and their instances. Owner only."),
        (name = "setup", description = "The steps of a plugin's guided setup. Owner only, in the session that opened it."),
        (name = "events", description = "The event stream, as Server-Sent Events."),
    ),
    components(schemas(
        ErrorEnvelope,
        swifty_plugin_api::Op,
        swifty_plugin_api::State,
        swifty_plugin_api::Capability,
        swifty_plugin_api::SetupStep,
        swifty_plugin_api::Health,
        swifty_plugin_api::Readiness,
        super::events::RunEvent,
        super::events::StateEvent,
        super::events::DevicesEvent,
        super::events::PluginEvent,
        super::events::SetupEvent,
        super::events::AssistantEvent,
    ))
)]
struct Base;

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().routes(routes!(openapi_json))
}

/// This document.
///
/// The same text `swifty openapi` prints. Open to anyone.
#[utoipa::path(
    get,
    path = "/openapi.json",
    tag = "meta",
    responses((status = 200, description = "The OpenAPI 3.1 document of API v1.", body = Object))
)]
async fn openapi_json() -> Json<Value> {
    Json(document().clone())
}

/// The OpenAPI document of API v1, made once from the router.
pub fn document() -> &'static Value {
    static DOCUMENT: OnceLock<Value> = OnceLock::new();
    DOCUMENT.get_or_init(|| {
        let mut base = Base::openapi();
        base.merge(super::api().into_openapi());
        let mut document = serde_json::to_value(base).expect("the document serialises");
        finish(&mut document);
        document
    })
}

/// [`document`] as `swifty openapi` prints it and `docs/api/openapi.v1.json` holds it.
pub fn document_json() -> String {
    let mut text = serde_json::to_string_pretty(document()).expect("the document serialises");
    text.push('\n');
    text
}

const METHODS: [&str; 5] = ["get", "put", "post", "delete", "patch"];

/// What every operation has in common, added to each from what it declares.
fn finish(document: &mut Value) {
    document["info"]["version"] = json!(format!("{API_MAJOR}.{API_MINOR}"));
    document["info"]["license"] = json!({ "name": "Apache-2.0", "identifier": "Apache-2.0" });
    document["components"]["securitySchemes"] = json!({
        "session": {
            "type": "apiKey",
            "in": "cookie",
            "name": COOKIE_NAME,
            "description": "The session a passkey sign-in sets (`POST /api/v1/auth/login/finish`). An operation \
                            that asks for the scope `owner` is refused to anyone else with `403 owner_only`.",
        }
    });
    document["components"]["headers"] = json!({
        "X-Swifty-Api": {
            "description": "The API's major version. Every response carries it, errors included.",
            "schema": { "type": "string", "enum": [API_MAJOR.to_string()] },
        },
        "Deprecation": {
            "description": "On a deprecated route only: when it was deprecated (RFC 9745).",
            "schema": { "type": "string", "enum": [DEPRECATION] },
        },
    });
    let shared = shared_responses();
    document["components"]["responses"] = Value::Object(shared.clone());
    let Some(paths) = document["paths"].as_object_mut() else {
        return;
    };
    for (path, item) in paths.iter_mut() {
        let deprecated = path.strip_prefix(API_PREFIX).is_some_and(|below| DEPRECATED.contains(&below));
        for method in METHODS {
            if let Some(operation) = item.get_mut(method) {
                finish_operation(operation, method, deprecated, &shared);
            }
        }
    }
}

/// The error answers many operations share, each with the `X-Swifty-Api` header.
fn shared_responses() -> Map<String, Value> {
    let error = |description: &str, code: &str, message: &str| {
        json!({
            "description": description,
            "headers": { API_HEADER_NAME: { "$ref": "#/components/headers/X-Swifty-Api" } },
            "content": {
                "application/json": {
                    "schema": { "$ref": "#/components/schemas/ErrorEnvelope" },
                    "example": { "error": { "code": code, "message": message } },
                }
            },
        })
    };
    const OWNER: &str = "`owner_only`: only the owner can do this. The refusal is audited.";
    const ORIGIN: &str = "`bad_origin`: the request did not come from the hub's own origin.";
    let mut shared = Map::new();
    shared.insert(
        "NotSignedIn".into(),
        error(
            "Not signed in: `not_signed_in`, or why the session ended. A locked session gets `423 session_locked`.",
            "not_signed_in",
            "Sign in with a passkey.",
        ),
    );
    shared.insert("OwnerOnly".into(), error(OWNER, "owner_only", "Only the owner can do this."));
    shared.insert("WrongOrigin".into(), error(ORIGIN, "bad_origin", "Wrong origin."));
    shared.insert(
        "OwnerOnlyOrWrongOrigin".into(),
        error(&format!("{OWNER} {ORIGIN}"), "owner_only", "Only the owner can do this."),
    );
    shared.insert(
        "InvalidBody".into(),
        error(
            "The body was refused: `invalid_body` when it could not be read (an unknown field, a missing one, a wrong              type), or the route's own code when a value is not allowed.",
            "invalid_body",
            "The request body could not be read.",
        ),
    );
    shared.insert("Error".into(), error("Any other error, in the same envelope.", "not_found", "Not found."));
    shared
}

fn finish_operation(operation: &mut Value, method: &str, deprecated: bool, shared: &Map<String, Value>) {
    let scopes: Option<Vec<String>> = operation["security"].as_array().map(|requirements| {
        requirements
            .iter()
            .filter_map(|requirement| requirement.get("session").and_then(Value::as_array))
            .flatten()
            .filter_map(|scope| scope.as_str().map(str::to_string))
            .collect()
    });
    let has_body = operation.get("requestBody").is_some_and(|body| body["content"].get("application/json").is_some());
    let owner = scopes.as_ref().is_some_and(|scopes| scopes.iter().any(|scope| scope == "owner"));
    let write = method != "get";
    let mut added = Vec::new();
    if scopes.is_some() {
        added.push(("401", "NotSignedIn"));
    }
    match (owner, write) {
        (true, true) => added.push(("403", "OwnerOnlyOrWrongOrigin")),
        (true, false) => added.push(("403", "OwnerOnly")),
        (false, true) => added.push(("403", "WrongOrigin")),
        (false, false) => {}
    }
    if has_body {
        added.push(("422", "InvalidBody"));
    }
    added.push(("default", "Error"));
    let responses = operation["responses"].as_object_mut().expect("every operation has responses");
    for response in responses.values_mut() {
        let headers = response.as_object_mut().expect("a response is an object").entry("headers").or_insert(json!({}));
        headers[API_HEADER_NAME] = json!({ "$ref": "#/components/headers/X-Swifty-Api" });
        if deprecated {
            headers["Deprecation"] = json!({ "$ref": "#/components/headers/Deprecation" });
        }
    }
    for (status, name) in added {
        if responses.contains_key(status) {
            continue;
        }
        // A deprecated route's answers carry its header too, so they are written out.
        let response = if deprecated {
            let mut response = shared[name].clone();
            response["headers"]["Deprecation"] = json!({ "$ref": "#/components/headers/Deprecation" });
            response
        } else {
            json!({ "$ref": format!("#/components/responses/{name}") })
        };
        responses.insert(status.to_string(), response);
    }
    if deprecated {
        operation["deprecated"] = json!(true);
    }
}

/// [`super::API_HEADER`] as the document writes it.
const API_HEADER_NAME: &str = "X-Swifty-Api";

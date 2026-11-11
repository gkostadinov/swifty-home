//! The OpenAPI document cannot drift from the hub:
//!
//! - the router's paths and the document's are the same set, both ways, and each
//!   path answers exactly the methods the document gives it;
//! - who may call each operation is what the document says;
//! - the responses of the typed operations match their schemas, in every field;
//! - the deprecated routes are marked and send their header;
//! - `docs/api/openapi.v1.json` is the document the router makes, and the hub
//!   serves the same at `GET /api/v1/openapi.json`.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use axum::http::{Method, StatusCode};
use axum::Router;
use common::*;
use serde_json::{json, Value};
use swifty_hub::http::openapi::{document, document_json, DEPRECATED, DEPRECATION};
use swifty_hub::http::API_PREFIX;

const METHODS: [Method; 5] = [Method::GET, Method::PUT, Method::POST, Method::DELETE, Method::PATCH];

/// What is served outside the API: the page, its files, and the health check.
const PAGE_PATHS: [&str; 3] = ["/", "/app/{*path}", "/healthz"];

fn method_key(method: &Method) -> String {
    method.as_str().to_ascii_lowercase()
}

/// Every operation of the document: path, then its methods.
fn operations() -> BTreeMap<String, BTreeSet<String>> {
    let paths = document()["paths"].as_object().expect("the document has paths");
    paths
        .iter()
        .map(|(path, item)| {
            let methods = METHODS.iter().map(method_key).filter(|method| item.get(method).is_some()).collect();
            (path.clone(), methods)
        })
        .collect()
}

fn operation(path: &str, method: &Method) -> &'static Value {
    &document()["paths"][path][method_key(method)]
}

/// A response of an operation, or the shared one it refers to.
fn response(response: &'static Value) -> &'static Value {
    match response.get("$ref").and_then(Value::as_str) {
        Some(reference) => {
            let name = reference.strip_prefix("#/components/responses/").expect("a response reference");
            &document()["components"]["responses"][name]
        }
        None => response,
    }
}

/// The paths the router itself has, as its own table lists them (its `Debug`
/// output: `RouteId(n): "/path"` in the path router's node).
fn router_paths(router: &Router) -> BTreeSet<String> {
    let text = format!("{router:?}");
    let routes = text.split("fallback_router").next().unwrap();
    let paths: BTreeSet<String> = routes
        .split("RouteId(")
        .skip(1)
        .filter_map(|piece| piece.split_once("): \"").map(|(_, rest)| rest.split('"').next().unwrap().to_string()))
        .collect();
    assert!(paths.len() > 50, "the router's table could not be read: {paths:?}");
    paths
}

/// A path of the document with a value for each parameter. A device target is a
/// light the family may see, so a family member's request is about something they
/// can see.
fn concrete(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('}').unwrap() + start;
        out.push_str(match &rest[start + 1..end] {
            "target" => "bulb1",
            "plugin" => "mock",
            _ => "x1",
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[test]
fn the_router_and_the_document_name_the_same_paths() {
    let harness = Harness::new();
    let routed = router_paths(&harness.router);
    let documented: BTreeSet<String> = operations().into_keys().collect();
    let api: BTreeSet<String> = routed.iter().filter(|path| path.starts_with(API_PREFIX)).cloned().collect();
    let outside: BTreeSet<&str> =
        routed.iter().filter(|path| !path.starts_with(API_PREFIX)).map(String::as_str).collect();

    let undocumented: Vec<&String> = api.difference(&documented).collect();
    let unrouted: Vec<&String> = documented.difference(&api).collect();
    assert!(undocumented.is_empty(), "routes the document does not have: {undocumented:?}");
    assert!(unrouted.is_empty(), "paths in the document the router does not have: {unrouted:?}");
    assert_eq!(outside, PAGE_PATHS.into_iter().collect(), "only the page and the health check are outside the API");
    // Every route a native app needs is there.
    for path in ["/meta", "/openapi.json", "/events", "/plugins", "/setup/{id}", "/scenes", "/devices/{target}/notes"] {
        assert!(documented.contains(&format!("{API_PREFIX}{path}")), "{path}");
    }
}

#[tokio::test]
async fn each_path_answers_the_methods_the_document_gives_it_and_no_other() {
    let harness = Harness::new();
    for (path, methods) in operations() {
        let url = concrete(&path);
        for method in METHODS {
            if path.ends_with("/events") && method == Method::GET {
                // A stream that would not end without a session; it answers 401 below.
                continue;
            }
            let answer = harness.send(method.clone(), &url, None, Some(ORIGIN), None).await;
            if methods.contains(&method_key(&method)) {
                assert_ne!(
                    answer.status,
                    StatusCode::METHOD_NOT_ALLOWED,
                    "{method} {path} is documented: {}",
                    answer.text
                );
                assert_ne!(answer.code(), "not_found", "{method} {path} is documented: {}", answer.text);
            } else {
                assert_eq!(answer.status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path} is not documented");
            }
        }
    }
    let events = harness.send(Method::GET, "/api/v1/events", None, None, None).await;
    events.assert_refused(StatusCode::UNAUTHORIZED, "not_signed_in");
}

/// The scopes of the `session` scheme an operation asks for, or none when it is open.
fn scopes(operation: &Value) -> Option<Vec<String>> {
    let requirements = operation.get("security")?.as_array()?;
    let scopes = requirements.iter().find_map(|requirement| requirement.get("session"))?;
    Some(scopes.as_array().unwrap().iter().map(|scope| scope.as_str().unwrap().to_string()).collect())
}

#[tokio::test]
async fn who_may_call_each_operation_is_what_the_document_says() {
    let (harness, mut owner, owner_cookie) = Harness::signed_in().await;
    let (_family, family_cookie, _) = harness.add_family(&owner_cookie, &mut owner, "Bob").await;
    let mut open = Vec::new();
    for (path, methods) in operations() {
        let url = concrete(&path);
        for method in methods.iter().map(|method| Method::from_bytes(method.to_uppercase().as_bytes()).unwrap()) {
            let operation = operation(&path, &method);
            let scopes = scopes(operation);
            let anonymous = harness.send(method.clone(), &url, None, Some(ORIGIN), None).await;
            match &scopes {
                None => {
                    assert_ne!(
                        anonymous.status,
                        StatusCode::UNAUTHORIZED,
                        "{method} {path} is open: {}",
                        anonymous.text
                    );
                    open.push(format!("{method} {path}"));
                }
                Some(_) => {
                    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED, "{method} {path} needs a session");
                    assert!(operation["responses"].get("401").is_some(), "{method} {path} lists its 401");
                }
            }
            // An owner-only operation refuses a family member before it looks at anything else.
            if scopes.as_ref().is_some_and(|scopes| scopes.iter().any(|scope| scope == "owner")) {
                let family = harness.send(method.clone(), &url, Some(&family_cookie), Some(ORIGIN), None).await;
                family.assert_refused(StatusCode::FORBIDDEN, "owner_only");
                assert!(operation["responses"].get("403").is_some(), "{method} {path} lists its 403");
            }
        }
    }
    open.sort();
    assert_eq!(
        open,
        [
            "GET /api/v1/meta",
            "GET /api/v1/openapi.json",
            "POST /api/v1/auth/enroll/finish",
            "POST /api/v1/auth/enroll/start",
            "POST /api/v1/auth/invite/finish",
            "POST /api/v1/auth/invite/start",
            "POST /api/v1/auth/login/finish",
            "POST /api/v1/auth/login/start",
        ],
        "only these are open to anyone"
    );
    // A session that is owner-only in the document and a family member's that is not.
    let audit = harness.get("/api/v1/audit", &family_cookie).await;
    audit.assert_refused(StatusCode::FORBIDDEN, "owner_only");
    harness.get("/api/v1/catalog", &family_cookie).await.assert_ok();
}

#[tokio::test]
async fn the_deprecated_routes_are_marked_and_say_so() {
    let (harness, _owner, cookie) = Harness::signed_in().await;
    for (path, methods) in operations() {
        let below = path.strip_prefix(API_PREFIX).unwrap();
        for method in methods {
            let operation = &document()["paths"][&path][&method];
            let deprecated = DEPRECATED.contains(&below);
            assert_eq!(operation["deprecated"].as_bool().unwrap_or(false), deprecated, "{method} {path}");
            for response in operation["responses"].as_object().unwrap().values().map(response) {
                assert_eq!(response["headers"].get("Deprecation").is_some(), deprecated, "{method} {path}");
                assert!(response["headers"].get("X-Swifty-Api").is_some(), "{method} {path} names X-Swifty-Api");
            }
        }
    }
    let lights = harness.get("/api/v1/lights", &cookie).await;
    lights.assert_ok();
    assert_eq!(lights.headers["deprecation"], DEPRECATION);
    // On a refusal too.
    let anonymous = harness.send(Method::POST, "/api/v1/lights/discover", None, Some(ORIGIN), None).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.headers["deprecation"], DEPRECATION);
    let devices = harness.get("/api/v1/devices", &cookie).await;
    devices.assert_ok();
    assert!(devices.headers.get("deprecation").is_none());
}

#[tokio::test]
async fn the_committed_document_is_the_one_the_router_makes_and_the_hub_serves() {
    let committed = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api/openapi.v1.json"))
        .expect("docs/api/openapi.v1.json is committed");
    assert!(
        committed == document_json(),
        "docs/api/openapi.v1.json is out of date: run `cargo xtask openapi` and commit the result"
    );
    let harness = Harness::new();
    let served = harness.send(Method::GET, "/api/v1/openapi.json", None, None, None).await;
    served.assert_ok();
    assert_eq!(&served.body, document());
    assert_eq!(served.headers["x-swifty-api"], "1");
    assert_eq!(document()["openapi"], "3.1.0");
    assert_eq!(document()["info"]["version"], "1.0");
}

// ----- responses against their schemas

/// Checks a value against a schema of the document. An object schema that lists
/// its properties takes no other key, so a field added to a response and not to
/// its schema is caught.
fn check(schema: &Value, value: &Value, at: &str, errors: &mut Vec<String>) {
    let schema = resolve(schema);
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        for part in all {
            check_loose(part, value, at, errors);
        }
        check_keys(schema, value, at, errors);
        return;
    }
    if let Some(any) = schema.get("oneOf").or_else(|| schema.get("anyOf")).and_then(Value::as_array) {
        let fits = any.iter().any(|branch| {
            let mut mine = Vec::new();
            check(branch, value, at, &mut mine);
            mine.is_empty()
        });
        if !fits {
            errors.push(format!("{at}: {value} fits none of {}", Value::Array(any.clone())));
        }
        return;
    }
    check_loose(schema, value, at, errors);
    check_keys(schema, value, at, errors);
}

fn resolve(schema: &Value) -> &Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(reference) => {
            let name = reference.strip_prefix("#/components/schemas/").expect("a schema reference");
            resolve(&document()["components"]["schemas"][name])
        }
        None => schema,
    }
}

/// The keys an object schema declares, through `allOf`, or `None` when it takes any.
fn declared(schema: &Value) -> Option<BTreeSet<String>> {
    let schema = resolve(schema);
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        let mut keys = BTreeSet::new();
        for part in all {
            keys.extend(declared(part)?);
        }
        return Some(keys);
    }
    if schema.get("additionalProperties").is_some_and(|more| more != &json!(false)) {
        return None;
    }
    Some(schema.get("properties")?.as_object()?.keys().cloned().collect())
}

fn check_keys(schema: &Value, value: &Value, at: &str, errors: &mut Vec<String>) {
    let (Some(keys), Some(object)) = (declared(schema), value.as_object()) else {
        return;
    };
    for key in object.keys().filter(|key| !keys.contains(*key)) {
        errors.push(format!("{at}.{key} is not in the schema"));
    }
}

/// Everything but the check for keys the schema does not declare.
fn check_loose(schema: &Value, value: &Value, at: &str, errors: &mut Vec<String>) {
    let schema = resolve(schema);
    if schema.get("allOf").is_some() || schema.get("oneOf").is_some() || schema.get("anyOf").is_some() {
        let mut mine = Vec::new();
        check(schema, value, at, &mut mine);
        errors.extend(mine.into_iter().filter(|error| !error.ends_with("is not in the schema")));
        return;
    }
    if let Some(kinds) = schema.get("type") {
        let kinds: Vec<&str> = match kinds {
            Value::String(kind) => vec![kind.as_str()],
            Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let fits = kinds.iter().any(|kind| match *kind {
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "string" => value.is_string(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            _ => true,
        });
        if !fits {
            errors.push(format!("{at}: {value} is not {kinds:?}"));
            return;
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            errors.push(format!("{at}: {value} is not one of {allowed:?}"));
        }
    }
    if let Some(object) = value.as_object() {
        for required in schema.get("required").and_then(Value::as_array).into_iter().flatten() {
            let key = required.as_str().unwrap();
            if !object.contains_key(key) {
                errors.push(format!("{at}.{key} is missing"));
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, field) in object {
            match properties.and_then(|properties| properties.get(key)) {
                Some(property) => check(property, field, &format!("{at}.{key}"), errors),
                None => {
                    if let Some(more) = schema.get("additionalProperties").filter(|more| more.is_object()) {
                        check(more, field, &format!("{at}.{key}"), errors);
                    }
                }
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (n, item) in array.iter().enumerate() {
            check(items, item, &format!("{at}[{n}]"), errors);
        }
    }
}

/// The schema the document gives this answer: its status, or `default`.
fn response_schema(method: &Method, path: &str, status: StatusCode) -> &'static Value {
    let template = document()["paths"]
        .as_object()
        .unwrap()
        .keys()
        .find(|template| concrete_matches(template, path))
        .unwrap_or_else(|| panic!("no path of the document matches {path}"));
    let responses = &operation(template, method)["responses"];
    let answer = response(responses.get(status.as_str()).unwrap_or(&responses["default"]));
    &answer["content"]["application/json"]["schema"]
}

fn concrete_matches(template: &str, path: &str) -> bool {
    let path = path.split('?').next().unwrap();
    let (a, b): (Vec<&str>, Vec<&str>) = (template.split('/').collect(), path.split('/').collect());
    a.len() == b.len() && a.iter().zip(&b).all(|(t, p)| t == p || (t.starts_with('{') && t.ends_with('}')))
}

/// Checks an answer against the schema of its operation and status.
#[track_caller]
fn conforms(method: Method, path: &str, answer: &Resp) {
    let schema = response_schema(&method, path, answer.status);
    assert!(!schema.is_null(), "{method} {path} answered {} with JSON the document does not describe", answer.status);
    let mut errors = Vec::new();
    check(schema, &answer.body, "$", &mut errors);
    assert!(errors.is_empty(), "{method} {path} ({}): {errors:#?}\n{}", answer.status, answer.text);
}

#[test]
fn the_schema_check_catches_drift() {
    let errors_for = |schema: Value, value: Value| {
        let mut errors = Vec::new();
        check(&schema, &value, "$", &mut errors);
        errors
    };
    let meta = json!({ "$ref": "#/components/schemas/Meta" });
    let good = json!({
        "product": "Swifty Home", "version": "0.1.0", "api": { "major": 1, "minor": 0 }, "plugin_api": 1,
        "live": false, "needs_owner": true, "auth": ["passkey"], "events": ["sse"],
    });
    assert_eq!(errors_for(meta.clone(), good.clone()), Vec::<String>::new());
    let mut more = good.clone();
    more["extra"] = json!(1);
    assert_eq!(errors_for(meta.clone(), more), ["$.extra is not in the schema"]);
    let mut nested = good.clone();
    nested["api"]["patch"] = json!(0);
    assert_eq!(errors_for(meta.clone(), nested), ["$.api.patch is not in the schema"]);
    let mut less = good.clone();
    less.as_object_mut().unwrap().remove("live");
    assert_eq!(errors_for(meta.clone(), less), ["$.live is missing"]);
    let mut wrong = good;
    wrong["auth"] = json!("passkey");
    assert_eq!(errors_for(meta, wrong).len(), 1);
    // Through a flattened struct (`allOf`) and a tagged enum (`oneOf`).
    let row = json!({ "$ref": "#/components/schemas/SessionRow" });
    let errors = errors_for(row, json!({ "current": true, "session_ref": "s" }));
    assert!(errors.iter().any(|error| error.contains("credential_id is missing")), "{errors:?}");
    let state = json!({ "$ref": "#/components/schemas/State" });
    assert!(errors_for(state.clone(), json!({ "kind": "plug", "on": true })).is_empty());
    assert!(!errors_for(state.clone(), json!({ "kind": "plug", "on": "yes" })).is_empty());
    assert!(!errors_for(state, json!({ "kind": "toaster", "on": true })).is_empty());
}

#[tokio::test]
async fn what_the_hub_answers_is_what_the_document_says() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    // A node's energy counter has been read, so the energy answer has a target to describe.
    harness.devices.set_collector(json!({ "energy": { "since": 1_794_380_000, "total_kwh": 2.5 } }));
    swifty_hub::history::sample_collector(&harness.app).await.unwrap();
    let mut seen = BTreeSet::new();
    let mut get = async |path: &str| {
        let answer = harness.get(path, &cookie).await;
        answer.assert_ok();
        conforms(Method::GET, path, &answer);
        seen.insert(format!("GET {}", path.split('?').next().unwrap()));
        answer
    };
    for path in [
        "/api/v1/meta",
        "/api/v1/session",
        "/api/v1/me",
        "/api/v1/catalog",
        "/api/v1/devices",
        "/api/v1/state",
        "/api/v1/state/bulb1",
        "/api/v1/state/vm100",
        "/api/v1/rooms",
        "/api/v1/scenes",
        "/api/v1/users",
        "/api/v1/passkeys",
        "/api/v1/sessions",
        "/api/v1/audit?limit=20",
        "/api/v1/audit/verify",
        "/api/v1/plugins",
        "/api/v1/plugins/mock",
        "/api/v1/machines",
        "/api/v1/energy",
        "/api/v1/memory",
        "/api/v1/health",
        "/api/v1/lights",
        "/api/v1/devices/bulb1/notes",
        "/api/v1/openapi.json",
    ] {
        get(path).await;
    }
    let plugins = get("/api/v1/plugins").await;
    let instance = plugins.body["instances"][0]["id"].as_str().unwrap().to_string();
    get(&format!("/api/v1/instances/{instance}")).await;
    get(&format!("/api/v1/instances/{instance}/candidates")).await;

    // The three flows: direct, confirm and step-up.
    let post = async |path: &str, body: Value| {
        let answer = harness.post(path, &cookie, body).await;
        conforms(Method::POST, path, &answer);
        answer
    };
    let direct = post(
        "/api/v1/actions/execute",
        json!({ "action": "light.power", "target": "bulb1", "params": { "on": true } }),
    )
    .await;
    direct.assert_ok();
    let prepared =
        post("/api/v1/actions/prepare", json!({ "action": "print.pause", "target": "printer", "params": {} })).await;
    prepared.assert_ok();
    assert_eq!(prepared.body["needs"], "tap");
    let confirmed = harness.confirm_tap(&cookie, &prepared).await;
    confirmed.assert_ok();
    conforms(Method::POST, "/api/v1/actions/confirm", &confirmed);
    let params = json!({ "plugin": "mock", "instance": "demo" });
    let critical =
        post("/api/v1/actions/prepare", json!({ "action": "plugin.add", "target": "new", "params": params })).await;
    assert_eq!(critical.body["needs"], "passkey");
    let started = harness.step_up_start(&cookie, &critical).await;
    started.assert_ok();
    conforms(Method::POST, "/api/v1/auth/step-up/start", &started);
    let assertion = assert_with(&mut owner, &started.body["options"], origin_url());
    let opened = harness.confirm_assertion(&cookie, &critical, &assertion).await;
    opened.assert_ok();
    conforms(Method::POST, "/api/v1/actions/confirm", &opened);

    // The setup the critical action opened, as the setup routes and the run show it.
    let setup_id = opened.body["setup"]["setup_id"].as_str().unwrap().to_string();
    let setup_path = format!("/api/v1/setup/{setup_id}");
    let step = get(&setup_path).await;
    assert_eq!(step.body, opened.body["setup"]);
    let form = post(&setup_path, json!({ "input": "chosen", "ids": ["lamp"] })).await;
    form.assert_ok();
    let done = post(&setup_path, json!({ "input": "values", "values": { "access_code": "demo-code-0815" } })).await;
    assert_eq!(done.body["step"]["step"], "done", "{}", done.text);

    let runs = get("/api/v1/actions/runs").await;
    let run_id = runs.body["runs"][0]["run_id"].as_str().unwrap().to_string();
    get(&format!("/api/v1/actions/runs/{run_id}")).await;
    let discovered = post("/api/v1/instances/demo/discover", json!(null)).await;
    discovered.assert_ok();

    // A run that carries a ceremony: another passkey.
    let registered = harness
        .critical(&cookie, &mut owner, "passkey.register", "new", json!({ "name": "Tablet", "recovery": false }))
        .await;
    registered.assert_ok();
    assert!(registered.body["registration"]["options"].is_object(), "{}", registered.text);
    conforms(Method::POST, "/api/v1/actions/confirm", &registered);

    // Arrangements.
    let room = post("/api/v1/rooms", json!({ "name": "Study" })).await;
    room.assert_ok();
    let favourites = harness
        .send(Method::PUT, "/api/v1/me/favourites", Some(&cookie), Some(ORIGIN), Some(json!({ "targets": ["bulb1"] })))
        .await;
    conforms(Method::PUT, "/api/v1/me/favourites", &favourites);
    let memory = post("/api/v1/memory", json!({ "text": "The plants are watered on Sundays." })).await;
    memory.assert_ok();
    // The tariff, and the energy answer once there is a tariff and a cost.
    let price_path = "/api/v1/energy/tariff";
    let price = harness
        .send(
            Method::PUT,
            price_path,
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "currency": "EUR", "day_price_per_kwh": 0.3, "night_price_per_kwh": 0.1,
                         "night": { "start": "22:00", "end": "06:00" }, "time_zone": "UTC" })),
        )
        .await;
    price.assert_ok();
    conforms(Method::PUT, price_path, &price);
    assert_eq!(price.body["targets"][0]["backfilled_kwh"], json!(2.5), "{}", price.text);
    let priced = harness.get("/api/v1/energy", &cookie).await;
    conforms(Method::GET, "/api/v1/energy", &priced);
    let refused = harness
        .send(
            Method::PUT,
            price_path,
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "day_price_per_kwh": 0, "currency": "EUR" })),
        )
        .await;
    refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_tariff");
    conforms(Method::PUT, price_path, &refused);

    // An error, in the envelope every operation shares.
    let missing = harness.get("/api/v1/actions/runs/nothing", &cookie).await;
    missing.assert_refused(StatusCode::NOT_FOUND, "unknown_run");
    conforms(Method::GET, "/api/v1/actions/runs/nothing", &missing);
    let refused = harness.post("/api/v1/actions/execute", &cookie, json!({ "action": "light.power" })).await;
    refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_body");
    conforms(Method::POST, "/api/v1/actions/execute", &refused);
    assert!(seen.len() > 25, "{seen:?}");
}

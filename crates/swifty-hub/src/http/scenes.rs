//! House scenes: `GET /api/v1/scenes` for anyone signed in, `PUT /api/v1/scenes/{id}`
//! for the owner.
//!
//! A scene is what [`crate::scenes`] stores: one level for every light, the lights
//! that differ from it, and for Movie what it does besides the lights. The catalog
//! plans `scene.apply` and `scene.movie` from the stored row, so a change here is
//! what the next run of the scene does; nothing is sent to a device now.
//!
//! A `PUT` changes a scene that exists. The scene actions have fixed targets, so a
//! new id would be a scene nothing could run, and it is refused as unknown. Each
//! change is audited as `scene.save` and published as a `devices` event, so open
//! pages fetch again.

use std::collections::BTreeSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use swifty_plugin_api::DeviceClass;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::{Authed, Body, Owner};
use super::{audit_arrangement, unprocessable, RouterState};
use crate::app::App;
use crate::catalog::validate_label;
use crate::error::ApiError;
use crate::events::Event;
use crate::policy::{self, Role};
use crate::runtime::DeviceIndex;
use crate::scenes::{self, Scene, SceneOverride};

/// The scene that does more than set the lights.
const MOVIE: &str = "movie";
/// The highest place a scene can be listed at.
const MAX_POSITION: i64 = 999;
/// The longest TV input name Movie can select.
const MAX_INPUT: usize = 40;

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().routes(routes!(scenes_list)).routes(routes!(scene_save))
}

/// A scene as the owner writes it. `position` and `extras` may be left out to keep
/// what is stored.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct SceneBody {
    name: String,
    percent: u8,
    hue: u16,
    overrides: Vec<SceneOverride>,
    #[serde(default)]
    position: Option<i64>,
    #[serde(default)]
    extras: Option<Extras>,
}

/// What Movie does besides the lights: the TV input it selects and the volume it
/// caps the speaker at. Nothing else is accepted.
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = SceneExtras)]
struct Extras {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tv_input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    volume_cap: Option<u8>,
}

fn unknown_scene() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_scene", "There is no such scene here.")
}

fn invalid(message: impl Into<String>) -> ApiError {
    unprocessable("invalid_scene", message)
}

/// The scene as this person may see it: a family member is not told of a level
/// for a device the index does not show them.
fn view(scene: Scene, index: &DeviceIndex, role: Role) -> Scene {
    if role == Role::Owner {
        return scene;
    }
    let overrides = scene.overrides.into_iter().filter(|level| policy::may_see(index, role, &level.target)).collect();
    Scene { overrides, ..scene }
}

/// The house scenes, in the order they are listed.
#[derive(Serialize, ToSchema)]
pub struct Scenes {
    pub scenes: Vec<Scene>,
}

/// One scene, after a change.
#[derive(Serialize, ToSchema)]
pub struct OneScene {
    pub scene: Scene,
}

/// Every house scene, in the order they are listed.
///
/// A family member is not told of a level for a device they may not see.
#[utoipa::path(
    get,
    path = "/scenes",
    tag = "scenes",
    security(("session" = [])),
    responses((status = 200, body = Scenes))
)]
async fn scenes_list(State(app): State<App>, Authed(session): Authed) -> Result<Json<Scenes>, ApiError> {
    let index = app.registry.index();
    let role = session.person.role;
    let scenes: Vec<Scene> = app.store.with(scenes::all)?.into_iter().map(|scene| view(scene, &index, role)).collect();
    Ok(Json(Scenes { scenes }))
}

/// The scene the body describes, on top of the stored one. A level may name a
/// light the index has, or one the stored scene already names (a light that is
/// away for now keeps its level).
fn changed_scene(stored: &Scene, body: SceneBody, index: &DeviceIndex) -> Result<Scene, ApiError> {
    let kept: BTreeSet<&str> = stored.overrides.iter().map(|level| level.target.as_str()).collect();
    for level in &body.overrides {
        let is_light =
            index.get(&level.target).is_some_and(|device| device.class == DeviceClass::Light && !device.pending);
        if !is_light && !kept.contains(level.target.as_str()) {
            return Err(invalid(format!("{} is not a light here.", level.target)));
        }
    }
    let position = body.position.unwrap_or(stored.position);
    if !(0..=MAX_POSITION).contains(&position) {
        return Err(invalid(format!("position is 0 to {MAX_POSITION}.")));
    }
    let extras = match body.extras {
        None => stored.extras.clone(),
        Some(extras) => {
            if stored.id != MOVIE && (extras.tv_input.is_some() || extras.volume_cap.is_some()) {
                return Err(invalid("Only Movie does more than set the lights."));
            }
            if let Some(input) = &extras.tv_input {
                validate_label("tv_input", input, MAX_INPUT).map_err(|e| invalid(e.0))?;
            }
            if extras.volume_cap.is_some_and(|cap| cap > 100) {
                return Err(invalid("volume_cap is 0 to 100."));
            }
            serde_json::to_value(&extras).map_err(ApiError::internal)?
        }
    };
    let scene = Scene {
        id: stored.id.clone(),
        name: body.name,
        position,
        percent: body.percent,
        hue: body.hue,
        overrides: body.overrides,
        extras,
    };
    if let Some(problem) = scene.problem() {
        return Err(invalid(problem));
    }
    Ok(scene)
}

/// Replaces a stored scene's name, levels, place and extras.
///
/// Only a scene that exists: an unknown id is `404 unknown_scene`. A level may name
/// a light the hub has, or one the stored scene already names. Only Movie has
/// extras. A change is audited as `scene.save` and announced as a `devices` event.
#[utoipa::path(
    put,
    path = "/scenes/{id}",
    tag = "scenes",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The scene's id, which is also its target, such as `evening`.")),
    request_body = SceneBody,
    responses((status = 200, body = OneScene))
)]
async fn scene_save(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
    Body(body): Body<SceneBody>,
) -> Result<Json<OneScene>, ApiError> {
    let index = app.registry.index();
    let (scene, changed) = app.store.tx(|tx| -> Result<_, ApiError> {
        let stored = scenes::get(tx, &id)?.ok_or_else(unknown_scene)?;
        let scene = changed_scene(&stored, body, &index)?;
        scenes::save(tx, &scene)?;
        let params = json!({
            "name": scene.name,
            "percent": scene.percent,
            "hue": scene.hue,
            "overrides": scene.overrides.len(),
        });
        audit_arrangement(&app, tx, &session, "scene.save", &scene.id, params)?;
        let changed = scene != stored;
        Ok((scene, changed))
    })?;
    if changed {
        app.events.publish(Event::Devices);
    }
    Ok(Json(OneScene { scene }))
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::runtime::index::legacy_seeded;

    fn body(value: Value) -> SceneBody {
        serde_json::from_value(value).unwrap()
    }

    fn movie() -> Scene {
        scenes::house_scenes().into_iter().find(|scene| scene.id == MOVIE).unwrap()
    }

    #[test]
    fn left_out_fields_keep_what_is_stored() {
        let index = legacy_seeded();
        let stored = movie();
        let scene =
            changed_scene(&stored, body(json!({ "name": "Film", "percent": 12, "hue": 20, "overrides": [] })), &index)
                .unwrap();
        assert_eq!((scene.name.as_str(), scene.percent, scene.hue), ("Film", 12, 20));
        assert_eq!((scene.position, &scene.extras), (stored.position, &stored.extras));
    }

    #[test]
    fn extras_are_movies_alone_and_checked() {
        let index = legacy_seeded();
        let level =
            |extras: Value| json!({ "name": "Evening", "percent": 40, "hue": 34, "overrides": [], "extras": extras });
        let evening = scenes::house_scenes().into_iter().find(|scene| scene.id == "evening").unwrap();
        assert!(changed_scene(&evening, body(level(json!({ "volume_cap": 20 }))), &index).is_err());
        assert_eq!(changed_scene(&evening, body(level(json!({}))), &index).unwrap().extras, json!({}));

        let stored = movie();
        let scene = changed_scene(&stored, body(level(json!({ "tv_input": "HDMI 2" }))), &index).unwrap();
        assert_eq!(scene.extras, json!({ "tv_input": "HDMI 2" }), "extras given replace the stored ones");
        assert!(changed_scene(&stored, body(level(json!({ "volume_cap": 101 }))), &index).is_err());
        assert!(changed_scene(&stored, body(level(json!({ "tv_input": " HDMI 2" }))), &index).is_err());
        assert!(serde_json::from_value::<SceneBody>(level(json!({ "volume": 20 }))).is_err(), "unknown extras");
    }

    #[test]
    fn a_level_names_a_light_here_or_one_the_scene_already_had() {
        let index = legacy_seeded();
        let mut stored = movie();
        stored.overrides = vec![SceneOverride { target: "wiz-a8bb50aabbcc".to_string(), percent: 5, hue: 5 }];
        let with = |target: &str| {
            body(json!({ "name": "Movie", "percent": 10, "hue": 14,
                         "overrides": [{ "target": target, "percent": 20, "hue": 30 }] }))
        };
        assert!(changed_scene(&stored, with("bulb2"), &index).is_ok());
        assert!(changed_scene(&stored, with("wiz-a8bb50aabbcc"), &index).is_ok(), "kept from the stored scene");
        for wrong in ["tv", "vm100", "nothing-here"] {
            assert!(changed_scene(&stored, with(wrong), &index).is_err(), "{wrong}");
        }
    }
}

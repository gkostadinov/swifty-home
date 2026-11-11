//! The executors of the plugin lifecycle actions: adding,
//! reconfiguring and removing an integration.
//!
//! They are catalog actions, so they get the tier machinery, the passkey step-up
//! and the audit rows of any critical action, with no mechanism of their own:
//!
//! - `plugin.add` (target `new`, params `{ plugin, instance? }`) opens a setup
//!   session ([`crate::setup`]) and answers with `{ setup_id, step }`;
//! - `plugin.reconfigure` (an instance id) does the same from the stored config;
//! - `plugin.remove` (an instance id) removes the instance, its devices, their places
//!   in rooms and the favourites that name them, their names and notes, and its
//!   secrets. Before it is confirmed, the person is shown exactly what goes.
//!
//! The command line uses [`remove`] and the setup sessions directly, as the local
//! administrator; what they do is audited the same way, with its own channel.

use axum::http::StatusCode;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use swifty_plugin_api::{NetUse, Plugin, Reach};

use crate::app::App;
use crate::audit::Event;
use crate::catalog::{ActionDef, Description, Executor};
use crate::error::ApiError;
use crate::inventory;
use crate::runtime::contained;
use crate::setup::{self, no_setup, plugin_missing, Opener};

/// What an executor did, for the run's record and its response.
pub struct Executed {
    /// One line for the run's record.
    pub detail: String,
    /// The setup that was opened, as the API shows it.
    pub setup: Option<Value>,
}

fn param<'a>(params: &'a Value, name: &str) -> Option<&'a str> {
    params.get(name).and_then(Value::as_str)
}

fn has_setup(plugin: &dyn Plugin) -> bool {
    contained(|| plugin.setup().is_some()).unwrap_or(false)
}

/// The plugin of a stored instance, by its id there, if the instance exists.
fn plugin_of(conn: &Connection, instance: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT plugin FROM plugin_instances WHERE id = ?1", params![instance], |row| row.get(0)).optional()
}

/// Why one of these actions cannot run now, beyond what the catalog checks: the
/// plugin is not in this build or has no setup, the id is taken, a setup of the same
/// instance is open or too many are, or the hub itself runs on one of its devices.
pub fn refusal(app: &App, executor: Executor, target: &str, params: &Value) -> Result<Option<ApiError>, ApiError> {
    let refused = |result: Result<(), ApiError>| result.err();
    Ok(match executor {
        Executor::PluginAdd => {
            let plugin = param(params, "plugin").unwrap_or_default();
            let checked = setup::addable(app, plugin).and_then(|found| {
                if !has_setup(found) {
                    return Err(no_setup(found));
                }
                match param(params, "instance") {
                    Some(instance) => setup::check_new_instance_id(app, instance),
                    None => Ok(()),
                }
            });
            refused(checked).or_else(|| app.setup.refusal(app, param(params, "instance")))
        }
        Executor::PluginReconfigure => {
            let Some(plugin) = app.store.with(|conn| plugin_of(conn, target))? else {
                return Ok(None);
            };
            match app.registry.plugin(&plugin) {
                None => Some(plugin_missing(&plugin)),
                Some(found) if !has_setup(found) => Some(no_setup(found)),
                Some(_) => app.setup.refusal(app, Some(target)),
            }
        }
        Executor::PluginRemove => {
            let runs_here = app.config.runs_on.as_deref().is_some_and(|runs_on| {
                app.store
                    .with(|conn| {
                        conn.query_row(
                            "SELECT EXISTS(SELECT 1 FROM devices WHERE target = ?1 AND instance_id = ?2)",
                            params![runs_on, target],
                            |row| row.get::<_, bool>(0),
                        )
                    })
                    .unwrap_or(false)
            });
            runs_here.then(|| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "console_machine",
                    "This hub runs on one of this integration's devices, so the integration cannot be removed.",
                )
            })
        }
        _ => None,
    })
}

// ----- what the person is shown before confirming

/// "a", "a and b", "a, b and c".
fn listed(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn net_use(net: &NetUse) -> String {
    match net {
        NetUse::Http { port } => format!("HTTP on port {port}"),
        NetUse::Https { port } => format!("HTTPS on port {port}"),
        NetUse::Ws { port } => format!("a websocket on port {port}"),
        NetUse::Tcp { port } => format!("TCP port {port}"),
        NetUse::Udp { port } => format!("UDP port {port}"),
        NetUse::UdpBroadcast { port } => format!("broadcasts on UDP port {port}"),
        NetUse::WakeOnLan => "Wake-on-LAN".to_string(),
        NetUse::CloudHost { host } => format!("HTTPS to {host}"),
    }
}

/// Where a plugin's traffic goes, as one or two sentences for the person adding it.
pub fn reach_text(plugin: &dyn Plugin) -> Vec<String> {
    let manifest = plugin.manifest();
    let uses: Vec<String> = manifest.network.iter().map(net_use).collect();
    let how = if uses.is_empty() { String::new() } else { format!(": {}", listed(&uses)) };
    match manifest.reach {
        Reach::Local => vec![format!("It talks only to devices on your network{how}.")],
        Reach::Cloud { vendor, official: true } => {
            vec![format!("It goes through {vendor}'s servers on the internet{how}.")]
        }
        Reach::Cloud { vendor, official: false } => vec![
            format!("It goes through {vendor}'s servers on the internet{how}."),
            format!(
                "{vendor} does not document this for others: it can stop working whenever {vendor} changes something, \
                 and it may be against {vendor}'s terms."
            ),
        ],
    }
}

/// What the person is shown before confirming: what this action does to this
/// integration, then what the catalog says. An instance id may also be a device's
/// target (the bridge of an instance called `hue` is `hue`), so nothing said about
/// that device is kept.
pub fn describe(
    app: &App,
    def: &ActionDef,
    target: &str,
    params: &Value,
    text: &mut Description,
) -> Result<(), ApiError> {
    let mut own: Vec<String> = Vec::new();
    text.label = def.label.to_string();
    text.target_name = target.to_string();
    text.what = def.description.to_string();
    text.affected = Vec::new();
    text.warning = None;
    match def.executor {
        Executor::PluginAdd => {
            if let Some(plugin) = param(params, "plugin").and_then(|id| app.registry.plugin(id)) {
                let manifest = plugin.manifest();
                text.label = format!("Add {}", manifest.name);
                text.target_name = manifest.name.to_string();
                text.what = format!("Starts the setup of {}: {}", manifest.name, manifest.summary);
                own.extend(reach_text(plugin));
            }
        }
        Executor::PluginReconfigure => {
            let plugin = app.store.with(|conn| plugin_of(conn, target))?;
            let name = plugin
                .as_deref()
                .and_then(|id| app.registry.plugin(id))
                .map_or_else(|| target.to_string(), |plugin| plugin.manifest().name.to_string());
            text.label = format!("Set up {target} again");
            text.target_name = target.to_string();
            text.what = format!("Starts the setup of {target} ({name}) again, from its stored settings.");
        }
        Executor::PluginRemove => {
            text.label = format!("Remove {target}");
            if let Some(goes) = removal(app, target)? {
                own.extend(goes.lines());
                text.affected = goes.devices.iter().map(|(_, name)| name.clone()).collect();
            }
        }
        _ => {}
    }
    own.extend(def.consequences.iter().map(|line| line.to_string()));
    text.consequences = own;
    Ok(())
}

// ----- running them

/// Runs one of the three. `opener` is who confirmed it, as the setup's and the
/// removal's own audit rows say.
pub async fn execute(
    app: &App,
    opener: &Opener,
    executor: Executor,
    target: &str,
    params: &Value,
) -> Result<Executed, ApiError> {
    match executor {
        Executor::PluginAdd => {
            let plugin = param(params, "plugin").unwrap_or_default();
            let view = app.setup.begin(app, opener, plugin, param(params, "instance")).await?;
            Ok(Executed { detail: format!("The setup of {} is open.", view.instance), setup: Some(view.to_json()) })
        }
        Executor::PluginReconfigure => {
            let view = app.setup.begin_again(app, opener, target).await?;
            Ok(Executed { detail: format!("The setup of {} is open.", view.instance), setup: Some(view.to_json()) })
        }
        Executor::PluginRemove => {
            let removed = remove(app, opener, target).await?;
            Ok(Executed { detail: removed.summary(), setup: None })
        }
        _ => Err(ApiError::internal("not a plugin action")),
    }
}

/// Everything that goes with an instance.
#[derive(Debug, Clone)]
pub struct Removal {
    pub instance: String,
    pub plugin: String,
    /// Its devices, by target, with the name each is shown under.
    pub devices: Vec<(String, String)>,
    /// Places of its devices in rooms.
    pub room_places: usize,
    /// Favourites, of anyone, that name one of its devices.
    pub favourites: usize,
    /// Its secret slots that hold a value. Never the values.
    pub secrets: Vec<String>,
    /// The further hosts that were added beside it, which go with it.
    pub further_hosts: Vec<String>,
}

impl Removal {
    /// What goes, as sentences, most important first.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.devices.is_empty() {
            lines.push("It has no devices.".to_string());
        } else {
            let names: Vec<String> = self.devices.iter().map(|(_, name)| name.clone()).collect();
            lines.push(match names.as_slice() {
                [one] => format!("Its device goes: {one}."),
                _ => format!("Its {} devices go: {}.", names.len(), listed(&names)),
            });
        }
        let mut places = Vec::new();
        if self.room_places > 0 {
            places.push(counted(self.room_places, "place in a room", "places in rooms"));
        }
        if self.favourites > 0 {
            places.push(counted(self.favourites, "favourite", "favourites"));
        }
        if !places.is_empty() {
            lines.push(format!("With them go {}.", listed(&places)));
        }
        if !self.secrets.is_empty() {
            lines.push(format!("Its stored secrets go: {}.", listed(&self.secrets)));
        }
        if !self.further_hosts.is_empty() {
            lines.push(format!(
                "The {} added beside it go too, with their machines: {}.",
                if self.further_hosts.len() == 1 { "further host" } else { "further hosts" },
                listed(&self.further_hosts)
            ));
        }
        lines
    }

    /// One line for the run's record.
    pub fn summary(&self) -> String {
        format!(
            "Removed {}: {}, {}, {} and {}.",
            self.instance,
            counted(self.devices.len(), "device", "devices"),
            counted(self.room_places, "room place", "room places"),
            counted(self.favourites, "favourite", "favourites"),
            counted(self.secrets.len(), "secret", "secrets"),
        )
    }
}

fn count(conn: &Connection, sql: &str, instance: &str) -> rusqlite::Result<usize> {
    conn.query_row(sql, params![instance], |row| row.get::<_, i64>(0)).map(|n| usize::try_from(n).unwrap_or(0))
}

/// What removing this instance would take with it. `None` when there is no such instance.
pub fn removal(app: &App, instance: &str) -> Result<Option<Removal>, ApiError> {
    let further_hosts = match app.registry.host_instance() {
        Some(host) if host.spec.id == instance => {
            app.store.with(inventory::hosts)?.into_iter().map(|host| host.id).collect()
        }
        _ => Vec::new(),
    };
    let found = app.store.with(|conn| -> rusqlite::Result<Option<Removal>> {
        let Some(plugin) = plugin_of(conn, instance)? else {
            return Ok(None);
        };
        let devices = inventory::stored_devices(conn)?
            .into_iter()
            .filter(|device| device.instance == instance)
            .map(|device| (device.target, device.name))
            .collect();
        let room_places = count(
            conn,
            "SELECT COUNT(*) FROM room_devices WHERE target IN (SELECT target FROM devices WHERE instance_id = ?1)",
            instance,
        )?;
        let favourites = count(
            conn,
            "SELECT COUNT(*) FROM favourites WHERE target IN (SELECT target FROM devices WHERE instance_id = ?1)",
            instance,
        )?;
        Ok(Some(Removal {
            instance: instance.to_string(),
            plugin,
            devices,
            room_places,
            favourites,
            secrets: Vec::new(),
            further_hosts,
        }))
    })?;
    Ok(found.map(|mut removal| {
        removal.secrets = app.registry.secret_store().slots(instance).unwrap_or_default().into_iter().collect();
        removal
    }))
}

/// Removes an instance with everything that goes with it, and loads what is left.
/// The secrets go first: if they cannot, nothing else is touched, so a credential
/// is never left behind an instance that is gone. Audit rows are kept; one more,
/// `plugin.remove` `done`, names what went (the slots, never their values).
pub async fn remove(app: &App, opener: &Opener, instance: &str) -> Result<Removal, ApiError> {
    let Some(removal) = removal(app, instance)? else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_instance", "There is no such integration here."));
    };
    app.setup.abandon_instance(app, instance).await;
    if let Err(error) = app.registry.secret_store().remove_instance(instance) {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secrets_not_removed",
            format!("Its secrets could not be removed ({:?}). Nothing was removed.", error.kind()),
        ));
    }
    for host in &removal.further_hosts {
        app.registry.remove_host(host);
    }
    let actor = &opener.actor;
    app.store.tx(|tx| -> Result<(), ApiError> {
        for (target, _) in &removal.devices {
            for table in ["room_devices", "favourites", "device_names", "device_notes"] {
                tx.execute(&format!("DELETE FROM {table} WHERE target = ?1"), params![target])?;
            }
        }
        for host in &removal.further_hosts {
            tx.execute("DELETE FROM proxmox_hosts WHERE id = ?1", params![host])?;
        }
        tx.execute("DELETE FROM device_candidates WHERE instance_id = ?1", params![instance])?;
        tx.execute("DELETE FROM devices WHERE instance_id = ?1", params![instance])?;
        tx.execute("DELETE FROM plugin_instances WHERE id = ?1", params![instance])?;
        inventory::bump_instances_rev(tx)?;
        let event = Event {
            ts: app.now(),
            action: "plugin.remove".to_string(),
            target: instance.to_string(),
            params: json!({
                "instance": instance,
                "plugin": removal.plugin,
                "devices": removal.devices.len(),
                "room_places": removal.room_places,
                "favourites": removal.favourites,
                "secrets": removal.secrets,
                "further_hosts": removal.further_hosts,
            }),
            credential_ref: actor.credential_id.clone(),
            session_ref: actor.session_ref.clone(),
            user_ref: actor.user_id.clone(),
            device: actor.device.clone(),
            channel: opener.channel.clone(),
            step_up: opener.step_up.clone(),
            result: "done".to_string(),
            rtt_ms: None,
            run_id: None,
        };
        app.append_audit(tx, &event)?;
        Ok(())
    })?;
    app.sync_directory()?;
    Ok(removal)
}

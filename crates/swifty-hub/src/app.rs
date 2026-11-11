//! The shared application state handed to every request.

use std::ops::Deref;
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use thiserror::Error;
use webauthn_rs::prelude::{Uuid, WebauthnError};

use crate::assistant::Assistant;
use crate::audit::{self, AuditKey, Event, VerifyOptions, VerifyReport};
use crate::clock::Clock;
use crate::config::{Config, ConfigError};
use crate::console::ConsoleBroker;
use crate::crypto::random_bytes;
use crate::error::ApiError;
use crate::events::EventBus;
use crate::inventory;
use crate::passkeys::Passkeys;
use crate::runtime::{Origin, Registry};
use crate::sessions::Limits;
use crate::setup::SetupSessions;
use crate::store::Store;
use crate::voice::{DroppedWord, PluginWords, SpeechClient};
use crate::{actions, catalog, voice};

pub const MAX_ANONYMOUS_REFUSALS_PER_MINUTE: i64 = 30;
/// The audit action of the row that counts refusals the cap kept out.
pub const DROPPED_REFUSALS_ACTION: &str = "audit.refusals_dropped";
const DROPPED_REFUSALS_KEY: &str = "audit_refusals_dropped";
const MINUTE_MS: i64 = 60_000;

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("database: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("passkey setup: {0:?}")]
    Webauthn(WebauthnError),
    #[error("speech client: {0}")]
    Http(#[from] reqwest::Error),
    #[error("refusing to start: a live device registry was given a configuration that is not live")]
    LiveMismatch,
}

pub struct Inner {
    pub config: Config,
    pub store: Store,
    pub clock: Arc<dyn Clock>,
    pub registry: Registry,
    pub passkeys: Passkeys,
    pub speech: Option<SpeechClient>,
    pub assistant: Option<Assistant>,
    pub audit_key: Option<AuditKey>,
    pub consoles: ConsoleBroker,
    /// What changed, for the event stream. The registry publishes to it too.
    pub events: Arc<EventBus>,
    /// The words the plugins of this build give voice and Swifty.
    pub plugin_words: Vec<PluginWords>,
    /// The words of theirs the grammar would not take, for the plugin page.
    pub dropped_words: Vec<DroppedWord>,
    /// The plugin setups that are open.
    pub setup: SetupSessions,
}

#[derive(Clone)]
pub struct App(Arc<Inner>);

impl Deref for App {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl App {
    /// The hub's app: what `swifty serve` runs.
    pub fn new(config: Config, clock: Arc<dyn Clock>, registry: Registry) -> Result<Self, StartupError> {
        let app = Self::open(config, clock, registry)?;
        app.load_further_hosts()?;
        crate::actions::recover_interrupted_runs(&app)?;
        app.store.tx(|tx| app.flush_dropped_refusals(tx, app.now()))?;
        Ok(app)
    }

    /// The app of a command that works beside a running hub, on the same database:
    /// the command line. It loads the instances as the hub does, and leaves alone
    /// what belongs to the service: a run of the hub's that waits for a device is
    /// not closed as interrupted, and no further host is added.
    pub fn for_command_line(config: Config, clock: Arc<dyn Clock>, registry: Registry) -> Result<Self, StartupError> {
        Self::open(config, clock, registry)
    }

    fn open(config: Config, clock: Arc<dyn Clock>, registry: Registry) -> Result<Self, StartupError> {
        if registry.is_live() && !config.live {
            return Err(StartupError::LiveMismatch);
        }
        let store = Store::open(&config.database_path())?;
        let owner = owner_id(&store)?;
        let passkeys = Passkeys::new(&config.webauthn, owner).map_err(StartupError::Webauthn)?;
        let speech = config.speech.as_ref().map(SpeechClient::new).transpose()?;
        // In dry-run nothing leaves this machine, and that includes anything said to the
        // model. The assistant uses the registry's grant: a live registry is the only
        // thing that has one.
        let assistant = match (config.assistant.as_ref(), registry.grant()) {
            (Some(settings), Some(grant)) => Some(Assistant::new(grant, settings)?),
            _ => None,
        };
        let audit_key = config.audit.key_file.as_deref().map(AuditKey::load).transpose()?;
        if audit_key.is_none() {
            tracing::warn!(
                "audit.key_file is not set: someone who can write the database can rewrite the audit log undetected"
            );
        }
        // The plugins' custom actions join the catalog, and their words the lexicon,
        // once for the life of the process.
        let plugins: Vec<_> = registry.plugins().collect();
        catalog::register_plugins(&plugins);
        let plugin_words: Vec<PluginWords> =
            plugins.iter().map(|plugin| PluginWords::from(plugin.manifest())).collect();
        let dropped_words = voice::register_plugin_words(&plugin_words);
        let app = Self(Arc::new(Inner {
            config,
            store,
            clock,
            registry,
            passkeys,
            speech,
            assistant,
            audit_key,
            consoles: ConsoleBroker::default(),
            events: Arc::new(EventBus::new()),
            plugin_words,
            dropped_words,
            setup: SetupSessions::default(),
        }));
        app.sync_directory()?;
        // Only after the first load: nobody can be following yet, so what the start
        // itself loads is not news. Every later rebuild, change of an instance and
        // changed reading is.
        app.registry.publish_to(app.events.clone());
        Ok(app)
    }

    pub fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Brings the devices each instance's config names into the store, then loads
    /// the instances and every stored device into the registry, which rebuilds the
    /// device index it routes by and keeps each instance's directory current. Call
    /// it after anything that changes the instances, the devices or their names.
    /// The registry publishes `devices`, and `plugin` for each instance whose
    /// readiness or health changed with it.
    pub fn sync_directory(&self) -> rusqlite::Result<()> {
        let now = self.now();
        self.store.tx(|tx| -> rusqlite::Result<()> {
            let stored = inventory::stored_devices(tx)?;
            for spec in inventory::instance_specs(tx)? {
                let adopted: Vec<_> = stored
                    .iter()
                    .filter(|device| device.instance == spec.id && device.origin == Origin::Adopted && !device.pending)
                    .map(|device| device.record())
                    .collect();
                if let Some(declared) = self.registry.declared(&spec, &adopted) {
                    inventory::store_config_devices(tx, now, &spec.id, &declared)?;
                }
            }
            Ok(())
        })?;
        let (specs, devices) = self.store.with(|conn| -> rusqlite::Result<_> {
            Ok((inventory::instance_specs(conn)?, inventory::stored_devices(conn)?))
        })?;
        self.registry.load(specs, devices);
        Ok(())
    }

    /// A stored host whose files have gone away, or that the configuration no
    /// longer allows, is unavailable. The rest of the console keeps working.
    fn load_further_hosts(&self) -> rusqlite::Result<()> {
        let (networks, files) = actions::host_rules(self);
        for host in self.store.with(|conn| inventory::stored_hosts(conn, &networks, files.as_deref()))? {
            if let Err(reason) = self.registry.add_host(&host) {
                tracing::warn!(host = %host.id, %reason, "a stored host could not be loaded");
            }
        }
        Ok(())
    }

    pub fn limits(&self) -> Limits {
        Limits { absolute_ms: self.config.session_absolute_ms(), idle_ms: self.config.session_idle_ms() }
    }

    /// Appends to the audit log. Use it inside the transaction doing the work.
    pub fn append_audit(&self, conn: &Connection, event: &Event) -> rusqlite::Result<i64> {
        audit::append(conn, event, self.audit_key.as_ref())
    }

    pub fn audit(&self, event: Event) -> Result<(), ApiError> {
        self.store.with(|conn| self.append_audit(conn, &event))?;
        Ok(())
    }

    pub fn verify_audit(&self, conn: &Connection) -> rusqlite::Result<VerifyReport> {
        audit::verify_with(conn, VerifyOptions { key: self.audit_key.as_ref(), ..Default::default() })
    }

    /// Records a refusal that needs no secret to trigger, from someone without a
    /// session. Capped, so anonymous requests cannot grow the log without limit.
    /// What the cap keeps out is counted, and the count is written once the
    /// minute is over. Refusals that follow a valid signature do not come here.
    pub fn audit_anonymous_refusal(&self, event: Event) -> Result<(), ApiError> {
        self.store.tx(|tx| -> rusqlite::Result<()> {
            self.flush_dropped_refusals(tx, event.ts)?;
            let recent: i64 = tx.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE session_ref = '-' AND ts > ?1",
                [event.ts - MINUTE_MS],
                |row| row.get(0),
            )?;
            if recent < MAX_ANONYMOUS_REFUSALS_PER_MINUTE {
                self.append_audit(tx, &event)?;
            } else {
                count_dropped_refusal(tx, event.ts)?;
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Writes the roll-up row for a minute that is over, if refusals were dropped
    /// in it. One row a minute at most, however heavy the flood.
    pub fn flush_dropped_refusals(&self, conn: &Connection, now: i64) -> rusqlite::Result<()> {
        let Some((minute, dropped)) = dropped_refusals(conn)? else {
            return Ok(());
        };
        if minute == now.div_euclid(MINUTE_MS) {
            return Ok(());
        }
        self.append_audit(
            conn,
            &Event {
                ts: now,
                action: DROPPED_REFUSALS_ACTION.to_string(),
                target: "-".to_string(),
                params: json!({ "dropped": dropped, "minute_starting": minute * MINUTE_MS }),
                credential_ref: "-".to_string(),
                session_ref: "-".to_string(),
                user_ref: "-".to_string(),
                device: "-".to_string(),
                channel: "panel".to_string(),
                step_up: "none".to_string(),
                result: format!("dropped: {dropped}"),
                rtt_ms: None,
                run_id: None,
            },
        )?;
        conn.execute("DELETE FROM meta WHERE key = ?1", params![DROPPED_REFUSALS_KEY])?;
        Ok(())
    }
}

fn dropped_refusals(conn: &Connection) -> rusqlite::Result<Option<(i64, i64)>> {
    let stored: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![DROPPED_REFUSALS_KEY], |row| row.get(0))
        .optional()?;
    Ok(stored.and_then(|text| {
        let (minute, dropped) = text.split_once(':')?;
        Some((minute.parse().ok()?, dropped.parse().ok()?))
    }))
}

fn count_dropped_refusal(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    let minute = now.div_euclid(MINUTE_MS);
    let dropped = match dropped_refusals(conn)? {
        Some((stored, dropped)) if stored == minute => dropped + 1,
        _ => 1,
    };
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![DROPPED_REFUSALS_KEY, format!("{minute}:{dropped}")],
    )?;
    Ok(())
}

/// The single owner's stable WebAuthn user handle.
fn owner_id(store: &Store) -> rusqlite::Result<Uuid> {
    store.with(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('owner_id', ?1)",
            [Uuid::from_bytes(random_bytes::<16>()).to_string()],
        )?;
        let text: String = conn.query_row("SELECT value FROM meta WHERE key = 'owner_id'", [], |row| row.get(0))?;
        text.parse().map_err(|_| rusqlite::Error::InvalidQuery)
    })
}

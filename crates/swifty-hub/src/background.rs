//! What the service does on its own while it runs. The sampling, the scan and the
//! health poll go through the registry, which contacts nothing in dry-run, and the
//! weekly review only counts what the store already holds. None of them can change
//! a device. Watching for instances another process changed runs in dry-run too: it
//! reads only the store.

use std::time::Duration;

use crate::app::App;
use crate::error::ApiError;
use crate::history;
use crate::inventory;
use crate::memory;

const SAMPLE_EVERY: Duration = Duration::from_secs(60);
const SCAN_EVERY: Duration = Duration::from_secs(300);
const HEALTH_EVERY: Duration = Duration::from_secs(60);
/// How often to see whether another process changed the instances. The CLI's
/// changes reach a running hub within this and a moment.
const WATCH_EVERY: Duration = Duration::from_secs(1);
/// How often to see whether a week has passed since Swifty's last review.
const REVIEW_EVERY: Duration = Duration::from_secs(3600);

/// Asks every instance that hosts machines which exist, and lists any new one as
/// pending under the instance that reported it. Returns what was added, or `None`
/// in dry-run.
pub async fn scan_machines(app: &App) -> Result<Option<Vec<String>>, swifty_plugin_api::DeviceError> {
    let Some(seen) = app.registry.list_machines().await? else {
        return Ok(None);
    };
    let now = app.now();
    let added = app
        .store
        .tx(|tx| -> Result<Vec<String>, ApiError> {
            let mut added = Vec::new();
            for (instance, machines) in &seen {
                added.extend(inventory::record_seen(tx, now, instance, machines)?);
            }
            Ok(added)
        })
        .map_err(|error| swifty_plugin_api::DeviceError::Disabled(error.message))?;
    // A pending machine is in the index too, under the name Proxmox gives it.
    app.sync_directory().map_err(|error| swifty_plugin_api::DeviceError::Disabled(ApiError::from(error).message))?;
    Ok(Some(added))
}

/// Starts the periodic jobs. Nothing is started in dry-run.
pub fn start(app: App) {
    if !app.registry.is_live() {
        return;
    }
    let sampler = app.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SAMPLE_EVERY);
        loop {
            interval.tick().await;
            if let Err(error) = history::sample_collector(&sampler).await {
                tracing::warn!(message = %error.message, "could not store a collector sample");
            }
        }
    });
    let scanner = app.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SCAN_EVERY);
        loop {
            interval.tick().await;
            if let Err(error) = scan_machines(&scanner).await {
                tracing::warn!(%error, "could not list the machines");
            }
        }
    });
    let poller = app.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(HEALTH_EVERY);
        loop {
            interval.tick().await;
            poller.registry.poll_health().await;
        }
    });
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REVIEW_EVERY);
        loop {
            interval.tick().await;
            if let Err(error) = weekly_review(&app) {
                tracing::warn!(message = %error.message, "could not write Swifty's weekly review");
            }
        }
    });
}

/// Loads the instances again whenever another process (the CLI) changed them, which
/// it says by moving `instances_rev` in the store.
pub fn watch_instances(app: App) {
    tokio::spawn(async move {
        let mut seen = app.store.with(inventory::instances_rev).unwrap_or_default();
        let mut interval = tokio::time::interval(WATCH_EVERY);
        loop {
            interval.tick().await;
            if let Err(error) = reload_if_moved(&app, &mut seen) {
                tracing::warn!(%error, "could not load the changed integrations");
            }
        }
    });
}

/// Loads the instances again if `instances_rev` moved since `seen`. Says whether it did.
pub fn reload_if_moved(app: &App, seen: &mut i64) -> rusqlite::Result<bool> {
    let rev = app.store.with(inventory::instances_rev)?;
    if rev == *seen {
        return Ok(false);
    }
    app.sync_directory()?;
    *seen = rev;
    Ok(true)
}

/// Once a week Swifty's review goes into History: how many commands it learned, worked
/// out, did not understand, and was told it got wrong. Counts only, never the words.
/// A quiet week writes nothing. Returns whether a review was written.
pub fn weekly_review(app: &App) -> Result<bool, ApiError> {
    let now = app.now();
    app.store.tx(|tx| -> Result<bool, ApiError> {
        if !memory::review_due(tx, now)? {
            return Ok(false);
        }
        memory::mark_reviewed(tx, now)?;
        let review = memory::review(tx, now)?;
        if review.is_empty() {
            return Ok(false);
        }
        let event = crate::http::plain_event(app, "swifty.review", "-", None, &review.line());
        app.append_audit(tx, &event)?;
        Ok(true)
    })
}

//! Console sessions on approved VMs.
//!
//! Getting one takes a tier 3 step-up on `console.open`, whose signature commits to
//! that VM. That produces a ticket: random, single-use, good for a few seconds,
//! bound to the session that asked, and kept only in memory. The browser presents
//! it when opening the websocket, in `Sec-WebSocket-Protocol` so it stays out of
//! URLs and access logs.
//!
//! The relay carries one VM's VNC bytes and nothing else. It stops as soon as the
//! session it belongs to stops being able to act (locked, ended, expired, or its
//! passkey revoked) or its VM comes off the approved list.
//!
//! Which VMs have a console is the plugin's to say: a VM it gives the `Console`
//! capability has one, and the catalog admits no other.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::http::StatusCode;
use serde_json::json;
use swifty_net::ConsoleLink;
use swifty_plugin_api::{CapKind, ConsoleGrant};

use crate::app::App;
use crate::audit::Event;
use crate::crypto::{random_token, sha256_hex};
use crate::error::ApiError;
use crate::inventory::{self, MachineStatus};
use crate::sessions::{self, Session};

/// How long a ticket lasts. Proxmox's own VNC proxy only waits a few seconds.
pub const TICKET_TTL_MS: i64 = 15_000;
/// The websocket subprotocol prefix a ticket travels under.
pub const TICKET_PROTOCOL_PREFIX: &str = "swifty-ticket.";
pub const MAX_OPEN_CONSOLES: usize = 4;
const MAX_PENDING_TICKETS: usize = 16;
/// How often a running console checks that it may carry on.
const SESSION_CHECK: Duration = Duration::from_secs(1);
/// The largest message taken from the browser. VNC input is tiny.
pub const MAX_BROWSER_MESSAGE: usize = 256 * 1024;
/// A side that will not take bytes for this long has gone. Without a limit, a
/// stuck socket would keep the loop from noticing the session has ended.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
struct Pending {
    session_ref: String,
    /// The VM, as the hub names it.
    target: String,
    /// The instance that granted it, which also opens its socket.
    instance: String,
    /// What the plugin gave. Its own target is the plugin's local id.
    grant: ConsoleGrant,
    expires_at: i64,
}

/// A console ticket that was spent: what it opens.
#[derive(Debug)]
pub struct Redeemed {
    pub target: String,
    pub instance: String,
    pub grant: ConsoleGrant,
}

/// The tickets waiting to be used and the consoles that are open.
#[derive(Default)]
pub struct ConsoleBroker {
    pending: Mutex<HashMap<String, Pending>>,
    open: Arc<AtomicUsize>,
}

/// A place among the open consoles, given back when dropped.
pub struct OpenSlot(Arc<AtomicUsize>);

impl Drop for OpenSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ConsoleBroker {
    /// Issues a ticket for a grant from the instance that carries `target`. Only its
    /// hash is kept.
    pub fn issue(
        &self,
        now: i64,
        session_ref: &str,
        target: &str,
        instance: &str,
        grant: ConsoleGrant,
    ) -> Result<(String, i64), ApiError> {
        let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.retain(|_, ticket| ticket.expires_at > now);
        if pending.len() >= MAX_PENDING_TICKETS {
            return Err(busy());
        }
        let ticket = random_token();
        let expires_at = now + TICKET_TTL_MS;
        let entry = Pending {
            session_ref: session_ref.to_string(),
            target: target.to_string(),
            instance: instance.to_string(),
            grant,
            expires_at,
        };
        pending.insert(sha256_hex(ticket.as_bytes()), entry);
        Ok((ticket, expires_at))
    }

    /// Spends a ticket. Whatever the outcome it cannot be presented again, because
    /// it is removed before anything about it is checked.
    pub fn redeem(&self, now: i64, session_ref: &str, ticket: &str) -> Result<Redeemed, ApiError> {
        let found =
            self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&sha256_hex(ticket.as_bytes()));
        let Some(found) = found else {
            return Err(invalid_ticket());
        };
        if now >= found.expires_at || found.session_ref != session_ref {
            return Err(invalid_ticket());
        }
        Ok(Redeemed { target: found.target, instance: found.instance, grant: found.grant })
    }

    pub fn try_open(&self) -> Result<OpenSlot, ApiError> {
        let taken = self.open.fetch_add(1, Ordering::SeqCst);
        let slot = OpenSlot(self.open.clone());
        if taken >= MAX_OPEN_CONSOLES {
            return Err(busy());
        }
        Ok(slot)
    }
}

fn busy() -> ApiError {
    ApiError::new(StatusCode::TOO_MANY_REQUESTS, "console_busy", "Too many consoles are open. Close one first.")
}

fn invalid_ticket() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "invalid_console_ticket", "That console ticket is not valid.")
}

/// Why a VM on the approved list has no console, if it does not: its plugin gives
/// it no `Console` capability.
pub fn not_available(app: &App, target: &str) -> Option<ApiError> {
    if app.registry.index().has(target, CapKind::Console) {
        return None;
    }
    Some(ApiError::new(StatusCode::CONFLICT, "console_not_available", "This VM is not on the console list."))
}

/// The ticket in a `Sec-WebSocket-Protocol` header.
pub fn ticket_from_protocols(header: &str) -> Option<&str> {
    header.split(',').find_map(|protocol| protocol.trim().strip_prefix(TICKET_PROTOCOL_PREFIX))
}

fn console_event(app: &App, session: &Session, action: &str, target: &str, result: &str) -> Event {
    Event {
        ts: app.now(),
        action: action.to_string(),
        target: target.to_string(),
        params: json!({}),
        credential_ref: session.credential_id.clone(),
        session_ref: session.session_ref.clone(),
        user_ref: session.person.user_id.clone(),
        device: session.device.clone(),
        channel: "panel".to_string(),
        step_up: "ticket".to_string(),
        result: result.to_string(),
        rtt_ms: None,
        run_id: None,
    }
}

pub fn record(app: &App, session: &Session, action: &str, target: &str, result: &str) -> Result<(), ApiError> {
    app.audit(console_event(app, session, action, target, result))
}

/// Why a running console must stop, if it must. Typing at the console counts as
/// activity, so someone working in it does not reach the idle limit.
fn reason_to_stop(app: &App, session: &Session, target: &str, was_active: bool) -> Option<&'static str> {
    let now = app.now();
    let limits = app.limits();
    let checked = app.store.with(|conn| -> rusqlite::Result<Option<&'static str>> {
        if was_active {
            sessions::touch(conn, now, &session.session_ref)?;
        }
        if !sessions::is_live(conn, now, limits, &session.session_ref)? {
            return Ok(Some("session_ended"));
        }
        let approved = inventory::machine_status(conn, target)? == Some(MachineStatus::Approved);
        Ok((!approved).then_some("machine_removed"))
    });
    // A store that cannot answer is no reason to leave a console open.
    checked.unwrap_or(Some("session_ended"))
}

/// Carries bytes both ways until either side closes, the session stops being able
/// to act or the VM comes off the approved list, and writes down how it ended.
pub async fn relay(
    app: App,
    session: Session,
    target: String,
    mut browser: WebSocket,
    mut vm: ConsoleLink,
    slot: OpenSlot,
) {
    let mut check = tokio::time::interval(SESSION_CHECK);
    let mut was_active = false;
    let reason = loop {
        tokio::select! {
            frame = browser.recv() => match frame {
                Some(Ok(Message::Binary(data))) => {
                    was_active = true;
                    if !matches!(tokio::time::timeout(WRITE_TIMEOUT, vm.send(data.to_vec())).await, Ok(Ok(()))) {
                        break "vm_closed";
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break "browser_closed",
                Some(Ok(_)) => {}
            },
            frame = vm.recv() => match frame {
                Some(data) => {
                    if !matches!(tokio::time::timeout(WRITE_TIMEOUT, browser.send(Message::Binary(data.into()))).await, Ok(Ok(()))) {
                        break "browser_closed";
                    }
                }
                None => break "vm_closed",
            },
            _ = check.tick() => {
                if let Some(reason) = reason_to_stop(&app, &session, &target, was_active) {
                    break reason;
                }
                was_active = false;
            }
        }
    };
    vm.close().await;
    let _ = tokio::time::timeout(Duration::from_secs(1), browser.send(Message::Close(None))).await;
    drop(slot);
    if let Err(error) = record(&app, &session, "console.disconnect", &target, reason) {
        tracing::error!(message = %error.message, "could not record the end of a console");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swifty_plugin_api::Secret;

    fn grant() -> ConsoleGrant {
        ConsoleGrant { target: "vm102".to_string(), port: 5900, ticket: Secret::new("pve".to_string()) }
    }

    fn issue(broker: &ConsoleBroker, now: i64, session: &str) -> Result<(String, i64), ApiError> {
        broker.issue(now, session, "vm102", "proxmox", grant())
    }

    #[test]
    fn a_ticket_works_once() {
        let broker = ConsoleBroker::default();
        let (ticket, _) = issue(&broker, 1_000, "s1").unwrap();
        let redeemed = broker.redeem(1_001, "s1", &ticket).unwrap();
        assert_eq!((redeemed.target.as_str(), redeemed.instance.as_str()), ("vm102", "proxmox"));
        assert_eq!(broker.redeem(1_002, "s1", &ticket).unwrap_err().code, "invalid_console_ticket");
    }

    #[test]
    fn a_ticket_expires_and_is_spent_by_a_late_attempt() {
        let broker = ConsoleBroker::default();
        let (ticket, expires_at) = issue(&broker, 1_000, "s1").unwrap();
        assert_eq!(expires_at, 1_000 + TICKET_TTL_MS);
        assert!(broker.redeem(expires_at, "s1", &ticket).is_err());
        assert!(broker.redeem(1_001, "s1", &ticket).is_err());
    }

    #[test]
    fn a_ticket_is_spent_by_the_wrong_session_too() {
        let broker = ConsoleBroker::default();
        let (ticket, _) = issue(&broker, 1_000, "s1").unwrap();
        assert!(broker.redeem(1_001, "s2", &ticket).is_err());
        assert!(broker.redeem(1_002, "s1", &ticket).is_err(), "a wrong attempt must use it up");
    }

    #[test]
    fn unused_tickets_cannot_pile_up() {
        let broker = ConsoleBroker::default();
        for _ in 0..MAX_PENDING_TICKETS {
            issue(&broker, 1_000, "s").unwrap();
        }
        assert_eq!(issue(&broker, 1_001, "s").unwrap_err().code, "console_busy");
        assert!(issue(&broker, 1_000 + TICKET_TTL_MS, "s").is_ok(), "expired ones make room");
    }

    #[test]
    fn open_consoles_are_capped_and_slots_come_back() {
        let broker = ConsoleBroker::default();
        let mut slots: Vec<OpenSlot> = (0..MAX_OPEN_CONSOLES).map(|_| broker.try_open().unwrap()).collect();
        assert_eq!(broker.try_open().err().unwrap().code, "console_busy");
        slots.pop();
        assert!(broker.try_open().is_ok());
    }

    #[test]
    fn the_ticket_is_found_among_the_protocols() {
        assert_eq!(ticket_from_protocols("binary, swifty-ticket.abc"), Some("abc"));
        assert_eq!(ticket_from_protocols("swifty-ticket.abc,binary"), Some("abc"));
        assert_eq!(ticket_from_protocols("binary"), None);
    }
}

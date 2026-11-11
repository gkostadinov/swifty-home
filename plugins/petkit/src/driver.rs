//! The live side: the login, the readings and the two commands, over HTTPS through
//! `swifty-net`.
//!
//! The login is the account name and the MD5 of its sign-in secret, from the secret slot
//! `login`, and never repeated: every error here is fixed text and at most a number, so
//! neither the account, the hash, the session nor anything Petkit sent can reach a log or
//! a result.
//!
//! Nothing is retried that could be sent twice. A feed or a clean that gets no answer is
//! reported as silence and left to the engine, which sends a non-idempotent action once.
//! A session Petkit has signed out is replaced before a read is tried again, but a command
//! that finds it signed out is not resent. Cloud calls are rationed: a reading is kept for
//! `poll_secs`, or a few seconds while a command is being watched, and a failed login is
//! not tried again for half a minute, because the engine reads every 250 ms while it waits.
//! A call that gets no answer, or an answer that cannot be read, drops every kept
//! reading: what Petkit said before is not passed off as what the devices say now.
//! Every socket takes the hub's timeouts as they are given.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use swifty_net::{Http, LiveGrant, Timeouts};
use swifty_plugin_api::{CommandAck, DeviceError, Driver, Op, Secret, State};
use tokio::sync::Mutex;
use url::Url;

use crate::config::PetkitConfig;
use crate::protocol::{
    accepted_gateway, client_info, explain, feeder_portions, finished_cleanings, header_pairs, id_of,
    latest_completion, litter_busy, local_day, local_hhmm, now_secs, open, unreadable, Login, Wire, CHINA_GATEWAY,
    FEEDER, FEEDER_TYPE, LITTER, LITTER_TYPE, LOGIN_REFUSED, LOGIN_SECRET_FIELD,
};
use crate::refusal;

/// A login that failed is not repeated for this long.
const LOGIN_PAUSE: Duration = Duration::from_secs(30);
/// A reading that failed is not asked for again for this long.
const FAILURE_PAUSE: Duration = Duration::from_secs(5);
/// While a command is being watched, readings are this fresh.
const WATCH_EVERY: Duration = Duration::from_secs(4);
const WATCH_FOR: Duration = Duration::from_secs(90);
/// A device that was not on the account is looked for again no sooner than this.
const LOOK_AGAIN: Duration = Duration::from_secs(600);
/// Used when Petkit does not say how long a session lasts.
const SESSION_FALLBACK_SECS: u64 = 3_600;
const MAX_BODY: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Feeder,
    Litter,
}

impl Which {
    fn of(target: &str) -> Result<Self, DeviceError> {
        match target {
            FEEDER => Ok(Self::Feeder),
            LITTER => Ok(Self::Litter),
            other => Err(DeviceError::Disabled(format!("{other} is not a Petkit device."))),
        }
    }
}

/// The ids of the devices this instance uses, as far as they are known.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Found {
    pub feeder: Option<u64>,
    pub litter: Option<u64>,
}

struct Session {
    id: Secret,
    gateway: Http,
    expires: Instant,
}

enum Reading {
    Fresh(Instant, State),
    Failed(Instant, DeviceError),
}

#[derive(Default)]
struct Inner {
    session: Option<Session>,
    login_pause: Option<Instant>,
    login_refused: bool,
    found: Option<(Found, Instant)>,
    feeder: Option<Reading>,
    litter: Option<Reading>,
    watching: Option<Instant>,
}

impl Inner {
    fn slot(&mut self, which: Which) -> &mut Option<Reading> {
        match which {
            Which::Feeder => &mut self.feeder,
            Which::Litter => &mut self.litter,
        }
    }

    fn watched(&self) -> bool {
        self.watching.is_some_and(|until| Instant::now() < until)
    }

    /// Petkit just failed to answer, or answered with something that could not
    /// be read: the readings it gave before can no longer be passed off as what
    /// the devices say now, so the next read asks again.
    fn forget_readings(&mut self) {
        for slot in [&mut self.feeder, &mut self.litter] {
            if matches!(slot, Some(Reading::Fresh(..))) {
                *slot = None;
            }
        }
    }
}

/// One call to Petkit, described before it is sent.
struct Call {
    post: bool,
    path: &'static str,
    query: Vec<(&'static str, String)>,
    form: Vec<(&'static str, String)>,
}

impl Call {
    fn get(path: &'static str) -> Self {
        Self { post: false, path, query: Vec::new(), form: Vec::new() }
    }

    fn post(path: &'static str) -> Self {
        Self { post: true, path, query: Vec::new(), form: Vec::new() }
    }

    fn query(mut self, key: &'static str, value: impl ToString) -> Self {
        self.query.push((key, value.to_string()));
        self
    }

    fn form(mut self, key: &'static str, value: impl ToString) -> Self {
        self.form.push((key, value.to_string()));
        self
    }
}

/// The feeder and the litter box of one instance.
pub struct PetkitDriver {
    grant: LiveGrant,
    config: PetkitConfig,
    login: Login,
    passport_url: Url,
    passport: Http,
    timeouts: Timeouts,
    /// Held for a whole call, so one login serves every caller and Petkit never sees two
    /// requests from here at once.
    inner: Mutex<Inner>,
}

impl PetkitDriver {
    /// Builds clients and opens nothing.
    pub fn new(grant: LiveGrant, config: &PetkitConfig, login: Login, timeouts: Timeouts) -> Result<Self, String> {
        let passport_url =
            Url::parse(&config.passport_url).map_err(|_| "the Petkit passport URL is not valid".to_string())?;
        let passport = Http::new(grant, &config.passport_url, timeouts, None)?.with_max_body(MAX_BODY);
        Ok(Self {
            grant,
            config: config.clone(),
            login,
            passport_url,
            passport,
            timeouts,
            inner: Mutex::new(Inner::default()),
        })
    }

    fn headers(&self, session: Option<&Secret>) -> Result<HeaderMap, DeviceError> {
        let bad = || DeviceError::Disabled("a Petkit header is not valid".to_string());
        let mut map = HeaderMap::new();
        for (name, value) in header_pairs(&self.config, session) {
            map.insert(HeaderName::from_static(name), HeaderValue::from_str(&value).map_err(|_| bad())?);
        }
        Ok(map)
    }

    /// Petkit's server for this account's region, and the region's name as Petkit spells it.
    async fn gateway_for_region(&self) -> Result<(String, String), Wire> {
        let region = self.config.region.trim().to_ascii_lowercase();
        if matches!(region.as_str(), "china" | "cn") {
            return Ok((CHINA_GATEWAY.to_string(), region));
        }
        let url = self.passport.url("v1/regionservers").map_err(Wire::Fault)?;
        let request = self.passport.get(url).headers(self.headers(None).map_err(Wire::Fault)?);
        let result = open(&self.passport.send(request).await.map_err(Wire::Fault)?)?;
        let list = result.get("list").and_then(Value::as_array).ok_or(Wire::Fault(unreadable()))?;
        for server in list {
            let text = |key: &str| server.get(key).and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase();
            if text("name") == region || text("id") == region {
                let gateway = server.get("gateway").and_then(Value::as_str).unwrap_or_default();
                return accepted_gateway(gateway, &self.passport_url).map(|url| (url, text("id"))).ok_or_else(|| {
                    Wire::Fault(DeviceError::Rejected(
                        "Petkit named a server for this region that this service will not log in to".to_string(),
                    ))
                });
            }
        }
        Err(Wire::Fault(DeviceError::Rejected("Petkit has no server for this region".to_string())))
    }

    async fn sign_in(&self) -> Result<Session, Wire> {
        let (gateway_url, region) = self.gateway_for_region().await?;
        let gateway = Http::new(self.grant, &gateway_url, self.timeouts, None)
            .map_err(|_| Wire::Fault(DeviceError::Disabled("the Petkit server address is not valid".to_string())))?
            .with_max_body(MAX_BODY);
        let url = gateway.url("user/login").map_err(Wire::Fault)?;
        let form = [
            ("oldVersion", self.config.app_version.clone()),
            ("client", client_info(&self.config)),
            ("encrypt", "1".to_string()),
            ("region", region),
            ("username", self.login.username.clone()),
            (LOGIN_SECRET_FIELD, self.login.md5.expose().to_string()),
        ];
        let request = gateway.post(url).headers(self.headers(None).map_err(Wire::Fault)?).form(&form);
        let result = open(&gateway.send(request).await.map_err(Wire::Fault)?)?;
        let session = result.get("session").ok_or(Wire::Fault(unreadable()))?;
        let id = session
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 256 && id.bytes().all(|b| b.is_ascii_graphic()))
            .ok_or(Wire::Fault(unreadable()))?;
        let lasts = session.get("expiresIn").and_then(Value::as_u64).unwrap_or(SESSION_FALLBACK_SECS);
        let expires = Instant::now() + Duration::from_secs(lasts.clamp(120, 30 * 86_400) - 60);
        Ok(Session { id: Secret::new(id), gateway, expires })
    }

    async fn ensure_session(&self, inner: &mut Inner) -> Result<(), DeviceError> {
        if inner.session.as_ref().is_some_and(|session| Instant::now() < session.expires) {
            return Ok(());
        }
        inner.session = None;
        if inner.login_refused {
            return Err(DeviceError::Rejected(LOGIN_REFUSED.to_string()));
        }
        if inner.login_pause.is_some_and(|until| Instant::now() < until) {
            return Err(DeviceError::Unreachable(
                "Petkit's login failed a moment ago, so it is not being tried again yet".to_string(),
            ));
        }
        match self.sign_in().await {
            Ok(session) => {
                inner.session = Some(session);
                inner.login_pause = None;
                Ok(())
            }
            Err(Wire::Code(122 | 125)) => {
                inner.login_refused = true;
                Err(DeviceError::Rejected(LOGIN_REFUSED.to_string()))
            }
            Err(failure) => {
                inner.login_pause = Some(Instant::now() + LOGIN_PAUSE);
                Err(match failure {
                    Wire::Code(code) => explain(code),
                    Wire::Fault(error) => error,
                })
            }
        }
    }

    /// Sends one call on the current session.
    async fn once(&self, inner: &mut Inner, call: &Call) -> Result<Value, Wire> {
        self.ensure_session(inner).await.map_err(Wire::Fault)?;
        let session = inner.session.as_ref().ok_or(Wire::Fault(unreadable()))?;
        let url = session.gateway.url(call.path).map_err(Wire::Fault)?;
        let mut request = if call.post { session.gateway.post(url) } else { session.gateway.get(url) };
        request = request.headers(self.headers(Some(&session.id)).map_err(Wire::Fault)?);
        if !call.query.is_empty() {
            request = request.query(&call.query);
        }
        if !call.form.is_empty() {
            request = request.form(&call.form);
        }
        open(&session.gateway.send(request).await.map_err(Wire::Fault)?)
    }

    /// A call that may be asked again. If Petkit has signed the session out, a new one is
    /// made and the call is made once more.
    async fn read(&self, inner: &mut Inner, call: &Call) -> Result<Value, DeviceError> {
        for attempt in 0..2 {
            match self.once(inner, call).await {
                Ok(value) => return Ok(value),
                Err(Wire::Code(5)) => {
                    inner.session = None;
                    if attempt == 1 {
                        return Err(explain(5));
                    }
                }
                Err(Wire::Code(code)) => return Err(explain(code)),
                Err(Wire::Fault(error)) => return Err(error),
            }
        }
        Err(explain(5))
    }

    /// A call that must not be sent twice. A session found signed out is dropped and
    /// reported, and the call is not made again.
    async fn send_once(&self, inner: &mut Inner, call: &Call) -> Result<Value, DeviceError> {
        match self.once(inner, call).await {
            Ok(value) => Ok(value),
            Err(Wire::Code(5)) => {
                inner.session = None;
                Err(explain(5))
            }
            Err(Wire::Code(code)) => Err(explain(code)),
            Err(Wire::Fault(error)) => Err(error),
        }
    }

    /// Every device on the account and shared to it, as its id and its type in lower case.
    async fn listed_devices(&self, inner: &mut Inner) -> Result<Vec<(u64, String)>, DeviceError> {
        let families = self.read(inner, &Call::get("group/family/list")).await?;
        let mut listed: Vec<(u64, String)> = Vec::new();
        let mut empty_groups = Vec::new();
        for account in families.as_array().into_iter().flatten() {
            let devices = account.get("deviceList").and_then(Value::as_array).filter(|devices| !devices.is_empty());
            match devices {
                Some(devices) => {
                    for device in devices {
                        let kind =
                            device.get("deviceType").and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase();
                        if let Some(id) = device.get("deviceId").and_then(id_of) {
                            listed.push((id, kind));
                        }
                    }
                }
                None => empty_groups.extend(account.get("groupId").and_then(id_of)),
            }
        }
        // Devices shared to this account are not in its own list. Petkit has them in a roster.
        for group in empty_groups {
            let call = Call::post("discovery/device_roster_v2")
                .form("day", local_day(now_secs(), self.config.utc_offset_minutes))
                .form("groupId", group);
            match self.read(inner, &call).await {
                Ok(roster) => {
                    for device in roster.get("devices").and_then(Value::as_array).into_iter().flatten() {
                        let kind = device.get("type").and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase();
                        if let Some(id) = device.get("id").and_then(id_of) {
                            listed.push((id, kind));
                        }
                    }
                }
                Err(error) if error.is_silence() => return Err(error),
                Err(_) => {}
            }
        }
        Ok(listed)
    }

    async fn look_for_devices(&self, inner: &mut Inner) -> Result<Found, DeviceError> {
        let listed = self.listed_devices(inner).await?;
        let first = |kind: &str| listed.iter().find(|(_, found)| found == kind).map(|(id, _)| *id);
        Ok(Found {
            feeder: self.config.feeder_id.or_else(|| first(FEEDER_TYPE)),
            litter: self.config.litter_id.or_else(|| first(LITTER_TYPE)),
        })
    }

    /// The feeders and litter boxes on the account, as id and type (`d4` or `t4`). For the setup.
    pub async fn devices_on_account(&self) -> Result<Vec<(u64, String)>, DeviceError> {
        let mut inner = self.inner.lock().await;
        let mut listed = self.listed_devices(&mut inner).await?;
        listed.retain(|(_, kind)| kind == FEEDER_TYPE || kind == LITTER_TYPE);
        let mut seen = std::collections::BTreeSet::new();
        listed.retain(|(id, _)| seen.insert(*id));
        Ok(listed)
    }

    /// Uses these devices from now on, without asking Petkit for the list again. For the setup.
    pub async fn pin(&self, found: Found) {
        self.inner.lock().await.found = Some((found, Instant::now()));
    }

    async fn device_id(&self, inner: &mut Inner, which: Which) -> Result<u64, DeviceError> {
        let known = inner
            .found
            .filter(|(found, at)| (found.feeder.is_some() && found.litter.is_some()) || at.elapsed() < LOOK_AGAIN);
        let found = match known {
            Some((found, _)) => found,
            None => {
                let found = self.look_for_devices(inner).await?;
                inner.found = Some((found, Instant::now()));
                found
            }
        };
        match which {
            Which::Feeder => found.feeder.ok_or_else(|| {
                DeviceError::Disabled(
                    "No Petkit D4 feeder was found on this account. Share it to this account in the Petkit app."
                        .to_string(),
                )
            }),
            Which::Litter => found.litter.ok_or_else(|| {
                DeviceError::Disabled(
                    "No Petkit T4 litter box was found on this account. Share it to this account in the Petkit app."
                        .to_string(),
                )
            }),
        }
    }

    async fn fetch_feeder(&self, inner: &mut Inner, id: u64) -> Result<State, DeviceError> {
        let offset = self.config.utc_offset_minutes;
        let detail = self.read(inner, &Call::post("d4/device_detail").query("id", id)).await?;
        let portions = feeder_portions(&detail)?;
        // The time of the last feed is a nicety. Its failure does not hide the reading.
        let records = Call::post("d4/dailyFeeds").query("days", local_day(now_secs(), offset)).query("deviceId", id);
        let last = match self.read(inner, &records).await {
            Ok(records) => latest_completion(&records).map(|epoch| local_hhmm(epoch, offset)),
            Err(error) if error.is_silence() => return Err(error),
            Err(_) => None,
        };
        Ok(State::Feeder { last, portions })
    }

    async fn fetch_litter(&self, inner: &mut Inner, id: u64) -> Result<State, DeviceError> {
        let offset = self.config.utc_offset_minutes;
        let detail = self.read(inner, &Call::post("t4/device_detail").query("id", id)).await?;
        let busy = litter_busy(&detail)?;
        let records =
            Call::post("t4/getDeviceRecord").query("date", local_day(now_secs(), offset)).query("deviceId", id);
        let (cycles, last) = match self.read(inner, &records).await {
            Ok(records) => {
                let (cycles, newest) = finished_cleanings(&records);
                (cycles, newest.map(|epoch| local_hhmm(epoch, offset)))
            }
            Err(error) if error.is_silence() => return Err(error),
            Err(_) => (0, None),
        };
        Ok(State::Litter { last, busy, cycles })
    }
}

#[async_trait]
impl Driver for PetkitDriver {
    async fn command(&self, target: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        if let Some(reason) = refusal(Some(&self.config), target, op) {
            return Err(DeviceError::Disabled(reason));
        }
        let which = Which::of(target)?;
        let mut inner = self.inner.lock().await;
        let id = match self.device_id(&mut inner, which).await {
            Ok(id) => id,
            Err(error) => {
                inner.forget_readings();
                return Err(error);
            }
        };
        let call = match op {
            Op::PetFeed { grams } => Call::post("d4/saveDailyFeed")
                .form("day", local_day(now_secs(), self.config.utc_offset_minutes))
                .form("deviceId", id)
                .form("name", "")
                .form("time", "-1")
                .form("amount", grams),
            Op::LitterClean => {
                Call::post("t4/controlDevice").form("id", id).form("kv", r#"{"start_action":0}"#).form("type", "start")
            }
            _ => return Err(DeviceError::Disabled("This Petkit device cannot do that.".to_string())),
        };
        if let Err(error) = self.send_once(&mut inner, &call).await {
            inner.forget_readings();
            return Err(error);
        }
        // What was read before no longer holds, and the next readings are watched closely.
        *inner.slot(which) = None;
        inner.watching = Some(Instant::now() + WATCH_FOR);
        Ok(CommandAck)
    }

    async fn read_state(&self, target: &str) -> Result<State, DeviceError> {
        let which = Which::of(target)?;
        let mut inner = self.inner.lock().await;
        let fresh_for = if inner.watched() { WATCH_EVERY } else { Duration::from_secs(self.config.poll_secs) };
        match inner.slot(which) {
            Some(Reading::Fresh(at, state)) if at.elapsed() < fresh_for => return Ok(state.clone()),
            Some(Reading::Failed(at, error)) if at.elapsed() < FAILURE_PAUSE => return Err(error.clone()),
            _ => {}
        }
        let fetched = match self.device_id(&mut inner, which).await {
            Ok(id) => match which {
                Which::Feeder => self.fetch_feeder(&mut inner, id).await,
                Which::Litter => self.fetch_litter(&mut inner, id).await,
            },
            Err(error) => Err(error),
        };
        if fetched.is_err() {
            inner.forget_readings();
        }
        *inner.slot(which) = Some(match &fetched {
            Ok(state) => Reading::Fresh(Instant::now(), state.clone()),
            Err(error) => Reading::Failed(Instant::now(), error.clone()),
        });
        fetched
    }
}

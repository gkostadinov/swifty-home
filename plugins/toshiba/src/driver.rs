//! The live side: signing in, reading and commanding, all over HTTPS through `swifty-net`.
//!
//! - Signing in: `POST /api/Consumer/Login` gives a bearer token and a consumer id.
//! - Reading: `GET /api/AC/GetCurrentACState` gives the unit's state as hex digits.
//! - Commanding: `POST /api/Consumer/RegisterMobileDevice` gives a shared-access token
//!   for Toshiba's Azure IoT Hub. One command is one device-to-cloud message to that
//!   hub, with the application property `type=mob`.
//!
//! Azure's HTTPS endpoint takes the same message the `toshiba-ac` library sends over
//! AMQP, with no long-lived connection, so the whole driver fits `swifty-net`.
//!
//! Every request to the REST API names this service in a `Device-ID` header, with the
//! same id it registers for commands, as the official app does. Since about July 2026
//! the cloud answers 429, "too many requests", to a sign-in without one, however
//! rarely it comes: clients that do not name themselves seem to share one allowance.
//!
//! The account's sign-in, the bearer token and the hub token are only ever held as a
//! [`Secret`] or a sensitive header. Every error is fixed text and at most a status
//! number, so none of them, and nothing the cloud sent, can reach a result. The log
//! says which request the cloud turned away, its status, how long it asked to be left
//! alone and, when it answered with a refusal, Toshiba's code for it if that is one
//! plain word. Nothing else the cloud sent, and nothing about the account, is logged.
//!
//! A cloud is slower than a device on the LAN and it counts requests. The `toshiba-ac`
//! library reads the state over HTTPS only every half hour and hears changes pushed to
//! it, which this service does not, so a reading here is shared for five minutes.
//! After a command it is looked at again at once and then less and less often, never
//! more than four times. A refused sign-in or a request to slow down stops all asking
//! for a while, at least as long as the cloud asked, and each further request to slow
//! down doubles that wait: a refused account must not be tried again on every poll,
//! and a repeat on 401 is not a repeat of a command, because the cloud said it did not
//! take it. While the cloud is resting (it asked for fewer requests), the last good
//! reading keeps standing for an hour, so the unit does not vanish from the page.
//! When the cloud does not answer, or answers with something that cannot be read,
//! that is said: no reading taken before is passed off as what the unit says now,
//! for any unit on the account, until the cloud has answered again.
//!
//! Every socket takes the hub's timeouts. `timeout_ms` in the config can only
//! shorten the wait for a reply.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use http::header::{AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use http::{HeaderValue, StatusCode};
use serde_json::{json, Value};
use swifty_net::{Http, LiveGrant, Reply, Timeouts};
use swifty_plugin_api::{CommandAck, DeviceError, Driver, Op, Secret, State};
use tokio::sync::Mutex;

use crate::config::{AirconConfig, ToshibaConfig};
use crate::protocol::{
    changes_for, client_id, command_data, listed_units, message_body, percent_encode, sensitive, unreadable, HubToken,
    ListedUnit, Reading, DEVICE_ID, HUB_API_VERSION, LOGIN_PATH, MAPPING_PATH, REGISTER_PATH, STATE_PATH,
};

/// How long a reading is shared when nothing has been asked of the unit lately.
const READING_STAYS_FRESH: Duration = Duration::from_secs(300);
/// After a command the unit is looked at once at once, then after this, and each look
/// after that waits twice as long as the one before: 0, 3, 6 and 12 seconds.
const LOOK_AGAIN_AFTER_COMMAND: Duration = Duration::from_secs(3);
/// How long that closer watching lasts after a command.
const COMMAND_WINDOW: Duration = Duration::from_secs(30);
/// How long the last good reading stands in while the cloud cannot be asked.
const LAST_READING_STANDS: Duration = Duration::from_secs(3600);
/// A failed reading stands for this long, so that polls cannot queue up behind a
/// cloud that has stopped answering.
const FAILED_READING_STANDS: Duration = Duration::from_secs(5);
/// A refused sign-in is not tried again for this long.
const BAD_LOGIN_WAIT: Duration = Duration::from_secs(900);
/// The cloud asked for fewer requests, and gets none for this long. Each time it asks
/// again before an answer gets through, the wait doubles, up to the longest.
const SLOW_DOWN_WAIT: Duration = Duration::from_secs(60);
const SLOW_DOWN_LONGEST: Duration = Duration::from_secs(900);

pub const NO_SIGN_IN: &str = "The stored Toshiba sign-in is missing, or is not a base64 sign-in.";

fn cloud_trouble() -> DeviceError {
    DeviceError::Unreachable("Toshiba's cloud is having trouble".to_string())
}

fn told_to_slow_down() -> DeviceError {
    DeviceError::Unreachable("Toshiba's cloud asked for fewer requests, so it is left alone for a while".to_string())
}

pub fn login_refused() -> DeviceError {
    DeviceError::Rejected(
        "Toshiba refused the sign-in. Run this integration's setup again to check the account".to_string(),
    )
}

/// How long to leave the cloud alone after it asked for fewer requests this many times in a
/// row: never less than it asked for, and never more than the longest wait.
fn slow_down_wait(times: u32, asked: Option<Duration>) -> Duration {
    let ours = SLOW_DOWN_WAIT.saturating_mul(1 << times.saturating_sub(1).min(4));
    ours.max(asked.unwrap_or_default()).min(SLOW_DOWN_LONGEST)
}

/// Writes to the log that the cloud turned a request away: which request, the status and
/// how long the cloud asked to be left alone. Nothing else from the reply is written.
fn log_turned_away(request: &'static str, reply: &Reply) {
    let retry_after = reply.retry_after.map(|wait| wait.as_secs());
    tracing::warn!(request, status = reply.status.as_u16(), ?retry_after, "Toshiba's cloud turned a request away");
}

/// Writes to the log that the cloud answered a request with a refusal, with Toshiba's code
/// for it when that is one plain word, and otherwise without.
fn log_refused(request: &'static str, body: &Value) {
    let code = body
        .get("StatusCode")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty() && code.len() <= 48 && code.bytes().all(|b| b.is_ascii_alphanumeric()))
        .unwrap_or("none");
    tracing::warn!(request, code, "Toshiba's cloud refused a request");
}

/// Writes to the log that a request could not be made. The error is fixed text.
fn log_unreached(request: &'static str, error: &DeviceError) {
    tracing::warn!(request, %error, "Toshiba's cloud could not be reached");
}

#[derive(Clone)]
struct Login {
    authorization: HeaderValue,
    consumer_id: String,
}

#[derive(Default)]
struct Session {
    login: Option<Login>,
    hub: Option<HubToken>,
    /// While this has not passed, nothing is asked of the cloud.
    held_until: Option<(Instant, DeviceError)>,
    /// How many times in a row the cloud has asked for fewer requests.
    slow_downs: u32,
}

impl Session {
    /// The cloud asked for fewer requests: it is left alone, each time for longer.
    fn slow_down(&mut self, asked: Option<Duration>) -> DeviceError {
        self.slow_downs = self.slow_downs.saturating_add(1);
        self.held_until = Some((Instant::now() + slow_down_wait(self.slow_downs, asked), told_to_slow_down()));
        told_to_slow_down()
    }
}

/// A signed-in request to the REST API, described before it is built and sent.
enum Rest<'a> {
    Register { device_id: &'a str, username: &'a str },
    State { ac_id: &'a str },
    Mapping,
}

impl Rest<'_> {
    fn name(&self) -> &'static str {
        match self {
            Rest::Register { .. } => "registration",
            Rest::State { .. } => "reading",
            Rest::Mapping => "listing",
        }
    }
}

/// The sign-in and everything else that is shared by the units on one account.
pub struct Cloud {
    api: Http,
    hub: Http,
    email: Secret,
    sign_in: Secret,
    /// [`client_id`] as a sensitive header, since it holds the account's name.
    device: HeaderValue,
    /// What every request to the REST API says it is.
    user_agent: HeaderValue,
    session: Mutex<Session>,
    /// When the cloud last failed to answer, or answered with something that could not
    /// be read. A reading taken before then is not served again.
    failed_at: std::sync::Mutex<Option<Instant>>,
}

impl Cloud {
    /// `login` is the stored sign-in. Builds clients and opens nothing.
    pub fn new(grant: LiveGrant, config: &ToshibaConfig, login: &str, timeouts: Timeouts) -> Result<Self, String> {
        let (email, sign_in) = crate::protocol::load_credentials(login).ok_or_else(|| NO_SIGN_IN.to_string())?;
        let device = sensitive(&client_id(email.expose(), &config.client_suffix))
            .ok_or_else(|| "The Toshiba account name cannot be sent as a device id.".to_string())?;
        let user_agent = HeaderValue::from_str(&config.user_agent)
            .map_err(|_| "The user agent for Toshiba's cloud cannot be sent as a header.".to_string())?;
        let timeouts =
            Timeouts { connect: timeouts.connect, read: timeouts.read.min(Duration::from_millis(config.timeout_ms)) };
        Ok(Self {
            api: Http::new(grant, &config.api_url, timeouts, None)?,
            hub: Http::new(grant, &config.hub_url, timeouts, None)?,
            email,
            sign_in,
            device,
            user_agent,
            session: Mutex::new(Session::default()),
            failed_at: std::sync::Mutex::new(None),
        })
    }

    /// Takes note that a request failed. Being told to rest, or a refused sign-in,
    /// says nothing about the units, so neither counts.
    fn note_failure(&self, why: &DeviceError) {
        if *why != told_to_slow_down() && *why != login_refused() {
            *self.failed_at.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
        }
    }

    /// Whether a reading taken at `at` came after the cloud last failed.
    fn vouches_for(&self, at: Instant) -> bool {
        self.failed_at.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_none_or(|failed| failed < at)
    }

    async fn slow_down(&self, asked: Option<Duration>) -> DeviceError {
        self.session.lock().await.slow_down(asked)
    }

    /// An answer got through, so the next request to slow down starts from the shortest wait.
    async fn answered(&self) {
        self.session.lock().await.slow_downs = 0;
    }

    /// Sends the sign-in. It looks like a browser and says which client it is.
    async fn send_login(&self) -> Result<Reply, DeviceError> {
        let request = self
            .api
            .post(self.api.url(LOGIN_PATH)?)
            .header(USER_AGENT, self.user_agent.clone())
            .header(DEVICE_ID, self.device.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(self.sign_in.expose().to_string());
        self.api.send(request).await
    }

    /// Sends one signed-in request to the REST API. Like every request there, it looks
    /// like a browser and says which client it is.
    async fn send_rest(&self, call: &Rest<'_>, login: &Login) -> Result<Reply, DeviceError> {
        let request = match call {
            Rest::Register { device_id, username } => self
                .api
                .post(self.api.url(REGISTER_PATH)?)
                .json(&json!({ "DeviceID": device_id, "DeviceType": "1", "Username": username })),
            Rest::State { ac_id } => {
                let mut url = self.api.url(STATE_PATH)?;
                url.query_pairs_mut().append_pair("ACId", ac_id).append_pair("consumerId", &login.consumer_id);
                self.api.get(url)
            }
            Rest::Mapping => {
                let mut url = self.api.url(MAPPING_PATH)?;
                url.query_pairs_mut().append_pair("consumerId", &login.consumer_id);
                self.api.get(url)
            }
        };
        let request = request
            .header(USER_AGENT, self.user_agent.clone())
            .header(DEVICE_ID, self.device.clone())
            .header(AUTHORIZATION, login.authorization.clone());
        self.api.send(request).await
    }

    /// The current sign-in, or a new one. The lock is held throughout, so only one
    /// sign-in is ever under way.
    async fn login(&self) -> Result<Login, DeviceError> {
        let mut session = self.session.lock().await;
        if let Some((until, why)) = &session.held_until {
            if Instant::now() < *until {
                return Err(why.clone());
            }
            session.held_until = None;
        }
        if let Some(login) = &session.login {
            return Ok(login.clone());
        }
        let reply = self.send_login().await.inspect_err(|error| log_unreached("sign-in", error))?;
        let status = reply.status;
        if !status.is_success() {
            log_turned_away("sign-in", &reply);
        }
        if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(session.slow_down(reply.retry_after));
        }
        if status == StatusCode::UNAUTHORIZED {
            session.held_until = Some((Instant::now() + BAD_LOGIN_WAIT, login_refused()));
            return Err(login_refused());
        }
        if !status.is_success() {
            return Err(cloud_trouble());
        }
        let body = reply.json()?;
        if body.get("IsSuccess").and_then(Value::as_bool) != Some(true) {
            log_refused("sign-in", &body);
            session.held_until = Some((Instant::now() + BAD_LOGIN_WAIT, login_refused()));
            return Err(login_refused());
        }
        let result = body.get("ResObj").ok_or_else(unreadable)?;
        let field = |name: &str| result.get(name).and_then(Value::as_str);
        let (token, kind, consumer) = (
            field("access_token").ok_or_else(unreadable)?,
            field("token_type").ok_or_else(unreadable)?,
            field("consumerId").ok_or_else(unreadable)?,
        );
        let login = Login {
            authorization: sensitive(&format!("{kind} {token}")).ok_or_else(unreadable)?,
            consumer_id: consumer.to_string(),
        };
        session.login = Some(login.clone());
        session.slow_downs = 0;
        tracing::info!("signed in to Toshiba's cloud");
        Ok(login)
    }

    /// Stops using this sign-in, unless another has already replaced it.
    async fn forget_login(&self, tried: &Login) {
        let mut session = self.session.lock().await;
        if session.login.as_ref().is_some_and(|login| login.authorization == tried.authorization) {
            session.login = None;
        }
    }

    /// What an answer from the REST API means. Anything but a success is fixed text.
    async fn result_of(&self, request: &'static str, reply: &Reply) -> Result<Value, DeviceError> {
        let status = reply.status;
        if !status.is_success() {
            log_turned_away(request, reply);
        }
        if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(self.slow_down(reply.retry_after).await);
        }
        if status == StatusCode::UNAUTHORIZED {
            return Err(login_refused());
        }
        if status.is_server_error() {
            return Err(cloud_trouble());
        }
        if !status.is_success() {
            return Err(DeviceError::Rejected(format!("Toshiba's cloud answered with status {}", status.as_u16())));
        }
        let body = reply.json()?;
        if body.get("IsSuccess").and_then(Value::as_bool) != Some(true) {
            log_refused(request, &body);
            return Err(DeviceError::Rejected("Toshiba's cloud refused the request".to_string()));
        }
        self.answered().await;
        body.get("ResObj").cloned().ok_or_else(unreadable)
    }

    /// Sends one signed-in request. If the sign-in has gone stale it signs in again
    /// and sends it once more, because the cloud did nothing with the first.
    async fn signed_in(&self, call: Rest<'_>) -> Result<Value, DeviceError> {
        let request = call.name();
        let mut login = self.login().await?;
        let unreached = |error: &DeviceError| log_unreached(request, error);
        let mut reply = self.send_rest(&call, &login).await.inspect_err(unreached)?;
        if reply.status == StatusCode::UNAUTHORIZED {
            log_turned_away(request, &reply);
            self.forget_login(&login).await;
            login = self.login().await?;
            reply = self.send_rest(&call, &login).await.inspect_err(unreached)?;
        }
        self.result_of(request, &reply).await
    }

    /// The units on the account that this plugin can use. For the setup.
    pub async fn units_on_account(&self) -> Result<Vec<ListedUnit>, DeviceError> {
        Ok(listed_units(&self.signed_in(Rest::Mapping).await?))
    }

    /// The token for commands, renewed once it is close to running out.
    async fn hub_token(&self) -> Result<HubToken, DeviceError> {
        {
            let session = self.session.lock().await;
            if let Some(token) = session.hub.as_ref().filter(|token| token.lasting()) {
                return Ok(token.clone());
            }
        }
        let username = self.email.expose().to_string();
        let device_id = self.device.to_str().map_err(|_| unreadable())?.to_string();
        let result = self.signed_in(Rest::Register { device_id: &device_id, username: &username }).await?;
        let token = result.get("SasToken").and_then(Value::as_str).ok_or_else(unreadable)?;
        let token = HubToken::parse(token, self.hub.base()).ok_or_else(|| {
            DeviceError::Rejected("Toshiba's cloud named a hub this service does not use".to_string())
        })?;
        self.session.lock().await.hub = Some(token.clone());
        Ok(token)
    }

    async fn forget_hub_token(&self) {
        self.session.lock().await.hub = None;
    }

    /// Sends one device-to-cloud message to the hub.
    async fn send_message(&self, token: &HubToken, unit: &Unit, data: &str) -> Result<Reply, DeviceError> {
        let path =
            format!("/devices/{}/messages/events?api-version={HUB_API_VERSION}", percent_encode(&token.device_id));
        let request = self
            .hub
            .post(self.hub.url(&path)?)
            .header(AUTHORIZATION, token.authorization.clone())
            .header(CONTENT_TYPE, "application/json")
            .header("iothub-app-type", "mob")
            .header("iothub-contenttype", "application/json")
            .header("iothub-contentencoding", "utf-8")
            .body(message_body(&token.device_id, &unit.unique_id, data));
        self.hub.send(request).await
    }

    /// Sends one command. If the token had gone stale it sends once more with a new
    /// one, because the hub refused the first and passed nothing on.
    async fn send_command(&self, unit: &Unit, data: &str) -> Result<(), DeviceError> {
        let token = self.hub_token().await?;
        let unreached = |error: &DeviceError| log_unreached("command", error);
        let mut reply = self.send_message(&token, unit, data).await.inspect_err(unreached)?;
        if reply.status == StatusCode::UNAUTHORIZED {
            log_turned_away("command", &reply);
            self.forget_hub_token().await;
            let token = self.hub_token().await?;
            reply = self.send_message(&token, unit, data).await.inspect_err(unreached)?;
        }
        let status = reply.status;
        if !status.is_success() {
            log_turned_away("command", &reply);
        }
        if status.is_success() {
            Ok(())
        } else if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
            Err(self.slow_down(reply.retry_after).await)
        } else if status.is_server_error() {
            Err(cloud_trouble())
        } else {
            Err(DeviceError::Rejected(format!("Toshiba's cloud refused the command with status {}", status.as_u16())))
        }
    }
}

#[derive(Default)]
struct Cache {
    /// The last good reading, and when the cloud gave it.
    recent: Option<(Instant, Reading)>,
    failed: Option<(Instant, DeviceError)>,
    /// The last command: when it was sent, and how many readings have been taken since.
    commanded: Option<(Instant, u32)>,
}

impl Cache {
    /// What a reading that could not be taken leaves standing: while the cloud is only
    /// resting, the last good one, if it is recent enough. A cloud that did not answer,
    /// or a refused account, is said at once.
    fn instead_of(&self, why: DeviceError) -> Result<Reading, DeviceError> {
        match &self.recent {
            Some((at, reading)) if at.elapsed() < LAST_READING_STANDS && why == told_to_slow_down() => {
                Ok(reading.clone())
            }
            _ => Err(why),
        }
    }
}

pub struct Unit {
    id: String,
    unique_id: String,
    cache: Mutex<Cache>,
}

impl Unit {
    pub fn new(unit: &AirconConfig) -> Self {
        Self { id: unit.id.clone(), unique_id: unit.unique_id.clone(), cache: Mutex::new(Cache::default()) }
    }

    /// A reading made a moment ago, or a new one. The lock is held throughout, so
    /// that a reading taken just before a command cannot be mistaken for one after it.
    pub async fn reading(&self, cloud: &Cloud) -> Result<Reading, DeviceError> {
        let mut cache = self.cache.lock().await;
        if cache.commanded.is_some_and(|(sent, _)| sent.elapsed() >= COMMAND_WINDOW) {
            cache.commanded = None;
        }
        // After a command: once at once, then 3, 6 and 12 seconds apart.
        let (fresh_for, failure_stands) = match cache.commanded {
            Some((_, taken)) => {
                (LOOK_AGAIN_AFTER_COMMAND.saturating_mul(1 << taken.saturating_sub(1).min(4)), LOOK_AGAIN_AFTER_COMMAND)
            }
            None => (READING_STAYS_FRESH, FAILED_READING_STANDS),
        };
        if cache.recent.as_ref().is_some_and(|(at, _)| !cloud.vouches_for(*at)) {
            cache.recent = None;
        }
        if let Some((at, reading)) = cache.recent.as_ref() {
            let from_before_the_command = cache.commanded.is_some_and(|(sent, _)| *at < sent);
            if !from_before_the_command && at.elapsed() < fresh_for {
                return Ok(reading.clone());
            }
        }
        if let Some((at, why)) = cache.failed.as_ref() {
            if at.elapsed() < failure_stands {
                return cache.instead_of(why.clone());
            }
        }
        if let Some((_, taken)) = cache.commanded.as_mut() {
            *taken = taken.saturating_add(1);
        }
        match self.ask(cloud).await {
            Ok(reading) => {
                if cache.failed.is_some() || cache.recent.is_none() {
                    tracing::info!("Toshiba's cloud answered a reading");
                }
                cache.failed = None;
                cache.recent = Some((Instant::now(), reading.clone()));
                Ok(reading)
            }
            Err(why) => {
                cloud.note_failure(&why);
                cache.failed = Some((Instant::now(), why.clone()));
                cache.instead_of(why)
            }
        }
    }

    async fn ask(&self, cloud: &Cloud) -> Result<Reading, DeviceError> {
        let result = cloud.signed_in(Rest::State { ac_id: &self.id }).await?;
        let state = result.get("ACStateData").and_then(Value::as_str).ok_or_else(unreadable)?;
        Reading::parse(state)
    }

    async fn expect_change(&self) {
        let mut cache = self.cache.lock().await;
        cache.failed = None;
        cache.commanded = Some((Instant::now(), 0));
    }
}

/// The air conditioners of one instance.
pub struct ToshibaDriver {
    cloud: Arc<Cloud>,
    units: HashMap<String, Arc<Unit>>,
}

impl ToshibaDriver {
    pub fn new(grant: LiveGrant, config: &ToshibaConfig, login: &str, timeouts: Timeouts) -> Result<Self, String> {
        let cloud = Arc::new(Cloud::new(grant, config, login, timeouts)?);
        let units = config.units.iter().map(|unit| (unit.target(), Arc::new(Unit::new(unit)))).collect();
        Ok(Self { cloud, units })
    }

    fn unit(&self, target: &str) -> Result<&Arc<Unit>, DeviceError> {
        self.units.get(target).ok_or_else(|| DeviceError::Disabled(format!("{target} is not a known air conditioner.")))
    }

    /// Made in its own task. A caller that stops waiting, as a poll does after a few
    /// seconds, does not throw away a slow sign-in or the reading it leads to.
    async fn reading(&self, unit: &Arc<Unit>) -> Result<Reading, DeviceError> {
        let (cloud, unit) = (self.cloud.clone(), unit.clone());
        tokio::spawn(async move { unit.reading(&cloud).await }).await.unwrap_or_else(|_| Err(cloud_trouble()))
    }
}

#[async_trait]
impl Driver for ToshibaDriver {
    async fn command(&self, target: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        let unit = self.unit(target)?;
        let needs_current = matches!(op, Op::AirconPower { on: true } | Op::AirconTemp { .. });
        let current = if needs_current { Some(self.reading(unit).await?) } else { None };
        let data = command_data(&changes_for(op, current.as_ref())?);
        if let Err(why) = self.cloud.send_command(unit, &data).await {
            self.cloud.note_failure(&why);
            return Err(why);
        }
        unit.expect_change().await;
        Ok(CommandAck)
    }

    async fn read_state(&self, target: &str) -> Result<State, DeviceError> {
        let unit = self.unit(target)?;
        Ok(self.reading(unit).await?.device_state())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_request_to_slow_down_doubles_the_wait_up_to_a_quarter_hour() {
        let waits: Vec<u64> = (1..=7).map(|times| slow_down_wait(times, None).as_secs()).collect();
        assert_eq!(waits, [60, 120, 240, 480, 900, 900, 900]);
    }

    #[test]
    fn the_wait_is_never_shorter_than_the_cloud_asked_nor_longer_than_the_longest() {
        let asked = |seconds| Some(Duration::from_secs(seconds));
        assert_eq!(slow_down_wait(1, asked(60)).as_secs(), 60);
        assert_eq!(slow_down_wait(1, asked(300)).as_secs(), 300);
        assert_eq!(slow_down_wait(3, asked(30)).as_secs(), 240);
        assert_eq!(slow_down_wait(1, asked(86_400)).as_secs(), 900);
    }

    #[test]
    fn a_retry_after_is_read_as_the_cloud_writes_it() {
        use swifty_net::retry_after_seconds;
        assert_eq!(retry_after_seconds("60 seconds"), Some(Duration::from_secs(60)));
        assert_eq!(retry_after_seconds("120"), Some(Duration::from_secs(120)));
        assert_eq!(retry_after_seconds(" 5"), Some(Duration::from_secs(5)));
        assert_eq!(retry_after_seconds("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(retry_after_seconds(""), None);
        assert_eq!(retry_after_seconds("99999999999999999999"), Some(Duration::from_secs(999_999)));
    }
}

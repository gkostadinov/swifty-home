//! The test kit's mocks as the suite sees them, and [`Rigged`], which adds the
//! switches a plain mock lacks.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use super::{Fault, MockDevice};
use crate::devices::*;
use crate::trap::{Trap, Traps};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The path and query of each request a mock HTTP server saw.
fn http_urls(http: &MockHttp) -> Vec<String> {
    http.requests()
        .into_iter()
        .map(|seen| if seen.query.is_empty() { seen.path } else { format!("{}?{}", seen.path, seen.query) })
        .collect()
}

/// Silence for a mock HTTP server: requests are recorded and left unanswered.
fn http_silent(http: &MockHttp, silent: bool) -> bool {
    http.hang.store(silent, Ordering::SeqCst);
    true
}

/// A fault on every reply's body.
fn http_fault(http: &MockHttp, fault: Option<Fault>) -> bool {
    http.switches.set_fault(fault);
    true
}

/// A name to report, when the handler reads one.
fn http_text(http: &MockHttp, text: Option<&str>) -> bool {
    if !http.reports_text() {
        return false;
    }
    http.switches.set_text(text);
    true
}

impl MockDevice for MockHttp {
    fn traffic(&self) -> usize {
        MockHttp::traffic(self)
    }
    /// One per request that asks the device to do something
    /// ([`MockHttp::count_commands`]): by default each that is not a `GET`,
    /// `HEAD` or `OPTIONS`. Reads and sign-ins before a command are not commands.
    fn exchanges(&self) -> usize {
        self.commands()
    }
    fn set_silent(&self, silent: bool) -> bool {
        http_silent(self, silent)
    }
    fn set_fault(&self, fault: Option<Fault>) -> bool {
        http_fault(self, fault)
    }
    fn set_device_text(&self, text: Option<&str>) -> bool {
        http_text(self, text)
    }
    fn urls(&self) -> Vec<String> {
        http_urls(self)
    }
}

impl MockDevice for MockUdp {
    fn traffic(&self) -> usize {
        MockUdp::traffic(self)
    }
    fn set_silent(&self, silent: bool) -> bool {
        self.silent.store(silent, Ordering::SeqCst);
        true
    }
}

impl MockDevice for MockTcp {
    fn traffic(&self) -> usize {
        MockTcp::traffic(self)
    }
    fn set_silent(&self, silent: bool) -> bool {
        self.hang.store(silent, Ordering::SeqCst);
        true
    }
}

impl MockDevice for MockWs {
    fn traffic(&self) -> usize {
        MockWs::traffic(self)
    }
    /// One per text frame.
    fn exchanges(&self) -> usize {
        lock(&self.frames).len()
    }
    fn set_silent(&self, silent: bool) -> bool {
        self.hang.store(silent, Ordering::SeqCst);
        true
    }
}

impl MockDevice for MockBulb {
    fn traffic(&self) -> usize {
        MockBulb::traffic(self)
    }
    fn set_silent(&self, silent: bool) -> bool {
        *lock(&self.mode) = if silent { BulbMode::Silent } else { BulbMode::Normal };
        true
    }
    /// Every reply datagram, spoiled. An oversized one is larger than the 2 kB
    /// a socket helper takes in.
    fn set_fault(&self, fault: Option<Fault>) -> bool {
        self.switches.set_fault(fault);
        true
    }
    /// In place of the MAC address a reading carries.
    fn set_device_text(&self, text: Option<&str>) -> bool {
        self.switches.set_text(text);
        true
    }
}

impl MockDevice for MockTuya {
    fn traffic(&self) -> usize {
        MockTuya::traffic(self)
    }
    fn set_silent(&self, silent: bool) -> bool {
        lock(&self.state).mode = if silent { TuyaMode::Silent } else { TuyaMode::Normal };
        true
    }
    /// A spoiled frame, half a frame, or a header that claims far more than any
    /// socket sends: a framed reply is read by its length, so bytes after it
    /// would never be looked at.
    fn set_fault(&self, fault: Option<Fault>) -> bool {
        match fault {
            Some(Fault::Oversized) => {
                self.switches.set_fault(None);
                lock(&self.state).mode = TuyaMode::Oversized;
            }
            other => {
                self.switches.set_fault(other);
                lock(&self.state).mode = TuyaMode::Normal;
            }
        }
        true
    }
    /// As the `devId` of a reading.
    fn set_device_text(&self, text: Option<&str>) -> bool {
        self.switches.set_text(text);
        true
    }
}

impl MockDevice for MockTv {
    fn traffic(&self) -> usize {
        MockTv::traffic(self)
    }
    /// One per message that is neither a registration nor a question: a
    /// `request` whose URI does not start with `get`.
    fn exchanges(&self) -> usize {
        lock(&self.received)
            .iter()
            .filter(|message| message["type"] == "request")
            .filter_map(|message| message["uri"].as_str())
            .filter(|uri| !uri.rsplit('/').next().unwrap_or("").starts_with("get"))
            .count()
            + lock(&self.frames).len()
    }
    fn set_silent(&self, silent: bool) -> bool {
        self.hang.store(silent, Ordering::SeqCst);
        true
    }
    fn set_fault(&self, fault: Option<Fault>) -> bool {
        self.switches.set_fault(fault);
        true
    }
    /// As the app in the foreground.
    fn set_device_text(&self, text: Option<&str>) -> bool {
        self.switches.set_text(text);
        true
    }
}

impl MockDevice for MockRtsp {
    fn traffic(&self) -> usize {
        MockRtsp::traffic(self)
    }
    fn set_silent(&self, silent: bool) -> bool {
        self.hang.store(silent, Ordering::SeqCst);
        true
    }
    fn set_fault(&self, fault: Option<Fault>) -> bool {
        self.switches.set_fault(fault);
        true
    }
    /// In a `Server` header.
    fn set_device_text(&self, text: Option<&str>) -> bool {
        self.switches.set_text(text);
        true
    }
}

/// The mocks built on [`MockHttp`]: their traffic as before, their own command
/// rule, silence, faults, the names they report, URLs.
macro_rules! over_http {
    ($($mock:ty => |$it:ident| $traffic:expr;)*) => {
        $(impl MockDevice for $mock {
            fn traffic(&self) -> usize {
                let $it = self;
                $traffic
            }
            fn exchanges(&self) -> usize {
                MockDevice::exchanges(&self.http)
            }
            fn set_silent(&self, silent: bool) -> bool {
                http_silent(&self.http, silent)
            }
            fn set_fault(&self, fault: Option<Fault>) -> bool {
                http_fault(&self.http, fault)
            }
            fn set_device_text(&self, text: Option<&str>) -> bool {
                http_text(&self.http, text)
            }
            fn urls(&self) -> Vec<String> {
                http_urls(&self.http)
            }
        })*
    };
}

over_http! {
    MockSonos => |it| it.http.traffic();
    MockMoonraker => |it| it.http.traffic();
    MockProxmox => |it| it.http.traffic();
    MockToshiba => |it| it.traffic();
    MockPetkit => |it| it.http.traffic();
}

/// The rest: traffic only.
macro_rules! traffic_is {
    ($($mock:ty => |$it:ident| $count:expr;)*) => {
        $(impl MockDevice for $mock {
            fn traffic(&self) -> usize {
                let $it = self;
                $count
            }
        })*
    };
}

traffic_is! {
    MockBroadcast => |it| it.traffic();
    MockWol => |it| it.traffic();
    MockTlsHttp => |it| it.traffic();
    MockTlsWs => |it| it.traffic();
    Trap => |it| it.traffic();
    Traps => |it| it.traffic();
}

/// Switches a mock's handler reads: how to spoil a reply, and what name to
/// report. Clone it into the handler, and wrap the mock in [`Rigged`].
///
/// ```text
/// let switches = Switches::default();
/// let inside = switches.clone();
/// let device = MockUdp::start(move |request| {
///     let name = inside.device_text("Desk lamp");
///     Some(inside.garble(answer(request, &name)))
/// }).await;
/// let device = Rigged::new(device, switches);
/// ```
#[derive(Clone, Default)]
pub struct Switches {
    fault: Arc<Mutex<Option<Fault>>>,
    text: Arc<Mutex<Option<String>>>,
}

impl Switches {
    /// The fault in force, if any.
    pub fn fault(&self) -> Option<Fault> {
        *lock(&self.fault)
    }

    /// Puts a fault in force, or takes it away.
    pub fn set_fault(&self, fault: Option<Fault>) {
        *lock(&self.fault) = fault;
    }

    /// Sets the name to report, or goes back to the normal one.
    pub fn set_text(&self, text: Option<&str>) {
        *lock(&self.text) = text.map(str::to_string);
    }

    /// Whether the suite has set a name to report.
    pub fn has_text(&self) -> bool {
        lock(&self.text).is_some()
    }

    /// `reply`, spoiled when a fault is in force.
    pub fn garble(&self, reply: Vec<u8>) -> Vec<u8> {
        match self.fault() {
            Some(fault) => fault.apply(&reply),
            None => reply,
        }
    }

    /// The same, for a device that speaks text.
    pub fn garble_text(&self, reply: String) -> String {
        match self.fault() {
            Some(fault) => fault.apply_text(&reply),
            None => reply,
        }
    }

    /// The name to report: the suite's hostile text while it is set, `normal` otherwise.
    pub fn device_text(&self, normal: &str) -> String {
        lock(&self.text).clone().unwrap_or_else(|| normal.to_string())
    }
}

/// A mock with [`Switches`]: it can spoil its replies and report any name,
/// as far as its handler reads the switches.
pub struct Rigged<M> {
    /// The mock.
    pub device: M,
    /// What its handler reads.
    pub switches: Switches,
}

impl<M: MockDevice> Rigged<M> {
    /// `device`, whose handler holds a clone of `switches`.
    pub fn new(device: M, switches: Switches) -> Self {
        Self { device, switches }
    }
}

impl<M: MockDevice> MockDevice for Rigged<M> {
    fn traffic(&self) -> usize {
        self.device.traffic()
    }
    fn exchanges(&self) -> usize {
        self.device.exchanges()
    }
    fn set_silent(&self, silent: bool) -> bool {
        self.device.set_silent(silent)
    }
    fn set_fault(&self, fault: Option<Fault>) -> bool {
        self.switches.set_fault(fault);
        true
    }
    fn set_device_text(&self, text: Option<&str>) -> bool {
        self.switches.set_text(text);
        true
    }
    fn urls(&self) -> Vec<String> {
        self.device.urls()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faults_spoil_a_reply_each_in_its_own_way() {
        let reply = br#"{"on":true,"bri":60}"#;
        let malformed = Fault::Malformed.apply(reply);
        assert_eq!(malformed.len(), reply.len());
        assert!(serde_json::from_slice::<serde_json::Value>(&malformed).is_err());
        assert_eq!(Fault::Truncated.apply(reply), reply[..reply.len() / 2].to_vec());
        assert_eq!(Fault::Oversized.apply(reply).len(), reply.len() + super::super::OVERSIZE);
        assert!(Fault::Malformed.apply_text("hello world!").contains('\u{fffd}'));

        let switches = Switches::default();
        assert_eq!(switches.garble(reply.to_vec()), reply.to_vec());
        let rigged = Rigged::new(super::super::Nothing, switches.clone());
        assert!(!rigged.set_silent(true), "silence is the mock's own");
        assert!(rigged.set_fault(Some(Fault::Truncated)));
        assert_eq!(switches.garble(reply.to_vec()).len(), reply.len() / 2);
        assert!(rigged.set_device_text(Some("x\u{7}")));
        assert_eq!(switches.device_text("Lamp"), "x\u{7}");
        rigged.set_device_text(None);
        assert_eq!(switches.device_text("Lamp"), "Lamp");
    }
}

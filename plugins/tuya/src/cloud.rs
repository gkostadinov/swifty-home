//! Tuya's cloud OpenAPI, used once during setup to list a project's devices and
//! their local keys. Nothing here runs after setup: the sockets are switched on
//! the local network only.
//!
//! Every request is a GET signed with HMAC-SHA256 under the project's access
//! secret, as Tuya documents for cloud projects:
//!
//! ```text
//! string_to_sign = METHOD "\n" sha256_hex(body) "\n" "" "\n" path_and_sorted_query
//! sign           = upper_hex(hmac_sha256(access_secret, client_id [access_token] t nonce string_to_sign))
//! ```
//!
//! The secret itself never leaves this process, and no text the cloud sends is
//! shown: errors are fixed sentences with at most Tuya's numeric code.

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use swifty_plugin_api::net::{device_text, Http, LiveGrant, Timeouts};
use swifty_plugin_api::{DeviceError, Secret};

use crate::config::is_plug_id;
use crate::protocol::stored_key;

/// A Tuya data center: the code a person picks, what it is called, and its OpenAPI host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// What the setup form offers.
    pub code: &'static str,
    /// Where it is.
    pub label: &'static str,
    /// Its OpenAPI host. Every one is declared in the manifest.
    pub host: &'static str,
}

/// Tuya's data centers, as its developer platform lists them.
pub const REGIONS: &[Region] = &[
    Region { code: "eu", label: "Central Europe", host: "openapi.tuyaeu.com" },
    Region { code: "eu-w", label: "Western Europe", host: "openapi-weaz.tuyaeu.com" },
    Region { code: "us", label: "Western America", host: "openapi.tuyaus.com" },
    Region { code: "us-e", label: "Eastern America", host: "openapi-ueaz.tuyaus.com" },
    Region { code: "cn", label: "China", host: "openapi.tuyacn.com" },
    Region { code: "in", label: "India", host: "openapi.tuyain.com" },
];

/// The region with this code.
pub fn region(code: &str) -> Option<&'static Region> {
    REGIONS.iter().find(|region| region.code == code)
}

/// The longest access id the form accepts. Tuya's are 20 characters.
pub const MAX_ACCESS_ID: usize = 64;
/// Devices asked for in one page of the list.
const PAGE_SIZE: u32 = 50;
/// Pages read at most, so a cloud that always says "more" cannot keep setup busy.
const MAX_PAGES: usize = 20;
/// The SHA-256 of an empty body, which every GET signs.
const EMPTY_BODY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// Tuya's code for a request whose time is too far from its own.
const CODE_BAD_TIME: i64 = 1013;

type HmacSha256 = Hmac<Sha256>;

/// One device the project lists, with a key the local protocol can use.
#[derive(Debug, Clone)]
pub struct CloudDevice {
    /// The device id, in lower case.
    pub id: String,
    /// Its name in the Tuya app, cleaned.
    pub name: String,
    /// Its product name or category, cleaned.
    pub kind: String,
    /// Tuya's category code, cleaned ("cz" is a socket, "pc" a power strip).
    pub category: String,
    /// Whether the cloud last saw it online.
    pub online: bool,
    /// The local key in the form the secret slot stores. `Debug` shows nothing of it.
    pub key: Secret,
}

impl CloudDevice {
    /// Whether Tuya files it as a socket or a power strip.
    pub fn is_socket(&self) -> bool {
        matches!(self.category.as_str(), "cz" | "pc")
    }
}

/// What the device list held, and what had to be left out.
#[derive(Debug, Default)]
pub struct Listing {
    /// Devices with a usable id and key.
    pub usable: Vec<CloudDevice>,
    /// How many devices had no usable id or local key (hubs, sub-devices, others).
    pub skipped: usize,
}

/// The signature over one request, in upper-case hex.
pub(crate) fn signature(
    signing_key: &[u8],
    client_id: &str,
    access_token: &str,
    t: &str,
    method: &str,
    path_and_query: &str,
) -> String {
    let string_to_sign = format!("{method}\n{EMPTY_BODY_SHA256}\n\n{path_and_query}");
    // The nonce is optional and left empty.
    let message = format!("{client_id}{access_token}{t}{string_to_sign}");
    let mut mac = HmacSha256::new_from_slice(signing_key).expect("HMAC takes a key of any length");
    mac.update(message.as_bytes());
    hex::encode_upper(mac.finalize().into_bytes())
}

fn milliseconds_now() -> String {
    let elapsed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    elapsed.as_millis().to_string()
}

/// Whether a row key Tuya hands back for the next page can go into a URL as it is.
fn is_row_key(text: &str) -> bool {
    (1..=128).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A signed-in session with one cloud project.
pub struct Cloud {
    http: Http,
    host: String,
    client_id: String,
    signing_key: Secret,
    access_token: String,
}

/// Why the cloud could not be used, as a sentence for the person.
fn unreachable(host: &str, error: &DeviceError) -> String {
    let why = if matches!(error, DeviceError::Timeout) { "did not answer in time" } else { "could not be reached" };
    format!(
        "Tuya's cloud at {host} {why}. Check that the hub can reach the internet and that the data center is right."
    )
}

impl Cloud {
    /// Asks for a token. `base` is `https://<region host>`, or a test cloud.
    pub async fn sign_in(
        grant: LiveGrant,
        base: &str,
        timeouts: Timeouts,
        client_id: &str,
        signing_key: Secret,
    ) -> Result<Self, String> {
        let http = Http::new(grant, base, timeouts, None).map_err(|_| "The cloud address is not valid.".to_string())?;
        let host = http.base().host_str().unwrap_or("the cloud").to_string();
        let mut cloud = Self { http, host, client_id: client_id.to_string(), signing_key, access_token: String::new() };
        let result = cloud.get("/v1.0/token?grant_type=1").await.map_err(|failure| match failure {
            Failure::Code(CODE_BAD_TIME) => {
                "Tuya refused the request's time (code 1013). Check that the hub's clock is right.".to_string()
            }
            Failure::Code(code) => format!(
                "Tuya refused the login (code {code}). Check the access id, the access secret and the data center."
            ),
            Failure::Text(text) => text,
        })?;
        let token = result.get("access_token").and_then(Value::as_str).unwrap_or("");
        if token.is_empty() || token.len() > 256 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("Tuya's cloud sent a login reply that could not be read.".to_string());
        }
        cloud.access_token = token.to_string();
        Ok(cloud)
    }

    /// Every device of the project's linked app accounts, a page at a time.
    pub async fn devices(&self) -> Result<Listing, String> {
        let mut listing = Listing::default();
        let mut row_key: Option<String> = None;
        for _ in 0..MAX_PAGES {
            // The query is sorted by key, as the signature requires.
            let path = match &row_key {
                Some(key) => format!("/v1.0/iot-01/associated-users/devices?last_row_key={key}&size={PAGE_SIZE}"),
                None => format!("/v1.0/iot-01/associated-users/devices?size={PAGE_SIZE}"),
            };
            let page = self.get(&path).await.map_err(|failure| match failure {
                Failure::Code(code) => format!(
                    "Tuya refused the device list (code {code}). Check that the cloud project has the IoT Core \
                     and Authorization services and that the Tuya app account is linked to it."
                ),
                Failure::Text(text) => text,
            })?;
            let devices = page.get("devices").and_then(Value::as_array).cloned().unwrap_or_default();
            for device in &devices {
                match usable(device) {
                    Some(found) => listing.usable.push(found),
                    None => listing.skipped += 1,
                }
            }
            let more = page.get("has_more").and_then(Value::as_bool).unwrap_or(false);
            let next = page.get("last_row_key").and_then(Value::as_str).filter(|key| is_row_key(key));
            match (more, next) {
                (true, Some(next)) if row_key.as_deref() != Some(next) => row_key = Some(next.to_string()),
                _ => return Ok(listing),
            }
        }
        Ok(listing)
    }

    /// One signed GET. The reply's `result` when Tuya says it succeeded.
    async fn get(&self, path_and_query: &str) -> Result<Value, Failure> {
        let t = milliseconds_now();
        let sign = signature(
            self.signing_key.expose().as_bytes(),
            &self.client_id,
            &self.access_token,
            &t,
            "GET",
            path_and_query,
        );
        let url = self.http.url(path_and_query).map_err(|_| Failure::Text("The cloud address is not valid.".into()))?;
        let mut request = self
            .http
            .get(url)
            .header("client_id", &self.client_id)
            .header("sign", sign)
            .header("sign_method", "HMAC-SHA256")
            .header("t", t);
        if !self.access_token.is_empty() {
            request = request.header("access_token", &self.access_token);
        }
        let reply = self.http.send(request).await.map_err(|error| Failure::Text(unreachable(&self.host, &error)))?;
        if !reply.status.is_success() {
            return Err(Failure::Text(format!(
                "Tuya's cloud answered with HTTP status {}. Check the data center.",
                reply.status.as_u16()
            )));
        }
        let unreadable = || Failure::Text("Tuya's cloud sent a reply that could not be read.".to_string());
        let body = reply.json().map_err(|_| unreadable())?;
        if body.get("success").and_then(Value::as_bool) == Some(true) {
            return body.get("result").cloned().ok_or_else(unreadable);
        }
        Err(body.get("code").and_then(Value::as_i64).map_or_else(unreadable, Failure::Code))
    }
}

/// Why one request failed: Tuya's own code, or a sentence already written.
enum Failure {
    Code(i64),
    Text(String),
}

/// A listed device the local protocol can use, if it is one.
fn usable(device: &Value) -> Option<CloudDevice> {
    let text = |key: &str| device.get(key).and_then(Value::as_str).unwrap_or("");
    let id = text("id").to_ascii_lowercase();
    if !is_plug_id(&id) {
        return None;
    }
    let key = stored_key(text("local_key"))?;
    let name = device_text(text("name")).trim().chars().take(crate::config::MAX_PLUG_NAME).collect::<String>();
    let kind = [text("product_name"), text("category")].into_iter().find(|kind| !kind.is_empty()).unwrap_or("");
    Some(CloudDevice {
        id,
        name,
        kind: device_text(kind),
        category: device_text(text("category")),
        online: device.get("online").and_then(Value::as_bool).unwrap_or(false),
        key: key.into(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_empty_body_hash_is_the_real_one() {
        use sha2::Digest;
        assert_eq!(hex::encode(Sha256::digest(b"")), EMPTY_BODY_SHA256);
    }

    /// RFC 4231 test case 2, so the MAC is checked against something other than itself.
    #[test]
    fn the_mac_is_the_standard_one() {
        let mut mac = HmacSha256::new_from_slice(b"Jefe").unwrap();
        mac.update(b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn a_signature_covers_every_part_and_is_upper_case_hex() {
        let base = signature(b"k", "id", "", "1700000000000", "GET", "/v1.0/token?grant_type=1");
        assert_eq!(base.len(), 64);
        assert!(base.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase()), "{base}");
        for other in [
            signature(b"j", "id", "", "1700000000000", "GET", "/v1.0/token?grant_type=1"),
            signature(b"k", "ie", "", "1700000000000", "GET", "/v1.0/token?grant_type=1"),
            signature(b"k", "id", "x", "1700000000000", "GET", "/v1.0/token?grant_type=1"),
            signature(b"k", "id", "", "1700000000001", "GET", "/v1.0/token?grant_type=1"),
            signature(b"k", "id", "", "1700000000000", "POST", "/v1.0/token?grant_type=1"),
            signature(b"k", "id", "", "1700000000000", "GET", "/v1.0/token?grant_type=2"),
        ] {
            assert_ne!(other, base);
        }
        let mut mac = HmacSha256::new_from_slice(b"k").unwrap();
        mac.update(format!("id1700000000000GET\n{EMPTY_BODY_SHA256}\n\n/v1.0/token?grant_type=1").as_bytes());
        assert_eq!(base, hex::encode_upper(mac.finalize().into_bytes()));
    }

    #[test]
    fn only_a_device_with_an_id_and_a_key_is_usable() {
        let good = json!({ "id": "BF0123456789ABCDEF", "local_key": "0123456789abcdef", "name": "Desk\u{7}",
            "category": "cz", "product_name": "Smart socket", "online": true });
        let found = usable(&good).unwrap();
        assert_eq!(found.id, "bf0123456789abcdef");
        assert_eq!(found.name, "Desk");
        assert_eq!(found.kind, "Smart socket");
        assert!(found.is_socket() && found.online);
        assert_eq!(found.key.expose(), "MDEyMzQ1Njc4OWFiY2RlZg==");
        assert!(!format!("{found:?}").contains("MDEyMzQ1"), "{found:?}");
        for bad in [
            json!({ "id": "bf0123456789abcdef" }),
            json!({ "id": "bf0123456789abcdef", "local_key": "" }),
            json!({ "id": "bf0123456789abcdef", "local_key": "short" }),
            json!({ "id": "bf/0123456789abcdef", "local_key": "0123456789abcdef" }),
            json!({ "local_key": "0123456789abcdef" }),
            json!("not an object"),
        ] {
            assert!(usable(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn regions_and_row_keys() {
        assert_eq!(region("eu-w").unwrap().host, "openapi-weaz.tuyaeu.com");
        assert!(region("mars").is_none());
        assert!(is_row_key("AbC_12-3"));
        for bad in ["", "a b", "a&size=1", "a=b", &"x".repeat(129)] {
            assert!(!is_row_key(bad), "{bad}");
        }
    }
}

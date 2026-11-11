//! How this plugin presents itself to Petkit's servers, in one place.
//!
//! Petkit offers its cloud to its own app only, so this plugin presents itself as
//! Petkit's Android app, the way the open-source `pypetkitapi` client does: an app
//! version, the HTTP client's user agent, and the phone the app runs on. These are the
//! values the servers accepted when this was written. They will drift: Petkit may
//! refuse an app version it considers too old, and may object to clients that are not
//! its app. That is why each of them is only a default, and the instance config can set
//! another value without a new build (`app_version`, `user_agent`, `os_version`,
//! `phone_model`, `phone_brand`).

/// The version of Petkit's app this plugin says it is. Sent as `X-Api-Version`, as the
/// login's `oldVersion`, and in the login's client description. Config key `app_version`.
pub const APP_VERSION: &str = "13.2.1";

/// The app's HTTP client. Config key `user_agent`.
pub const USER_AGENT: &str = "okhttp/3.14.9";

/// The Android version of the phone. Config key `os_version`.
pub const OS_VERSION: &str = "16.1";

/// The phone's model. Config key `phone_model`.
pub const PHONE_MODEL: &str = "23127PN0CG";

/// The phone's maker. Config key `phone_brand`.
pub const PHONE_BRAND: &str = "Xiaomi";

/// The language the app asks for. Fixed: the plugin reads no text Petkit writes.
pub const LOCALE: &str = "en-US";
pub const ACCEPT_LANGUAGE: &str = "en-US;q=1, it-US;q=0.9";

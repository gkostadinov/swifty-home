//! How this plugin presents itself to Toshiba's cloud, in one place.
//!
//! Toshiba offers its cloud to its own app only, so the cloud decides from what a
//! client sends whether to answer it at all. These are the values it accepted when
//! this was written. They will drift: the cloud may start to want a newer browser,
//! or a client id of another shape, and Toshiba may object to clients that look
//! like this one. That is why each of them is only a default, and the instance
//! config can set another value without a new build (`user_agent`, `client_suffix`).

/// The cloud's firewall turns away clients that do not look like a browser, so every
/// request to the REST API carries a desktop browser's user agent. Config key `user_agent`.
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Joined to the account's e-mail with `_`, this gives the hub its own client id at the
/// cloud, sent in a `Device-ID` header on every request and registered for commands.
/// It is a short word in hex ("swfthome"), the shape the Toshiba app's own ids have.
/// Config key `client_suffix`.
///
/// The cloud remembers the client ids it has seen for an account. An install that was
/// set up before this plugin existed keeps the suffix it registered then: the migration
/// writes that value into its config, so the cloud keeps seeing the same client.
pub const CLIENT_SUFFIX: &str = "73776674686f6d65";

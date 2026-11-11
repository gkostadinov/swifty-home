//! Which certificates a host's API may present, and the API token header.
//!
//! The configured host trusts the public roots plus the CA in the `ca` slot, as
//! before. A further host was added by address, so it trusts only the CA file
//! named for it. Nothing here ever accepts an invalid certificate, and the token
//! is sent only once a handshake has checked out.

use std::path::Path;

use http::HeaderValue;
use swifty_net::{Http, LiveGrant, Roots, Timeouts};

/// What a PEM certificate starts with.
const PEM_CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----";

/// The `Authorization` header for a token, marked sensitive so that no debug
/// output of a request shows it.
pub fn authorization(token: &str) -> Result<HeaderValue, String> {
    if !token.contains('!') || !token.contains('=') {
        return Err("the Proxmox token must hold user@realm!tokenid=uuid".to_string());
    }
    let mut value = HeaderValue::from_str(&format!("PVEAPIToken={token}"))
        .map_err(|_| "the Proxmox token holds invalid text".to_string())?;
    value.set_sensitive(true);
    Ok(value)
}

/// Whether `pem` holds at least one certificate.
pub fn is_pem_certificate(pem: &[u8]) -> bool {
    String::from_utf8_lossy(pem).contains(PEM_CERTIFICATE)
}

/// An HTTP client for `url` that trusts `roots` and the CA in `ca_pem`, the PEM
/// text of the `ca` slot. The CA goes to the client as it is: nothing is
/// written to a file.
pub fn client(
    grant: LiveGrant,
    url: &str,
    timeouts: Timeouts,
    ca_pem: Option<&[u8]>,
    roots: Roots,
) -> Result<Http, String> {
    let Some(pem) = ca_pem else {
        return Http::with_ca_pem(grant, url, timeouts, None, roots);
    };
    if !is_pem_certificate(pem) {
        return Err("the CA certificate is not a PEM certificate".to_string());
    }
    Http::with_ca_pem(grant, url, timeouts, Some(pem), roots).map_err(|error| {
        if error.contains("URL") {
            error
        } else {
            "the CA certificate could not be used".to_string()
        }
    })
}

/// An HTTP client for `url` that trusts `roots` and the CA in the file `ca_file`.
pub fn client_with_file(
    grant: LiveGrant,
    url: &str,
    timeouts: Timeouts,
    ca_file: &Path,
    roots: Roots,
) -> Result<Http, String> {
    Http::with_roots(grant, url, timeouts, Some(ca_file), roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_header_needs_the_documented_shape_and_is_sensitive() {
        assert!(authorization("just-a-value").is_err());
        assert!(authorization("user@pve!id").is_err());
        let header = authorization("user@pve!id=abc").unwrap();
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains("abc"));
        assert!(authorization("user@pve!id=a\nb").is_err());
    }

    #[test]
    fn a_ca_that_is_not_a_certificate_is_refused_before_anything_is_built() {
        let grant = LiveGrant::when_live(true).unwrap();
        let timeouts =
            Timeouts { connect: std::time::Duration::from_millis(1), read: std::time::Duration::from_millis(1) };
        let error = client(grant, "https://127.0.0.1:9", timeouts, Some(b"not a certificate"), Roots::PublicAndNamedCa)
            .err()
            .unwrap();
        assert_eq!(error, "the CA certificate is not a PEM certificate");
        let fake = format!("{PEM_CERTIFICATE}\nAAAA\n-----END CERTIFICATE-----\n");
        let error = client(grant, "https://127.0.0.1:9", timeouts, Some(fake.as_bytes()), Roots::PublicAndNamedCa)
            .err()
            .unwrap();
        assert_eq!(error, "the CA certificate could not be used");
    }

    #[test]
    fn a_ca_from_the_secret_store_is_used_without_a_file() {
        let source = include_str!("trust.rs");
        assert!(!source.contains(concat!("temp", "file")), "nothing is written to a file");
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let pem = params.self_signed(&key).unwrap().pem();
        let grant = LiveGrant::when_live(true).unwrap();
        let timeouts =
            Timeouts { connect: std::time::Duration::from_millis(1), read: std::time::Duration::from_millis(1) };
        assert!(client(grant, "https://127.0.0.1:9", timeouts, Some(pem.as_bytes()), Roots::NamedCaOnly).is_ok());
        let error = client(grant, "not a url", timeouts, Some(pem.as_bytes()), Roots::NamedCaOnly).err().unwrap();
        assert!(error.contains("URL"), "{error}");
    }
}

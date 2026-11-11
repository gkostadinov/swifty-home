//! Check 9: no secret value leaves the plugin except to the device.

use std::collections::BTreeSet;

use super::{Observed, Report};

/// Values shorter than this are not searched for: they would match by chance.
const SHORTEST_CANARY: usize = 4;

/// `text` with every byte that is not unreserved in a URL percent-encoded.
fn percent_encoded(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Searches everything the plugin said, logged and asked for.
/// `canaries` is slot and value; a problem names the slot, never the value.
pub(super) fn check(
    canaries: &[(String, String)],
    observed: &Observed,
    logs: &str,
    urls: &[String],
    report: &mut Report,
) {
    let mut found = BTreeSet::new();
    for (slot, value) in canaries {
        if value.chars().count() < SHORTEST_CANARY {
            report.skip(9, format!("the secret for `{slot}` is too short to search for; use one of {SHORTEST_CANARY} or more characters"));
            continue;
        }
        let encoded = percent_encoded(value);
        for (place, text) in &observed.texts {
            if text.contains(value.as_str()) {
                found.insert(format!("the secret for `{slot}` appears in {place}"));
            }
        }
        if logs.contains(value.as_str()) {
            found.insert(format!("the secret for `{slot}` appears in the plugin's log output"));
        }
        for url in urls {
            let query = url.split_once('?').map(|(_, query)| query).unwrap_or("");
            if query.contains(value.as_str()) || query.contains(&encoded) {
                found.insert(format!(
                    "the secret for `{slot}` appears in a URL query the device saw; send it in a header or the body"
                ));
            }
        }
    }
    for text in found {
        report.fail(9, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_found_wherever_it_is_and_never_printed() {
        let canaries = vec![("key".to_string(), "s3cr3t value".to_string()), ("pin".to_string(), "12".to_string())];
        let mut observed = Observed::default();
        observed.add("an error", "failed with s3cr3t value");
        let mut report = Report::default();
        check(
            &canaries,
            &observed,
            "level=debug token=s3cr3t value",
            &["/api?k=s3cr3t%20value".to_string()],
            &mut report,
        );
        let texts: Vec<_> = report.problems.iter().map(|p| p.text.clone()).collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert!(texts.iter().all(|text| !text.contains("s3cr3t")), "{texts:?}");
        assert_eq!(report.not_run.len(), 1, "the short one is not searched");

        let mut report = Report::default();
        check(&canaries, &Observed::default(), "", &["/api/s3cr3t%20value".to_string()], &mut report);
        assert!(report.problems.is_empty(), "a path is not a query: {:?}", report.problems);
    }
}

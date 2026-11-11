//! Each deliberately broken variant of the lamp plugin fails the check that is
//! there to catch it, and only that one, so a problem points at its cause.

mod lamp;

use lamp::{Bug, Lamp};
use swifty_plugin_testkit::conformance::{report, Report};

async fn report_for(plugin: &'static Lamp) -> Report {
    report(plugin, lamp::fixture().await).await
}

/// The variant fails `check`, and nothing else.
async fn fails_only(plugin: &'static Lamp, check: u8, needle: &str) {
    let report = report_for(plugin).await;
    assert!(report.failed(check), "{:?} should fail check {check}:\n{report}", plugin.bug);
    assert_eq!(report.failed_checks().into_iter().collect::<Vec<_>>(), vec![check], "{:?}:\n{report}", plugin.bug);
    assert!(
        report.problems.iter().any(|problem| problem.check == check && problem.text.contains(needle)),
        "{:?}: no problem of check {check} says {needle:?}:\n{report}",
        plugin.bug
    );
}

#[tokio::test]
async fn a_socket_in_refusal_fails_purity() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::SocketInRefusal);
    let report = report_for(&PLUGIN).await;
    assert!(report.failed(3), "{report}");
    assert!(report.problems.iter().any(|p| p.check == 3 && p.text.contains("reached the network")), "{report}");
    // The dry-run hub and the live refusals reach the lamp too.
    assert!(report.failed(4) && report.failed(5), "{report}");
}

#[tokio::test]
async fn a_refusal_that_panics_at_the_edge_of_a_range_fails_the_dry_run() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::PanicsAtTheTopOfTheRange);
    fails_only(&PLUGIN, 4, "\"percent\":100").await;
}

#[tokio::test]
async fn a_rule_only_the_driver_knows_fails_refusals_agree() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::FloorOnlyInTheDriver);
    fails_only(&PLUGIN, 5, "the driver answers Disabled").await;
}

#[tokio::test]
async fn a_retry_fails_exactly_once() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::Retries);
    fails_only(&PLUGIN, 6, "never retry").await;
}

#[tokio::test]
async fn ignoring_the_timeouts_fails_timeouts() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::IgnoresTimeouts);
    fails_only(&PLUGIN, 7, "against a device that never answers").await;
}

#[tokio::test]
async fn a_state_that_is_not_the_devices_fails_confirmation() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::InvertsPower);
    fails_only(&PLUGIN, 8, "does not confirm it").await;
}

#[tokio::test]
async fn a_key_in_an_error_fails_secrets() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::LeaksTheKey);
    fails_only(&PLUGIN, 9, "appears in the error of").await;
    let report = report_for(&PLUGIN).await;
    assert!(!report.to_string().contains(lamp::KEY), "a problem never prints the secret:\n{report}");
}

#[tokio::test]
async fn a_key_in_the_log_fails_secrets() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::LogsTheKey);
    fails_only(&PLUGIN, 9, "log output").await;
}

#[tokio::test]
async fn a_raw_device_name_fails_device_text() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::RawDeviceText);
    fails_only(&PLUGIN, 10, "net::device_text").await;
}

#[tokio::test]
async fn a_name_the_setup_shows_as_the_lamp_sent_it_fails_device_text() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::RawNameInSetup);
    fails_only(&PLUGIN, 10, "of the setup flow (a long name)").await;
}

#[tokio::test]
async fn a_form_that_shows_the_stored_key_fails_setup_when_set_up_again() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::PrefillsTheStoredKey);
    fails_only(&PLUGIN, 11, "prefills the secret field `key` when set up again with its secrets stored").await;
    // The first setup, with nothing stored, is not what is wrong.
    let report = report_for(&PLUGIN).await;
    assert!(
        report.problems.iter().all(|problem| problem.text.contains("when set up again")),
        "only the second run is at fault:
{report}"
    );
}

#[tokio::test]
async fn a_flow_that_turns_away_a_setup_made_again_fails_setup() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::RefusesToSetUpAgain);
    fails_only(&PLUGIN, 11, "end when set up again with its secrets stored at Failed").await;
}

#[tokio::test]
async fn a_setup_that_makes_its_own_grant_fails_setup() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::SetupWithoutGrant);
    fails_only(&PLUGIN, 11, "with grant: None").await;
}

#[tokio::test]
async fn a_panic_on_a_short_reply_fails_robustness() {
    static PLUGIN: Lamp = Lamp::with_bug(Bug::PanicsOnAShortReply);
    fails_only(&PLUGIN, 12, "panicked on a truncated reply").await;
}

#[tokio::test]
async fn a_fixture_whose_device_has_no_switches_lists_what_was_not_run() {
    static PLUGIN: Lamp = Lamp::good();
    // The bare lamp, without the switches for faults and names.
    let report = report(&PLUGIN, lamp::fixture_with(false).await).await;
    assert!(report.passed(), "{report}");
    let skipped: Vec<u8> = report.not_run.iter().map(|p| p.check).collect();
    assert_eq!(skipped, vec![10, 12, 12, 12], "{report}");
}

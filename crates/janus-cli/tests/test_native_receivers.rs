//! Opt-in LAN acceptance for every receiver that is switched on and reachable.
//! Uses the same implementation as `janus diagnostics check-receivers`; no
//! device is named. Plug in any TV/screen and rerun to validate it.
//!
//! cargo test -p janus-cli --test test_native_receivers -- --ignored --nocapture
//! Optional: JANUS_TEST_RECEIVER_NAME="<exact name>" (target one unit),
//! JANUS_TEST_MIN_RECEIVERS=<n> (default 1), JANUS_TEST_HOLD_SECONDS=<s> (default 12),
//! JANUS_TEST_MIRROR_SECONDS=<s> (cast a test pattern to each screen; shows on the TV).
use janus_cli::acceptance::{Options, check_receivers};
use std::time::Duration;

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .map(|value| value.parse().expect("numeric environment value"))
        .unwrap_or(default)
}

#[tokio::test]
#[ignore = "requires receivers on the LAN and multicast access"]
async fn test_every_discovered_receiver_passes_implemented_stages() {
    let options = Options {
        discover_for: Duration::from_secs(5),
        hold: Duration::from_secs(env_number("JANUS_TEST_HOLD_SECONDS", 12)),
        name: std::env::var("JANUS_TEST_RECEIVER_NAME").ok(),
        mirror: std::env::var("JANUS_TEST_MIRROR_SECONDS")
            .ok()
            .map(|seconds| Duration::from_secs(seconds.parse().expect("seconds"))),
    };
    let reports = check_receivers(&options).await.expect("discovery runs");
    println!("{}", serde_json::to_string_pretty(&reports).unwrap());
    let minimum = env_number("JANUS_TEST_MIN_RECEIVERS", 1) as usize;
    assert!(
        reports.len() >= minimum,
        "found {} receiver(s), expected at least {minimum}",
        reports.len()
    );
    let failed: Vec<_> = reports
        .iter()
        .filter(|report| report.failed())
        .map(|report| &report.name)
        .collect();
    assert!(
        failed.is_empty(),
        "receivers failing acceptance: {failed:?}"
    );
}

//! Real-device smoke test (task #82 / review #33 Major-3).
//!
//! Exercises the minimal launch + preflight path against the **actual** WPE
//! backend inside the CI container. The test is `#[ignore]`-gated (a dev
//! laptop has no WPE device) and only runs with
//! `cargo test --features legacy_cpp -- --ignored`, exactly as the
//! `legacy_cpp` CI job does.

#![cfg(feature = "legacy_cpp")]

use webai_bridge_cxx::WebkitBridgeCxx;
use webai_protocol::BrowserVerb;

#[test]
#[ignore = "requires a WPE device / image; run with --ignored in the legacy_cpp CI job"]
fn real_device_launch_and_preflight() {
    let mut bridge = WebkitBridgeCxx::default();
    // Minimal launch against the real backend: must not produce the
    // structured CogLaunch error on a device that has WPE available.
    bridge.launch().expect("WPE device launch failed");
    assert!(bridge.is_launched());
    // Minimal preflight round-trip after launch.
    bridge
        .preflight_browser_request(BrowserVerb::Navigate)
        .expect("preflight failed on the real device");
}

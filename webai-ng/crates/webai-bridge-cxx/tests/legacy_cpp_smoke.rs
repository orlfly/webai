//! Real-device smoke test (task 33 / Kaneo review of #33).
//!
//! Feature-gated and `#[ignore]`d by default: run inside the WPE CI image
//! with
//!
//! ```text
//! cargo test -p webai-bridge-cxx --features legacy_cpp -- --ignored
//! ```
//!
//! The default (no-`legacy_cpp`) CI path never compiles this file, keeping
//! the portable Rust-only build free of WebKit dependencies (§4.7).
#![cfg(feature = "legacy_cpp")]

use webai_bridge_cxx::WebkitBridgeCxx;
use webai_protocol::BrowserVerb;

/// Minimal real-device path: launch the cog view, preflight one browser
/// verb, and confirm the bridge reports itself launched. Run with
/// `-- --ignored` inside the WPE image.
#[test]
#[ignore = "real-device smoke: requires legacy_cpp + cog/WPE environment"]
fn launch_and_preflight_smoke() {
    let mut bridge = WebkitBridgeCxx::default();
    bridge
        .launch()
        .expect("cog launch must succeed in the WPE image");
    assert!(bridge.is_launched());

    bridge
        .preflight_browser_request(BrowserVerb::Navigate)
        .expect("preflight of Navigate must succeed after launch");
}

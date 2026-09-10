//! WebkitBridge: FFI, load events, document-start injection.
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). Defines the public bridge
//! surface (ARCHITECTURE.md §4.6): `open`, `evaluate_javascript`,
//! `wait_for_load`, `inject_user_script`, `screenshot`, plus the order-sensitive
//! `BUNDLE_SCRIPT_ORDER` constant and the structured `WebkitError::CogLaunch`
//! returned in no-FFI (stub) environments. Real FFI wiring lands with
//! `webai-bridge-cxx` in M2.
//!
//! The page-side document-start bundle is embedded at compile time via
//! `include_str!` (ARCHITECTURE.md §10: single binary + `page-bundle/`, no
//! external script dependency at deploy time).

use std::sync::{Arc, Mutex};

/// Ordered list of page-side bundle scripts (ARCHITECTURE.md §4.6). Order is
/// significant and must be preserved exactly.
pub const BUNDLE_SCRIPT_ORDER: &[&str] = &[
    "bridge-client.js",
    "parser/index.js",
    "accessibility/index.js",
    "dom.js",
    "selector.js",
    "events.js",
    "network.js",
    "storage.js",
    "actions/navigate.js",
    "actions/history.js",
    "actions/interact.js",
    "actions/extract.js",
    "actions/screenshot.js",
    "actions/composite.js",
    "legacy/playwright-shim.js",
];

/// Fetch the source of a page-bundle script by its `BUNDLE_SCRIPT_ORDER` path.
///
/// Returns `None` if the path is not a known bundle entry. The content is
/// embedded at compile time, so this never touches the filesystem at runtime.
pub fn bundle_script(path: &str) -> Option<&'static str> {
    if !BUNDLE_SCRIPT_ORDER.contains(&path) {
        return None;
    }
    Some(match path {
        "bridge-client.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/bridge-client.js"
        )),
        "parser/index.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/parser/index.js"
        )),
        "accessibility/index.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/accessibility/index.js"
        )),
        "dom.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/dom.js"
        )),
        "selector.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/selector.js"
        )),
        "events.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/events.js"
        )),
        "network.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/network.js"
        )),
        "storage.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/storage.js"
        )),
        "actions/navigate.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/actions/navigate.js"
        )),
        "actions/history.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/actions/history.js"
        )),
        "actions/interact.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/actions/interact.js"
        )),
        "actions/extract.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/actions/extract.js"
        )),
        "actions/screenshot.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/actions/screenshot.js"
        )),
        "actions/composite.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/actions/composite.js"
        )),
        "legacy/playwright-shim.js" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../page-bundle/legacy/playwright-shim.js"
        )),
        _ => unreachable!("BUNDLE_SCRIPT_ORDER membership already checked"),
    })
}

/// Iterate the full bundle in `BUNDLE_SCRIPT_ORDER`, yielding `(path, source)`.
pub fn bundle_scripts() -> impl Iterator<Item = (&'static str, &'static str)> {
    BUNDLE_SCRIPT_ORDER.iter().map(|path| {
        (
            *path,
            bundle_script(path).expect("bundle entry must have embedded source"),
        )
    })
}

/// WebKit bridge errors (ARCHITECTURE.md §7).
#[derive(Debug, thiserror::Error)]
pub enum WebkitError {
    /// Returned when no FFI/cog environment is available (stub mode).
    #[error("cog launch failed: {0}")]
    CogLaunch(String),
    #[error("script evaluation timed out after {0}ms")]
    Timeout(u64),
    #[error("script error: {0}")]
    ScriptError(String),
    #[error("load failed: {0}")]
    LoadFailed(String),
}

/// Result of an evaluate call.
#[derive(Debug, Clone)]
pub struct EvaluateResult {
    pub json: serde_json::Value,
    /// Path to an auto-captured PNG screenshot, if any.
    pub screenshot_path: Option<String>,
}

/// Load status snapshot reported on `WEBKIT_LOAD_FINISHED`.
#[derive(Debug, Clone)]
pub struct LoadSnapshot {
    pub url: String,
    pub title: String,
    pub status: String,
}

/// Thread-safe handle to the WebKit view. All calls are serialized via an
/// internal `Mutex` (single-thread affinity per ARCHITECTURE.md §6).
#[derive(Debug, Clone)]
pub struct WebkitBridge {
    // In stub mode we carry no *mut view; M2 wires the cog/WPE pointer here.
    #[allow(dead_code)] // consumed by the FFI backend (M2)
    view: Arc<Mutex<Option<u64>>>,
    stub_mode: bool,
}

impl WebkitBridge {
    /// Construct a stub bridge. With FFI absent every operation returns a
    /// structured [`WebkitError::CogLaunch`].
    pub fn new() -> Self {
        Self {
            view: Arc::new(Mutex::new(None)),
            stub_mode: true,
        }
    }

    /// Navigate the view to `url` and wait for load.
    pub async fn open(&self, url: &str) -> Result<LoadSnapshot, WebkitError> {
        if self.stub_mode {
            return Err(WebkitError::CogLaunch(format!(
                "no FFI environment (open {url}); configure webai-bridge-cxx and run with the cog bridge"
            )));
        }
        Ok(LoadSnapshot {
            url: url.into(),
            title: String::new(),
            status: "finished".into(),
        })
    }

    /// Evaluate a JavaScript snippet, injecting `window.__webkit_args__`.
    pub async fn evaluate_javascript(
        &self,
        src: &str,
        timeout_ms: u64,
    ) -> Result<EvaluateResult, WebkitError> {
        let _ = (src, timeout_ms);
        if self.stub_mode {
            return Err(WebkitError::CogLaunch(
                "no FFI environment; cannot evaluate_javascript in stub mode".into(),
            ));
        }
        Ok(EvaluateResult {
            json: serde_json::Value::Null,
            screenshot_path: None,
        })
    }

    /// Register a document-start user script.
    pub async fn inject_user_script(&self, src: &str) -> Result<(), WebkitError> {
        let _ = src;
        if self.stub_mode {
            return Err(WebkitError::CogLaunch(
                "no FFI environment; cannot inject_user_script in stub mode".into(),
            ));
        }
        Ok(())
    }

    /// Capture a viewport PNG.
    pub async fn screenshot(&self) -> Result<Vec<u8>, WebkitError> {
        if self.stub_mode {
            return Err(WebkitError::CogLaunch(
                "no FFI environment; cannot screenshot in stub mode".into(),
            ));
        }
        Ok(Vec::new())
    }
}

impl Default for WebkitBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_script_order_is_stable_and_starts_with_bridge_client() {
        assert_eq!(BUNDLE_SCRIPT_ORDER.first(), Some(&"bridge-client.js"));
        assert_eq!(
            BUNDLE_SCRIPT_ORDER.last(),
            Some(&"legacy/playwright-shim.js")
        );
        assert!(BUNDLE_SCRIPT_ORDER.contains(&"actions/screenshot.js"));
    }

    #[test]
    fn bundle_has_exactly_15_entries_in_documented_order() {
        // ARCHITECTURE.md §4.6: 15 entries, order-sensitive.
        let expected = [
            "bridge-client.js",
            "parser/index.js",
            "accessibility/index.js",
            "dom.js",
            "selector.js",
            "events.js",
            "network.js",
            "storage.js",
            "actions/navigate.js",
            "actions/history.js",
            "actions/interact.js",
            "actions/extract.js",
            "actions/screenshot.js",
            "actions/composite.js",
            "legacy/playwright-shim.js",
        ];
        assert_eq!(BUNDLE_SCRIPT_ORDER.len(), 15, "must be exactly 15 entries");
        assert_eq!(
            BUNDLE_SCRIPT_ORDER, &expected,
            "order must match ARCHITECTURE.md §4.6"
        );
    }

    #[test]
    fn every_bundle_entry_has_nonempty_embedded_source() {
        // Compile-time include_str! guarantees the file exists; this asserts
        // the content is non-empty and the accessor returns it.
        for (path, src) in bundle_scripts() {
            assert!(!src.trim().is_empty(), "bundle entry {path} is empty");
        }
    }

    #[test]
    fn bundle_script_returns_none_for_unknown_path() {
        assert!(bundle_script("not/a/real/script.js").is_none());
    }

    #[test]
    fn bridge_client_installs_webkit_bridge_global() {
        // The bridge-client.js source must install `window.__webkitBridge`.
        // We assert on the embedded source (no FFI / no JS runtime needed).
        let src = bundle_script("bridge-client.js").expect("bridge-client embedded");
        assert!(
            src.contains("window.__webkitBridge"),
            "must install __webkitBridge"
        );
        assert!(src.contains("__webkitBridgeLoaded"), "must signal loaded");
    }

    #[test]
    fn playwright_shim_declares_its_dependencies() {
        // The shim must explicitly declare the building blocks it depends on.
        let src = bundle_script("legacy/playwright-shim.js").expect("shim embedded");
        assert!(src.contains("WebkitAiDom"), "shim depends on WebkitAiDom");
        assert!(
            src.contains("DEPENDENCIES"),
            "shim must declare dependencies"
        );
    }

    #[tokio::test]
    async fn stub_mode_returns_structured_cog_launch_error() {
        let bridge = WebkitBridge::new();
        let err = bridge.open("https://example.com").await.unwrap_err();
        match err {
            WebkitError::CogLaunch(msg) => {
                assert!(msg.contains("no FFI environment"));
            }
            other => panic!("expected CogLaunch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stub_evaluate_and_screenshot_also_report_cog_launch() {
        let bridge = WebkitBridge::new();
        assert!(matches!(
            bridge.evaluate_javascript("1+1", 100).await,
            Err(WebkitError::CogLaunch(_))
        ));
        assert!(matches!(
            bridge.screenshot().await,
            Err(WebkitError::CogLaunch(_))
        ));
        assert!(matches!(
            bridge.inject_user_script("x").await,
            Err(WebkitError::CogLaunch(_))
        ));
    }
}

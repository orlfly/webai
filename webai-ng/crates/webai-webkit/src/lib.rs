//! WebkitBridge: FFI, load events, document-start injection.
//!
//! Defines the public bridge surface (ARCHITECTURE.md §4.6): `open`,
//! `evaluate_javascript`, `wait_for_load`, `inject_user_script`, `screenshot`,
//! plus the order-sensitive `BUNDLE_SCRIPT_ORDER` constant.
//!
//! The bridge is backed by `webai-bridge-cxx` (the real cog/WPE FFI). When the
//! FFI backend is unavailable (no `legacy_cpp` feature, or launch fails), every
//! operation returns a structured [`WebkitError::CogLaunch`] so upper layers can
//! diagnose a missing environment instead of crashing. A canned-response test
//! injection path lets the dispatch chain be covered on a dev machine with no
//! WebKit.
//!
//! The page-side document-start bundle is embedded at compile time via
//! `include_str!` (ARCHITECTURE.md §10: single binary + `page-bundle/`, no
//! external script dependency at deploy time).

use std::sync::{Arc, Mutex};

use webai_bridge_cxx::{BridgeCxxError, LoadFinished, WebkitBridgeCxx};

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
        "bridge-client.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/bridge-client.js")),
        "parser/index.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/parser/index.js")),
        "accessibility/index.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/accessibility/index.js")),
        "dom.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/dom.js")),
        "selector.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/selector.js")),
        "events.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/events.js")),
        "network.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/network.js")),
        "storage.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/storage.js")),
        "actions/navigate.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/actions/navigate.js")),
        "actions/history.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/actions/history.js")),
        "actions/interact.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/actions/interact.js")),
        "actions/extract.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/actions/extract.js")),
        "actions/screenshot.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/actions/screenshot.js")),
        "actions/composite.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/actions/composite.js")),
        "legacy/playwright-shim.js" => include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../page-bundle/legacy/playwright-shim.js")),
        _ => unreachable!("BUNDLE_SCRIPT_ORDER membership already checked"),
    })
}

/// Iterate the full bundle in `BUNDLE_SCRIPT_ORDER`, yielding `(path, source)`.
pub fn bundle_scripts() -> impl Iterator<Item = (&'static str, &'static str)> {
    BUNDLE_SCRIPT_ORDER
        .iter()
        .map(|path| (*path, bundle_script(path).expect("bundle entry must have embedded source")))
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

impl From<BridgeCxxError> for WebkitError {
    fn from(e: BridgeCxxError) -> Self {
        match e {
            BridgeCxxError::CogLaunch(msg) => WebkitError::CogLaunch(msg),
            BridgeCxxError::Ffi(msg) => WebkitError::ScriptError(msg),
        }
    }
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

/// A oneshot load-wait subscription, woken by the `WEBKIT_LOAD_FINISHED`
/// trampoline.
struct LoadWaiter {
    tx: tokio::sync::oneshot::Sender<LoadSnapshot>,
}

/// Thread-safe handle to the WebKit view. All calls are serialized via an
/// internal `Mutex` (single-thread affinity per ARCHITECTURE.md §6).
#[derive(Clone)]
pub struct WebkitBridge {
    /// The real FFI backend (cog/WPE). `None` when no FFI environment.
    backend: Arc<Mutex<Option<WebkitBridgeCxx>>>,
    /// The last URI observed on `WEBKIT_LOAD_FINISHED`.
    last_load_uri: Arc<Mutex<Option<String>>>,
    /// Pending load-waiters to wake on the next `WEBKIT_LOAD_FINISHED`.
    load_waiters: Arc<Mutex<Vec<LoadWaiter>>>,
    /// Canned-response injection for tests (no FFI needed).
    canned: Arc<Mutex<Option<CannedBackend>>>,
}

/// A canned backend for tests: returns scripted responses without WebKit.
#[derive(Debug, Clone, Default)]
struct CannedBackend {
    evaluate_result: Option<serde_json::Value>,
    screenshot_png: Option<Vec<u8>>,
    load_events: Vec<LoadSnapshot>,
}

impl WebkitBridge {
    /// Construct a bridge. Attempts to launch the real FFI backend; if that
    /// fails (no `legacy_cpp` feature, or cog/WPE unavailable), the bridge
    /// operates in no-FFI mode where every operation returns
    /// [`WebkitError::CogLaunch`].
    pub fn new() -> Self {
        let mut backend = WebkitBridgeCxx::default();
        let launched = backend.launch().is_ok();
        let backend = if launched { Some(backend) } else { None };
        Self {
            backend: Arc::new(Mutex::new(backend)),
            last_load_uri: Arc::new(Mutex::new(None)),
            load_waiters: Arc::new(Mutex::new(Vec::new())),
            canned: Arc::new(Mutex::new(None)),
        }
    }

    /// Construct a bridge with a canned backend for tests (no FFI needed).
    #[cfg(test)]
    fn with_canned(canned: CannedBackend) -> Self {
        Self {
            backend: Arc::new(Mutex::new(None)),
            last_load_uri: Arc::new(Mutex::new(None)),
            load_waiters: Arc::new(Mutex::new(Vec::new())),
            canned: Arc::new(Mutex::new(Some(canned))),
        }
    }

    /// Whether the real FFI backend is available.
    pub fn is_ffi_available(&self) -> bool {
        self.backend.lock().unwrap().is_some()
    }

    /// Navigate the view to `url` and wait for load.
    pub async fn open(&self, url: &str) -> Result<LoadSnapshot, WebkitError> {
        // Canned path (tests).
        if let Some(canned) = self.canned.lock().unwrap().as_ref() {
            if let Some(ev) = canned.load_events.first() {
                return Ok(ev.clone());
            }
        }
        let backend = self.backend.lock().unwrap();
        let backend = backend.as_ref().ok_or_else(|| {
            WebkitError::CogLaunch(format!(
                "no FFI environment (open {url}); configure webai-bridge-cxx and run with the cog bridge"
            ))
        })?;
        backend.load_uri(url)?;
        // Wait for the load to finish (oneshot).
        self.wait_for_load(15_000).await
    }

    /// Evaluate a JavaScript snippet, injecting `window.__webkit_args__`.
    pub async fn evaluate_javascript(
        &self,
        src: &str,
        timeout_ms: u64,
    ) -> Result<EvaluateResult, WebkitError> {
        // Canned path (tests).
        if let Some(canned) = self.canned.lock().unwrap().as_ref() {
            if let Some(json) = canned.evaluate_result.clone() {
                return Ok(EvaluateResult {
                    json,
                    screenshot_path: None,
                });
            }
        }
        let backend = self.backend.lock().unwrap();
        let backend = backend.as_ref().ok_or_else(|| {
            WebkitError::CogLaunch(
                "no FFI environment; cannot evaluate_javascript in stub mode".into(),
            )
        })?;
        let payload = backend.evaluate(src, timeout_ms as u32)?;
        // The C++ side returns `{"ok":true,"value":<json>}` or
        // `{"ok":false,"error":"..."}`.
        let parsed: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|e| WebkitError::ScriptError(format!("invalid evaluate payload: {e}")))?;
        if parsed.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let msg = parsed
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
                .to_owned();
            return Err(WebkitError::ScriptError(msg));
        }
        let json = parsed.get("value").cloned().unwrap_or(serde_json::Value::Null);
        Ok(EvaluateResult {
            json,
            screenshot_path: None,
        })
    }

    /// Wait for the next `WEBKIT_LOAD_FINISHED`, or time out.
    pub async fn wait_for_load(&self, timeout_ms: u64) -> Result<LoadSnapshot, WebkitError> {
        // Canned path (tests): if a load event is queued, return it.
        if let Some(canned) = self.canned.lock().unwrap().as_ref() {
            if let Some(ev) = canned.load_events.first() {
                return Ok(ev.clone());
            }
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.load_waiters.lock().unwrap().push(LoadWaiter { tx });
        match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
            Ok(Ok(snapshot)) => Ok(snapshot),
            Ok(Err(_)) => Err(WebkitError::LoadFailed("load waiter dropped".into())),
            Err(_) => Err(WebkitError::Timeout(timeout_ms)),
        }
    }

    /// Register a document-start user script.
    pub async fn inject_user_script(&self, src: &str) -> Result<(), WebkitError> {
        // Canned path (tests): no-op success.
        if self.canned.lock().unwrap().is_some() {
            return Ok(());
        }
        let backend = self.backend.lock().unwrap();
        let backend = backend.as_ref().ok_or_else(|| {
            WebkitError::CogLaunch(
                "no FFI environment; cannot inject_user_script in stub mode".into(),
            )
        })?;
        backend.inject_user_script(src)?;
        Ok(())
    }

    /// Capture a viewport PNG.
    pub async fn screenshot(&self) -> Result<Vec<u8>, WebkitError> {
        // Canned path (tests).
        if let Some(canned) = self.canned.lock().unwrap().as_ref() {
            if let Some(png) = canned.screenshot_png.clone() {
                return Ok(png);
            }
        }
        let backend = self.backend.lock().unwrap();
        let backend = backend.as_ref().ok_or_else(|| {
            WebkitError::CogLaunch(
                "no FFI environment; cannot screenshot in stub mode".into(),
            )
        })?;
        // The bridge-cxx screenshot writes to a temp file; read it back.
        let path = std::env::temp_dir().join(format!("webai-shot-{}.png", std::process::id()));
        let path_str = path.to_str().ok_or_else(|| {
            WebkitError::ScriptError("temp path not UTF-8".into())
        })?;
        backend.screenshot(path_str)?;
        let bytes = std::fs::read(&path)
            .map_err(|e| WebkitError::ScriptError(format!("read screenshot: {e}")))?;
        let _ = std::fs::remove_file(&path);
        Ok(bytes)
    }

    /// Handle a `WEBKIT_LOAD_FINISHED` event from the FFI trampoline.
    fn on_load_finished(&self, event: LoadFinished) {
        *self.last_load_uri.lock().unwrap() = Some(event.uri.clone());
        let snapshot = LoadSnapshot {
            url: event.uri,
            title: event.title,
            status: "finished".into(),
        };
        let waiters = std::mem::take(&mut *self.load_waiters.lock().unwrap());
        for w in waiters {
            let _ = w.tx.send(snapshot.clone());
        }
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
        assert_eq!(BUNDLE_SCRIPT_ORDER, &expected, "order must match ARCHITECTURE.md §4.6");
    }

    #[test]
    fn every_bundle_entry_has_nonempty_embedded_source() {
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
        let src = bundle_script("bridge-client.js").expect("bridge-client embedded");
        assert!(src.contains("window.__webkitBridge"), "must install __webkitBridge");
        assert!(src.contains("__webkitBridgeLoaded"), "must signal loaded");
    }

    #[test]
    fn playwright_shim_declares_its_dependencies() {
        let src = bundle_script("legacy/playwright-shim.js").expect("shim embedded");
        assert!(src.contains("WebkitAiDom"), "shim depends on WebkitAiDom");
        assert!(src.contains("DEPENDENCIES"), "shim must declare dependencies");
    }

    #[tokio::test]
    async fn no_ffi_returns_structured_cog_launch_error() {
        // A bridge with no FFI backend and no canned backend.
        let bridge = WebkitBridge {
            backend: Arc::new(Mutex::new(None)),
            last_load_uri: Arc::new(Mutex::new(None)),
            load_waiters: Arc::new(Mutex::new(Vec::new())),
            canned: Arc::new(Mutex::new(None)),
        };
        let err = bridge.open("https://example.com").await.unwrap_err();
        match err {
            WebkitError::CogLaunch(msg) => assert!(msg.contains("no FFI environment")),
            other => panic!("expected CogLaunch, got {other:?}"),
        }
        assert!(matches!(
            bridge.evaluate_javascript("1+1", 100).await,
            Err(WebkitError::CogLaunch(_))
        ));
        assert!(matches!(bridge.screenshot().await, Err(WebkitError::CogLaunch(_))));
        assert!(matches!(
            bridge.inject_user_script("x").await,
            Err(WebkitError::CogLaunch(_))
        ));
    }

    #[tokio::test]
    async fn canned_evaluate_returns_scripted_json() {
        let bridge = WebkitBridge::with_canned(CannedBackend {
            evaluate_result: Some(serde_json::json!(2)),
            ..Default::default()
        });
        let res = bridge.evaluate_javascript("1+1", 1000).await.unwrap();
        assert_eq!(res.json, serde_json::json!(2));
    }

    #[tokio::test]
    async fn canned_screenshot_returns_png_magic() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let bridge = WebkitBridge::with_canned(CannedBackend {
            screenshot_png: Some(png.clone()),
            ..Default::default()
        });
        let bytes = bridge.screenshot().await.unwrap();
        assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    }

    #[tokio::test]
    async fn wait_for_load_times_out_without_event() {
        let bridge = WebkitBridge {
            backend: Arc::new(Mutex::new(None)),
            last_load_uri: Arc::new(Mutex::new(None)),
            load_waiters: Arc::new(Mutex::new(Vec::new())),
            canned: Arc::new(Mutex::new(None)),
        };
        let err = bridge.wait_for_load(50).await.unwrap_err();
        assert!(matches!(err, WebkitError::Timeout(50)));
    }

    #[tokio::test]
    async fn load_finished_wakes_oneshot_waiter() {
        let bridge = WebkitBridge {
            backend: Arc::new(Mutex::new(None)),
            last_load_uri: Arc::new(Mutex::new(None)),
            load_waiters: Arc::new(Mutex::new(Vec::new())),
            canned: Arc::new(Mutex::new(None)),
        };
        let bridge2 = bridge.clone();
        let handle = tokio::spawn(async move {
            bridge2.wait_for_load(1000).await.unwrap()
        });
        // Give the waiter a moment to register.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        bridge.on_load_finished(LoadFinished {
            uri: "https://example.com".into(),
            title: "Example".into(),
            status: 3,
        });
        let snapshot = handle.await.unwrap();
        assert_eq!(snapshot.url, "https://example.com");
        assert_eq!(*bridge.last_load_uri.lock().unwrap().as_deref().unwrap(), "https://example.com");
    }
}

//! Type-safe Rust ↔ C++ bridge over the cog / WPEBackend-fdo C++ wrappers.
//!
//! **This is the only crate in the workspace that may contain C++** (ARCHITECTURE.md
//! §4.7 / §10). All system-WebKit/C++ linking is gated behind the opt-in
//! `legacy_cpp` feature:
//!
//! - **default** (no `legacy_cpp`): pure Rust, compiles and tests with **no**
//!   system WebKit, and every operation returns a structured `CogLaunch` error so
//!   the upper layers can diagnose a missing FFI environment instead of crashing.
//! - **`legacy_cpp`**: pulls the `cxx` bridge to cog / libwpe. Only enabled on an
//!   actual WebKit build environment.

// The cxx-generated bridge module and the `rust::Fn` callback type trigger
// several clippy lints (unsafe-fn docs, complex types) that are inherent to
// the cxx codegen and not actionable in hand-written code. The reference
// implementation takes the same approach.
#![cfg_attr(feature = "legacy_cpp", allow(clippy::all))]

use webai_protocol::{BrowserToolResponse, BrowserVerb};

#[cfg(feature = "legacy_cpp")]
use std::os::raw::c_char;

/// Structured error when an FFI / cog environment is required but unavailable.
#[derive(Debug, thiserror::Error)]
pub enum BridgeCxxError {
    #[error("cog launch failed (legacy_cpp feature disabled): {0}")]
    CogLaunch(String),
    #[error("FFI call failed: {0}")]
    Ffi(String),
}

/// A load-finished notification reported from the C++ side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadFinished {
    pub uri: String,
    pub title: String,
    pub status: i32,
}

/// The C++ bridge facade (ARCHITECTURE.md §4.7).
///
/// In default (no-`legacy_cpp`) builds every method returns
/// [`BridgeCxxError::CogLaunch`]. With `legacy_cpp` the facade wraps the real
/// cog/WPE view lifecycle and API forwarding.
#[derive(Debug, Default)]
pub struct WebkitBridgeCxx {
    launched: bool,
    #[cfg(feature = "legacy_cpp")]
    view: Option<*mut ffi::WebkitView>,
}

impl WebkitBridgeCxx {
    /// Launch/attach the real cog view. Only succeeds when compiled with the
    /// `legacy_cpp` feature and run in an FFI-capable environment.
    pub fn launch(&mut self) -> Result<(), BridgeCxxError> {
        #[cfg(not(feature = "legacy_cpp"))]
        {
            let _ = &mut self.launched;
            Err(BridgeCxxError::CogLaunch(
                "compiled without the legacy_cpp feature; enable it and build with the cog/WPE toolchain"
                    .into(),
            ))
        }
        #[cfg(feature = "legacy_cpp")]
        {
            let view = unsafe { ffi::webkit_bridge_open() }
                .map_err(|e| BridgeCxxError::Ffi(e.to_string()))?;
            if view.is_null() {
                return Err(BridgeCxxError::Ffi(
                    "webkit_bridge_open returned null (cog/WPE init failed)".into(),
                ));
            }
            self.view = Some(view);
            self.launched = true;
            Ok(())
        }
    }

    /// Whether the view has been launched.
    pub fn is_launched(&self) -> bool {
        self.launched
    }

    /// Register a callback fired on `WEBKIT_LOAD_FINISHED`.
    pub fn set_load_callback<F>(&mut self, cb: F)
    where
        F: Fn(LoadFinished) + Send + 'static,
    {
        #[cfg(feature = "legacy_cpp")]
        {
            LOAD_CALLBACK.with(|c| *c.borrow_mut() = Some(Box::new(cb)));
            if let Some(view) = self.view {
                unsafe {
                    ffi::webkit_bridge_set_load_callback(view, load_trampoline);
                }
            }
        }
        #[cfg(not(feature = "legacy_cpp"))]
        {
            let _ = cb;
        }
    }

    /// Navigate the view to `uri`.
    pub fn load_uri(&self, uri: &str) -> Result<(), BridgeCxxError> {
        #[cfg(not(feature = "legacy_cpp"))]
        {
            let _ = uri;
            Err(BridgeCxxError::CogLaunch(
                "load_uri requires the legacy_cpp feature".into(),
            ))
        }
        #[cfg(feature = "legacy_cpp")]
        {
            let view = self
                .view
                .ok_or_else(|| BridgeCxxError::CogLaunch("view not launched".into()))?;
            let rc = unsafe { ffi::webkit_bridge_load_uri(view, uri) };
            if rc != 0 {
                return Err(BridgeCxxError::Ffi(format!(
                    "webkit_bridge_load_uri returned {rc}"
                )));
            }
            Ok(())
        }
    }

    /// Evaluate a JavaScript snippet, returning the JSON payload.
    pub fn evaluate(&self, js: &str, timeout_ms: u32) -> Result<String, BridgeCxxError> {
        #[cfg(not(feature = "legacy_cpp"))]
        {
            let _ = (js, timeout_ms);
            Err(BridgeCxxError::CogLaunch(
                "evaluate requires the legacy_cpp feature".into(),
            ))
        }
        #[cfg(feature = "legacy_cpp")]
        {
            let view = self
                .view
                .ok_or_else(|| BridgeCxxError::CogLaunch("view not launched".into()))?;
            let mut kind: i32 = 3;
            let mut payload = cxx::UniquePtr::null();
            let rc = unsafe {
                ffi::webkit_bridge_evaluate_javascript(
                    view,
                    js,
                    timeout_ms,
                    &mut kind,
                    &mut payload,
                )
            };
            if rc != 0 {
                return Err(BridgeCxxError::Ffi(format!(
                    "webkit_bridge_evaluate_javascript returned {rc}"
                )));
            }
            let payload_str = if payload.is_null() {
                String::new()
            } else {
                payload.to_string_lossy().into_owned()
            };
            match kind {
                0 => Ok(payload_str),
                1 => Err(BridgeCxxError::Ffi("evaluate timed out".into())),
                2 => Err(BridgeCxxError::Ffi(format!("script error: {payload_str}"))),
                _ => Err(BridgeCxxError::Ffi("evaluate returned invalid kind".into())),
            }
        }
    }

    /// Capture a PNG screenshot of the current view to `dest_path`.
    pub fn screenshot(&self, dest_path: &str) -> Result<(), BridgeCxxError> {
        #[cfg(not(feature = "legacy_cpp"))]
        {
            let _ = dest_path;
            Err(BridgeCxxError::CogLaunch(
                "screenshot requires the legacy_cpp feature".into(),
            ))
        }
        #[cfg(feature = "legacy_cpp")]
        {
            let view = self
                .view
                .ok_or_else(|| BridgeCxxError::CogLaunch("view not launched".into()))?;
            let rc = unsafe { ffi::webkit_bridge_screenshot(view, dest_path) };
            if rc != 0 {
                return Err(BridgeCxxError::Ffi(format!(
                    "webkit_bridge_screenshot returned {rc}"
                )));
            }
            Ok(())
        }
    }

    /// Compile a browser request into a two-phase script module (pure helper;
    /// actual script composing lives in `webai-script`).
    pub fn preflight_browser_request(&self, verb: BrowserVerb) -> Result<(), BridgeCxxError> {
        if !self.launched {
            return Err(BridgeCxxError::CogLaunch(format!(
                "preflight_browser_request({verb:?}) requires a launched view"
            )));
        }
        Ok(())
    }

    /// Placeholder dispatch returning a not-yet-implemented response shape.
    pub fn dispatch_stub(&self) -> Result<BrowserToolResponse, BridgeCxxError> {
        if !self.launched {
            return Err(BridgeCxxError::CogLaunch(
                "dispatch_stub requires a launched view".into(),
            ));
        }
        Ok(BrowserToolResponse {
            ok: true,
            result: Some(serde_json::json!({ "stub": true })),
            error: None,
            image_path: None,
        })
    }
}

impl Drop for WebkitBridgeCxx {
    fn drop(&mut self) {
        #[cfg(feature = "legacy_cpp")]
        {
            if let Some(view) = self.view.take() {
                unsafe { ffi::webkit_bridge_close(view) };
            }
        }
    }
}

// The cxx bridge block. Only compiled when `legacy_cpp` is enabled.
#[cfg(feature = "legacy_cpp")]
#[cxx::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("wrapper.h");

        type WebkitView;

        unsafe fn webkit_bridge_open() -> Result<*mut WebkitView>;
        unsafe fn webkit_bridge_close(view: *mut WebkitView);
        unsafe fn webkit_bridge_load_uri(view: *mut WebkitView, uri: &str) -> i32;
        unsafe fn webkit_bridge_inject_user_script(view: *mut WebkitView, script: &str) -> i32;
        unsafe fn webkit_bridge_evaluate_javascript(
            view: *mut WebkitView,
            script: &str,
            timeout_ms: u32,
            kind_out: &mut i32,
            payload_out: &mut UniquePtr<CxxString>,
        ) -> i32;
        unsafe fn webkit_bridge_resize(view: *mut WebkitView, width: u32, height: u32) -> i32;
        unsafe fn webkit_bridge_screenshot(view: *mut WebkitView, dest_path: &str) -> i32;
        unsafe fn webkit_bridge_set_load_callback(
            view: *mut WebkitView,
            callback: unsafe extern "C" fn(*const c_char, *const c_char, i32),
        );
    }
}

// Rust-side callback invoked by the C++ bridge on `WEBKIT_LOAD_FINISHED`.
// Stored in a thread-local so the C++ trampoline can route to it.
#[cfg(feature = "legacy_cpp")]
thread_local! {
    static LOAD_CALLBACK: std::cell::RefCell<Option<Box<dyn Fn(LoadFinished) + Send>>> =
        std::cell::RefCell::new(None);
}

/// Rust callback fired by the C++ side on `WEBKIT_LOAD_FINISHED`. cxx wraps
/// this safe Rust `fn` into a `rust::Fn` on the C++ side.
#[cfg(feature = "legacy_cpp")]
fn load_trampoline(uri: *const c_char, title: *const c_char, status: i32) {
    let uri = if uri.is_null() {
        String::new()
    } else {
        // SAFETY: the C++ side passes a valid NUL-terminated C string.
        unsafe { std::ffi::CStr::from_ptr(uri) }
            .to_string_lossy()
            .into_owned()
    };
    let title = if title.is_null() {
        String::new()
    } else {
        // SAFETY: the C++ side passes a valid NUL-terminated C string.
        unsafe { std::ffi::CStr::from_ptr(title) }
            .to_string_lossy()
            .into_owned()
    };
    let event = LoadFinished { uri, title, status };
    LOAD_CALLBACK.with(|c| {
        if let Some(cb) = c.borrow().as_ref() {
            cb(event);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests assert the no-FFI (default) behaviour: every operation
    // returns CogLaunch. They only apply when `legacy_cpp` is disabled.
    #[cfg(not(feature = "legacy_cpp"))]
    #[test]
    fn default_build_returns_cog_launch_error_on_launch() {
        let mut bridge = WebkitBridgeCxx::default();
        let err = bridge.launch().unwrap_err();
        match err {
            BridgeCxxError::CogLaunch(msg) => assert!(!msg.is_empty()),
            other => panic!("expected CogLaunch, got {other:?}"),
        }
    }

    #[cfg(not(feature = "legacy_cpp"))]
    #[test]
    fn preflight_requires_launched_view() {
        let bridge = WebkitBridgeCxx::default();
        assert!(matches!(
            bridge.preflight_browser_request(BrowserVerb::Click),
            Err(BridgeCxxError::CogLaunch(_))
        ));
    }

    #[test]
    fn not_launched_by_default() {
        assert!(!WebkitBridgeCxx::default().is_launched());
    }

    #[cfg(not(feature = "legacy_cpp"))]
    #[test]
    fn load_uri_returns_cog_launch_without_feature() {
        let bridge = WebkitBridgeCxx::default();
        assert!(matches!(
            bridge.load_uri("https://example.com"),
            Err(BridgeCxxError::CogLaunch(_))
        ));
    }

    #[cfg(not(feature = "legacy_cpp"))]
    #[test]
    fn evaluate_returns_cog_launch_without_feature() {
        let bridge = WebkitBridgeCxx::default();
        assert!(matches!(
            bridge.evaluate("1+1", 100),
            Err(BridgeCxxError::CogLaunch(_))
        ));
    }

    #[cfg(not(feature = "legacy_cpp"))]
    #[test]
    fn screenshot_returns_cog_launch_without_feature() {
        let bridge = WebkitBridgeCxx::default();
        assert!(matches!(
            bridge.screenshot("/tmp/x.png"),
            Err(BridgeCxxError::CogLaunch(_))
        ));
    }
}

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

use webai_protocol::{BrowserToolResponse, BrowserVerb};

/// Structured error when an FFI / cog environment is required but unavailable.
#[derive(Debug, thiserror::Error)]
pub enum BridgeCxxError {
    #[error("cog launch failed (legacy_cpp feature disabled): {0}")]
    CogLaunch(String),
    #[error("FFI call failed: {0}")]
    Ffi(String),
}

/// The C++ bridge facade (ARCHITECTURE.md §4.7).
///
/// In default (no-`legacy_cpp`) builds every method returns
/// [`BridgeCxxError::CogLaunch`], which the WebKit crate surfaces as
/// `WebkitError::CogLaunch`. The struct still exists so higher layers can be
/// assembled before M2 wires the real C++ implementation.
#[derive(Debug, Clone, Default)]
pub struct WebkitBridgeCxx {
    launched: bool,
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
            // Real cxx bridge lives in an M2 follow-up. Stub signals success so
            // the feature-gated path at least compiles and links.
            self.launched = true;
            Ok(())
        }
    }

    /// Whether the view has been launched.
    pub fn is_launched(&self) -> bool {
        self.launched
    }

    /// Compile a browser request into a two-phase script module (this is a
    /// pure helper; actual script composing lives in `webai-script`). In stub
    /// mode we surface CogLaunch until the real bridge is wired.
    pub fn preflight_browser_request(
        &self,
        verb: BrowserVerb,
    ) -> Result<(), BridgeCxxError> {
        if !self.launched {
            return Err(BridgeCxxError::CogLaunch(format!(
                "preflight_browser_request({verb:?}) requires a launched coy view"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_build_returns_cog_launch_error_on_launch() {
        let mut bridge = WebkitBridgeCxx::default();
        let err = bridge.launch().unwrap_err();
        match err {
            BridgeCxxError::CogLaunch(msg) => assert!(!msg.is_empty()),
            other => panic!("expected CogLaunch, got {other:?}"),
        }
    }

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
}

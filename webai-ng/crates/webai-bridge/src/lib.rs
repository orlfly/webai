//! webai-bridge: jcode_host equivalent — tool dispatch, screenshots, download, snapshot.
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). Defines the `dispatch` entry
//! point (ARCHITECTURE.md §4.8): it composes a script via `webai-script`, hands it
//! to `webai-webkit`, and merges the execute/verify payloads into a
//! `BrowserToolResponse`, automatically capturing a screenshot after non-read-only
//! actions. A real WebKit/FFI backend lands in M2.

use webai_protocol::{BrowserToolRequest, BrowserToolResponse};
use webai_script::{compose, ScriptError};
use webai_webkit::{EvaluateResult, WebkitBridge, WebkitError};

/// Structured error from the bridge layer.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("script composition failed: {0}")]
    Script(#[from] ScriptError),
    #[error("webkit bridge failed: {0}")]
    Webkit(#[from] WebkitError),
    #[error("missing required argument: {0}")]
    MissingArg(&'static str),
}

/// Whether an action mutates the page and therefore warrants an auto-screenshot.
fn wants_screenshot(verb: &webai_protocol::BrowserVerb) -> bool {
    use webai_protocol::BrowserVerb::{Click, Download, Drag, Evaluate, Fill, Hover, Navigate, PressKey};
    matches!(verb, Click | Download | Drag | Evaluate | Fill | Hover | Navigate | PressKey)
}

/// The bridge dispatcher (ARCHITECTURE.md §4.8).
pub struct Bridge {
    webkit: WebkitBridge,
}

impl Bridge {
    pub fn new(webkit: WebkitBridge) -> Self {
        Self { webkit }
    }

    pub fn webkit(&self) -> &WebkitBridge {
        &self.webkit
    }

    /// Dispatch a browser-tool request end to end.
    pub async fn dispatch(&self, req: &BrowserToolRequest) -> Result<BrowserToolResponse, BridgeError> {
        let module = compose(req)?;
        let result = self
            .webkit
            .evaluate_javascript(&module.execute_src, 30_000)
            .await?;
        self.merge(req, module.verify_src, result).await
    }

    /// Merge the execute/verify phase result into a response, capturing a
    /// screenshot after mutating actions.
    async fn merge(
        &self,
        req: &BrowserToolRequest,
        verify_src: String,
        eval: EvaluateResult,
    ) -> Result<BrowserToolResponse, BridgeError> {
        let ok = eval.json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut response = BrowserToolResponse {
            ok,
            result: Some(eval.json),
            error: None,
            image_path: None,
        };
        let _ = &verify_src;

        if ok && wants_screenshot(&req.verb) {
            // Screenshot capture is silent on failure (does not change the
            // operation's success semantics, ARCHITECTURE.md §4.8).
            if let Ok(png) = self.webkit.screenshot().await {
                if !png.is_empty() {
                    // Real persistence to a temp PNG lands with M2's FFI backend.
                    response.image_path = Some("/tmp/webai-stub.png".into());
                }
            }
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webai_protocol::BrowserVerb;
    use serde_json::json;

    #[test]
    fn wants_screenshot_marks_mutating_verbs() {
        use webai_protocol::BrowserVerb::*;
        assert!(wants_screenshot(&Click));
        assert!(wants_screenshot(&Navigate));
        assert!(wants_screenshot(&Fill));
        assert!(!wants_screenshot(&Screenshot));
        assert!(!wants_screenshot(&Snapshot));
        assert!(!wants_screenshot(&GetText));
    }

    #[tokio::test]
    async fn merge_sets_ok_from_execute_result() {
        let bridge = Bridge::new(WebkitBridge::new());
        let req = BrowserToolRequest {
            verb: BrowserVerb::GetText,
            args: json!({}),
        };
        let eval = EvaluateResult {
            json: json!({ "ok": true, "text": "hello" }),
            screenshot_path: None,
        };
        let resp = bridge
            .merge(&req, "export const verify=()=>({ok:true})".to_string(), eval)
            .await
            .unwrap();
        assert!(resp.ok);
    }
}

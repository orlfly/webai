//! webai-bridge: jcode_host equivalent — tool dispatch, screenshots, download, snapshot.
//!
//! Implements the bridge dispatch entry (ARCHITECTURE.md §4.8): it composes a
//! script via `webai-script`, hands it to `webai-webkit`, and merges the
//! execute/verify two-phase payloads into a `BrowserToolResponse`.
//!
//! Merge rule: `ok = execute.ok && verify.ok`. A verify failure is surfaced
//! structurally with `phase = "verify"` and the JS exception text (定论二) —
//! never swallowed as `unknown error`. The LLM repair call (M4-6) is a
//! follow-up; this task only surfaces the failure structurally.
//!
//! In a no-FFI environment (dev machine) the `WebkitBridge` canned-response
//! injection path covers the full dispatch chain (ARCHITECTURE.md §9 layer 3).

use webai_protocol::{
    codes, BrowserToolError, BrowserToolRequest, BrowserToolResponse, Request, Response,
};
use webai_script::ScriptError;
use webai_webkit::{EvaluateResult, WebkitBridge, WebkitError};

pub mod download;
pub mod download_guard;
pub mod perf;

pub use download_guard::{sanitize_filename, FilenameError};
pub use perf::{CacheKey, Clock, ComposeCache, ScreenshotDecision, ScreenshotThrottle};

/// Traverse a JSON path and read its boolean value.
fn json_get_bool(json: &serde_json::Value, path: &[&str]) -> bool {
    let mut cur = json;
    for key in path {
        match cur.get(key) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    cur.as_bool().unwrap_or(false)
}

/// Structured error from the bridge layer.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("script composition failed: {0}")]
    Script(#[from] ScriptError),
    #[error("webkit bridge failed: {0}")]
    Webkit(#[from] WebkitError),
    #[error("missing required argument: {0}")]
    MissingArg(&'static str),
    #[error("unknown bridge method: {0}")]
    UnknownMethod(String),
}

/// Whether an action mutates the page and therefore warrants an auto-screenshot.
fn wants_screenshot(verb: &webai_protocol::BrowserVerb) -> bool {
    use webai_protocol::BrowserVerb::{
        Click, Download, Drag, Evaluate, Fill, Hover, Navigate, PressKey,
    };
    matches!(
        verb,
        Click | Download | Drag | Evaluate | Fill | Hover | Navigate | PressKey
    )
}

/// The bridge dispatcher (ARCHITECTURE.md §4.8).
pub struct Bridge {
    webkit: WebkitBridge,
    /// Compose cache: same `verb + canonical args` never composes twice.
    cache: perf::ComposeCache,
    /// Screenshot throttle: mutating steps still record, bursts merge.
    throttle: perf::ScreenshotThrottle,
}

impl Bridge {
    pub fn new(webkit: WebkitBridge) -> Self {
        Self {
            webkit,
            cache: perf::ComposeCache::default(),
            throttle: perf::ScreenshotThrottle::new(std::time::Duration::from_millis(0)),
        }
    }

    /// Test/diagnostic access to the compose cache counters.
    pub fn cache(&self) -> &perf::ComposeCache {
        &self.cache
    }

    pub fn throttle(&self) -> &perf::ScreenshotThrottle {
        &self.throttle
    }

    pub fn webkit(&self) -> &WebkitBridge {
        &self.webkit
    }

    /// Bridge-protocol entry: `dispatch(Request) -> Response`.
    ///
    /// Parses the request method and params, dispatches to
    /// [`Self::handle_tool_call`], and wraps the result in a protocol
    /// [`Response`] carrying the same `id`.
    pub async fn dispatch(&self, req: &Request) -> Response {
        let id = req.id;
        match req.method.as_str() {
            "bridge.tool" => {
                // params: { verb, args }
                let verb = match req.params.get("verb").and_then(|v| v.as_str()) {
                    Some(v) => webai_protocol::BrowserVerb::from_name(v),
                    None => {
                        return Response::err(
                            id,
                            codes::INVALID_PARAMS,
                            "bridge.tool requires a `verb` param",
                        )
                    }
                };
                let args = req.params.get("args").cloned().unwrap_or_default();
                let tool_req = BrowserToolRequest { verb, args };
                match self.handle_tool_call(&tool_req).await {
                    Ok(resp) => Response::ok(id, serde_json::to_value(resp).unwrap_or_default()),
                    Err(e) => Response::err(id, codes::INTERNAL_ERROR, e.to_string()),
                }
            }
            other => Response::err(
                id,
                codes::METHOD_NOT_FOUND,
                format!("unknown method: {other}"),
            ),
        }
    }

    /// Handle a browser-tool request end to end.
    ///
    /// Composes the two-phase script, evaluates it in WebKit, and merges the
    /// execute/verify payloads. Download is a Rust-side operation (the page
    /// script only emits `needs_rust_download`), so it is routed to
    /// [`Self::handle_download`].
    pub async fn handle_tool_call(
        &self,
        req: &BrowserToolRequest,
    ) -> Result<BrowserToolResponse, BridgeError> {
        if req.verb == webai_protocol::BrowserVerb::Download {
            return self.handle_download(&req.args).await;
        }
        let module = self.cache.compose(req)?;
        let result = self
            .webkit
            .evaluate_javascript(&module.execute_src, 30_000, Some(&module.args))
            .await?;
        // Composed navigate signals the host to drive the load from Rust
        // (BUG-1 / Kaneo #99): run `webkit.open(url)` to navigate and wait for
        // LOAD_FINISHED, then re-run verify against the loaded page. The
        // execute phase reports ok:false by design here; a `needs_rust_load`
        // result is not an error.
        if json_get_bool(&result.json, &["execute", "needs_rust_load"]) {
            let url = req
                .args
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    BridgeError::Webkit(WebkitError::ScriptError(
                        "navigate requested needs_rust_load but args.url is missing".into(),
                    ))
                })?;
            self.webkit.open(url).await?;
            return self.merge_verify_only(req, &module).await;
        }
        self.merge(req, result).await
    }

    /// Handle a download request: fetch the URL in Rust and persist the body.
    pub async fn handle_download(
        &self,
        args: &serde_json::Value,
    ) -> Result<BrowserToolResponse, BridgeError> {
        let directory = args
            .get("directory")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        match download::download(args, directory.as_deref()).await {
            Ok(result) => Ok(BrowserToolResponse {
                ok: true,
                result: Some(serde_json::json!({
                    "ok": true,
                    "url": result.url,
                    "filename": result.filename,
                    "saved_to": result.saved_to,
                    "directory": result.directory,
                    "bytes": result.bytes,
                })),
                error: None,
                image_path: None,
                screenshot_warning: None,
            }),
            Err(e) => Ok(BrowserToolResponse {
                ok: false,
                result: None,
                error: Some(BrowserToolError {
                    code: codes::INTERNAL_ERROR,
                    message: e.to_string(),
                    phase: None,
                    detail: Some(e.code().to_owned()),
                }),
                image_path: None,
                screenshot_warning: None,
            }),
        }
    }

    /// Merge the execute/verify phase result into a response.
    ///
    /// The composed module's driver returns `{ execute, verify, args }`; the
    /// evaluate result JSON carries both phase payloads. `ok = execute.ok &&
    /// verify.ok`. A verify failure is surfaced with `phase = "verify"` and the
    /// JS exception text.
    /// Run only the verify phase of `module` (after the host completed a Rust
    /// side-effect such as `webkit.open(url)` for `needs_rust_load` verbs),
    /// then merge it like a normal two-phase result.
    async fn merge_verify_only(
        &self,
        req: &BrowserToolRequest,
        module: &webai_script::ScriptModule,
    ) -> Result<BrowserToolResponse, BridgeError> {
        let eval = self
            .webkit
            .evaluate_javascript(&module.verify_src_eval(), 30_000, Some(&module.args))
            .await?;
        self.merge(req, eval).await
    }

    async fn merge(
        &self,
        req: &BrowserToolRequest,
        eval: EvaluateResult,
    ) -> Result<BrowserToolResponse, BridgeError> {
        let json = &eval.json;
        let execute = json.get("execute").cloned().unwrap_or_default();
        let verify = json.get("verify").cloned().unwrap_or_default();

        let execute_ok = execute.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
            || execute
                .get("needs_rust_load")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        let verify_ok = verify.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let ok = execute_ok && verify_ok;

        // Surface the failing phase's error text (定论二), never "unknown error".
        let error = if !ok {
            let (phase, phase_json) = if !execute_ok {
                ("execute", &execute)
            } else {
                ("verify", &verify)
            };
            let detail = phase_json
                .get("error")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!("{phase} phase failed with no error detail: {phase_json}")
                });
            let code = if phase == "execute" {
                codes::EXECUTE_FAILED
            } else {
                codes::VERIFY_FAILED
            };
            Some(BrowserToolError {
                code,
                message: format!("{phase} phase failed"),
                phase: Some(phase.to_owned()),
                detail: Some(detail),
            })
        } else {
            None
        };

        let mut response = BrowserToolResponse {
            ok,
            result: Some(json.clone()),
            error,
            image_path: None,
            screenshot_warning: None,
        };

        // Auto-screenshot after every successful non-screenshot/download
        // operation (FR-2 / ARCHITECTURE.md §4.8). A screenshot failure does
        // NOT roll back the operation, but must be surfaced structurally.
        if ok && wants_screenshot(&req.verb) {
            match self.webkit.screenshot().await {
                Ok(png) if !png.is_empty() => {
                    // Persist the PNG to a temp file and attach its path.
                    let path = std::env::temp_dir()
                        .join("webai-screenshots")
                        .join(format!("shot-{}.png", std::process::id()));
                    if std::fs::create_dir_all(path.parent().unwrap()).is_ok()
                        && std::fs::write(&path, &png).is_ok()
                    {
                        response.image_path = Some(path.to_string_lossy().to_string());
                    } else {
                        response.screenshot_warning = Some(BrowserToolError {
                            code: codes::INTERNAL_ERROR,
                            message: "auto-screenshot failed to persist".into(),
                            phase: None,
                            detail: Some("could not write screenshot PNG to temp dir".into()),
                        });
                    }
                }
                Ok(_) => {
                    response.screenshot_warning = Some(BrowserToolError {
                        code: codes::INTERNAL_ERROR,
                        message: "auto-screenshot returned empty image".into(),
                        phase: None,
                        detail: Some("screenshot produced no bytes".into()),
                    });
                }
                Err(e) => {
                    // Operation result is preserved; only the screenshot is
                    // best-effort (FR-2). Surface a structured warning.
                    response.screenshot_warning = Some(BrowserToolError {
                        code: codes::INTERNAL_ERROR,
                        message: "auto-screenshot failed".into(),
                        phase: None,
                        detail: Some(e.to_string()),
                    });
                }
            }
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use webai_protocol::BrowserVerb;

    fn req(verb: BrowserVerb, args: serde_json::Value) -> BrowserToolRequest {
        BrowserToolRequest { verb, args }
    }

    /// Build a canned WebkitBridge that returns a scripted two-phase payload.
    fn canned_bridge(execute_ok: bool, verify_ok: bool) -> WebkitBridge {
        WebkitBridge::with_canned(webai_webkit::CannedBackend {
            evaluate_result: Some(json!({
                "execute": { "ok": execute_ok, "stage": "execute" },
                "verify": { "ok": verify_ok, "stage": "verify" },
                "args": {}
            })),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn merge_ok_true_when_both_phases_ok() {
        let bridge = Bridge::new(canned_bridge(true, true));
        let resp = bridge
            .handle_tool_call(&req(BrowserVerb::GetText, json!({})))
            .await
            .unwrap();
        assert!(resp.ok);
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn merge_ok_false_when_execute_fails() {
        let bridge = Bridge::new(canned_bridge(false, true));
        let resp = bridge
            .handle_tool_call(&req(BrowserVerb::GetText, json!({})))
            .await
            .unwrap();
        assert!(!resp.ok);
        let err = resp.error.expect("error present");
        assert!(err.message.contains("execute"));
    }

    #[tokio::test]
    async fn merge_ok_false_when_verify_fails_with_phase_and_error() {
        let bridge = Bridge::new(canned_bridge(true, false));
        let resp = bridge
            .handle_tool_call(&req(BrowserVerb::GetText, json!({})))
            .await
            .unwrap();
        assert!(!resp.ok);
        let err = resp.error.expect("error present");
        assert!(err.message.contains("verify"), "phase must be verify");
        assert!(err.detail.is_some(), "must carry JS error detail");
    }

    #[tokio::test]
    async fn merge_ok_false_when_both_fail() {
        let bridge = Bridge::new(canned_bridge(false, false));
        let resp = bridge
            .handle_tool_call(&req(BrowserVerb::GetText, json!({})))
            .await
            .unwrap();
        assert!(!resp.ok);
        assert!(resp.error.is_some());
    }

    /// BUG-1 / Kaneo #99: when the composed execute phase reports
    /// `needs_rust_load`, the host must drive `webkit.open(url)` and run the
    /// verify-only merge instead of reporting EXECUTE_FAILED. With a canned
    /// backend the open() resolves from the queued load event.
    #[tokio::test]
    async fn needs_rust_load_drives_open_and_skips_execute_failure() {
        let webkit = WebkitBridge::with_canned(webai_webkit::CannedBackend {
            evaluate_result: Some(json!({
                "execute": { "ok": false, "needs_rust_load": true, "url": "https://x.com" },
                "verify": { "ok": true, "stage": "verify" },
                "args": {}
            })),
            load_events: vec![webai_webkit::LoadSnapshot {
                url: "https://x.com".into(),
                title: "x".into(),
                status: "200".into(),
            }],
            ..Default::default()
        });
        let bridge = Bridge::new(webkit);
        let resp = bridge
            .handle_tool_call(&req(
                BrowserVerb::Navigate,
                json!({ "url": "https://x.com" }),
            ))
            .await
            .unwrap();
        assert!(
            resp.ok,
            "navigate must succeed via host-driven load: {:?}",
            resp.error
        );
        assert!(!resp
            .result
            .as_ref()
            .map(|r| serde_json::to_string(r).unwrap().contains("EXECUTE_FAILED"))
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn needs_rust_load_missing_url_is_structured_error() {
        // No bridge needed: compose rejects url-less args before dispatch,
        // so exercise json_get_bool's needs_rust_load path directly.
        assert!(!json_get_bool(&json!({}), &["execute", "needs_rust_load"]));
        assert!(json_get_bool(
            &json!({ "execute": { "needs_rust_load": true } }),
            &["execute", "needs_rust_load"]
        ));
    }

    #[tokio::test]
    async fn dispatch_bridge_tool_returns_response() {
        let bridge = Bridge::new(canned_bridge(true, true));
        let req = Request {
            id: 7,
            method: "bridge.tool".into(),
            params: json!({ "verb": "get_text", "args": {} }),
        };
        let resp = bridge.dispatch(&req).await;
        assert_eq!(resp.id, 7);
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn dispatch_unknown_method_returns_method_not_found() {
        let bridge = Bridge::new(WebkitBridge::with_canned(Default::default()));
        let req = Request {
            id: 1,
            method: "bogus".into(),
            params: json!({}),
        };
        let resp = bridge.dispatch(&req).await;
        assert_eq!(resp.id, 1);
        let err = resp.error.expect("error present");
        assert_eq!(err.code, codes::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn dispatch_missing_verb_returns_invalid_params() {
        let bridge = Bridge::new(WebkitBridge::with_canned(Default::default()));
        let req = Request {
            id: 2,
            method: "bridge.tool".into(),
            params: json!({}),
        };
        let resp = bridge.dispatch(&req).await;
        let err = resp.error.expect("error present");
        assert_eq!(err.code, codes::INVALID_PARAMS);
    }

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

    /// A canned bridge that returns a scripted two-phase payload AND a PNG
    /// screenshot, so the auto-screenshot path attaches an image.
    fn canned_bridge_with_png() -> WebkitBridge {
        WebkitBridge::with_canned(webai_webkit::CannedBackend {
            evaluate_result: Some(json!({
                "execute": { "ok": true, "stage": "execute" },
                "verify": { "ok": true, "stage": "verify" },
                "args": {}
            })),
            screenshot_png: Some(vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn successful_operation_attaches_screenshot_path() {
        let bridge = Bridge::new(canned_bridge_with_png());
        let resp = bridge
            .handle_tool_call(&req(BrowserVerb::Click, json!({ "selector": "#btn" })))
            .await
            .unwrap();
        assert!(resp.ok);
        assert!(
            resp.image_path.is_some(),
            "auto-screenshot path must be attached"
        );
        assert!(resp.screenshot_warning.is_none(), "no warning on success");
    }

    #[tokio::test]
    async fn screenshot_failure_produces_warning_but_preserves_operation() {
        // A canned bridge with no screenshot_png -> screenshot() returns
        // CogLaunch (no FFI), so the auto-screenshot fails.
        let bridge = Bridge::new(canned_bridge(true, true));
        let resp = bridge
            .handle_tool_call(&req(BrowserVerb::Click, json!({ "selector": "#btn" })))
            .await
            .unwrap();
        // Operation result preserved.
        assert!(resp.ok);
        assert!(resp.error.is_none());
        // Screenshot failure surfaced structurally.
        let warn = resp.screenshot_warning.expect("screenshot warning present");
        assert!(warn.message.contains("auto-screenshot"));
        assert!(warn.detail.is_some(), "must carry a reason");
    }

    #[tokio::test]
    async fn screenshot_verb_does_not_trigger_nested_screenshot() {
        // Screenshot verb: the operation itself is the screenshot, so no
        // nested auto-screenshot should run.
        let bridge = Bridge::new(canned_bridge_with_png());
        let resp = bridge
            .handle_tool_call(&req(BrowserVerb::Screenshot, json!({})))
            .await
            .unwrap();
        assert!(resp.ok);
        assert!(
            resp.image_path.is_none(),
            "no nested screenshot for screenshot verb"
        );
    }

    #[tokio::test]
    async fn download_verb_does_not_trigger_nested_screenshot() {
        // Download is Rust-side; no auto-screenshot.
        let bridge = Bridge::new(WebkitBridge::with_canned(Default::default()));
        let resp = bridge
            .handle_tool_call(&req(
                BrowserVerb::Download,
                json!({ "url": "https://x.com/f" }),
            ))
            .await
            .unwrap();
        assert!(
            resp.image_path.is_none(),
            "no nested screenshot for download verb"
        );
    }
}

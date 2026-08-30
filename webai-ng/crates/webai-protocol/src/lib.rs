//! Shared wire types for the webai-ng AI browser.
//!
//! This crate is the **zero-dependency, serde-only** protocol layer described in
//! `docs/ARCHITECTURE.md` §4.1. It contains no logic and no I/O: it exists so
//! that every other crate shares one canonical set of wire types (Request /
//! Response, browser-tool requests, error codes, and session events) and cannot
//! drift into type divergence.
//!
//! The wire shapes follow the bridge-protocol and browser-tool convention defined
//! in `docs/architecture/ARCHITECTURE.md`.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

// ---------------------------------------------------------------------------
// Bridge protocol envelope (Request / Response)
// ---------------------------------------------------------------------------

/// Top-level bridge-protocol envelope.
///
/// Messages use an explicit `type` tag (`request` / `response` / `event`) so a
/// single framing channel can carry all three kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

/// An outbound request: asks the host to do something.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Monotonically increasing id. Pair every `Request` with exactly one
    /// `Response` that carries the same `id`.
    pub id: u64,
    /// Method name, e.g. `bridge.inject`, `bridge.evaluate`, `bridge.tool`.
    pub method: String,
    #[serde(default)]
    pub params: Json,
}

/// The host's answer to a [`Request`]: either a `result` or an `error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    /// Present on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Json>,
    /// Present on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorPayload>,
}

impl Response {
    /// Build a successful response.
    pub fn ok(id: u64, result: Json) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Build a failing response carrying a stable error code.
    pub fn err(id: u64, code: i32, message: impl Into<String>) -> Self {
        Self {
            id,
            result: None,
            error: Some(ErrorPayload {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// A host-initiated push (navigation completion, DOM mutation, lifecycle).
/// Never carries an `id` and is safe to drop under backpressure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Method name, e.g. `page.load`, `dom.mutation`.
    pub method: String,
    #[serde(default)]
    pub data: Json,
}

/// Structured error body attached to a failed [`Response`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    /// One of [`codes`], or a domain-specific code in the reserved range.
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Json>,
}

// ---------------------------------------------------------------------------
// Browser tool protocol
// ---------------------------------------------------------------------------

/// One browser-tool invocation the agent loop wants executed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserToolRequest {
    /// What to do in the browser. Maps onto a two-phase JavaScript snippet.
    pub verb: BrowserVerb,
    /// Free-form arguments. The shape depends on `verb`.
    #[serde(default)]
    pub args: Json,
}

/// The 13 browser verbs (`docs/ARCHITECTURE.md` §4.1).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BrowserVerb {
    /// `args.url` — drive `window.location.href` to the URL.
    Navigate,
    /// `args.selector` — dispatch a click on the element.
    Click,
    /// `args.selector`, `args.value` — fill an input.
    Fill,
    /// `args.selector` — hover an element.
    Hover,
    /// `args.source`, `args.target` — drag between two elements.
    Drag,
    /// `args.key`, `args.selector?` — press a keyboard key.
    PressKey,
    /// `args.script` — evaluate an arbitrary JS snippet.
    Evaluate,
    /// Capture a viewport screenshot (base64 PNG).
    Screenshot,
    /// Read the rendered accessibility tree.
    AccessibilityTree,
    /// Read the visible text on the current page.
    GetText,
    /// Read the visible HTML on the current page.
    GetHtml,
    /// `args.url`, `args.filename?`, `args.directory?` — fetch over HTTP and
    /// persist the body to disk (a Rust-side operation).
    Download,
    /// One-shot snapshot: `location.href` / `document.title` / `readyState` /
    /// visible text.
    Snapshot,
}

impl BrowserVerb {
    /// Parse a canonical name ("navigate", "click", ...).
    /// Unknown names fall back to [`BrowserVerb::Evaluate`] so remembered
    /// scripts still execute.
    pub fn from_name(name: &str) -> Self {
        match name {
            "navigate" => Self::Navigate,
            "click" => Self::Click,
            "fill" | "type_text" => Self::Fill,
            "hover" => Self::Hover,
            "drag" => Self::Drag,
            "press_key" => Self::PressKey,
            "evaluate" => Self::Evaluate,
            "screenshot" => Self::Screenshot,
            "accessibility_tree" => Self::AccessibilityTree,
            "extract_text" | "get_text" => Self::GetText,
            "extract_html" | "get_html" => Self::GetHtml,
            "download" => Self::Download,
            "snapshot" => Self::Snapshot,
            _ => Self::Evaluate,
        }
    }

    /// Canonical `command` string (inverse of [`Self::from_name`]).
    pub fn canonical_name(&self) -> &'static str {
        match self {
            Self::Navigate => "navigate",
            Self::Click => "click",
            Self::Fill => "fill",
            Self::Hover => "hover",
            Self::Drag => "drag",
            Self::PressKey => "press_key",
            Self::Evaluate => "evaluate",
            Self::Screenshot => "screenshot",
            Self::AccessibilityTree => "accessibility_tree",
            Self::GetText => "extract_text",
            Self::GetHtml => "extract_html",
            Self::Download => "download",
            Self::Snapshot => "snapshot",
        }
    }
}

/// Result of a single browser-tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserToolResponse {
    /// True iff both the execute and verify phases succeeded.
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Json>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<BrowserToolError>,
    /// Path to the auto-captured screenshot, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_path: Option<String>,
}

/// Structured error raised by a browser-tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserToolError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Session events (Step / Done / Error)
// ---------------------------------------------------------------------------

/// A step the agent loop streams to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStep {
    /// The tool name that produced this step (e.g. `browser.click`).
    pub tool_name: String,
    /// The model's short thought/observation for this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<String>,
    /// Path to a rendered image (screenshot) for this step, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// True when the step executed a remembered script instead of composing
    /// a new one (`docs/ARCHITECTURE.md` §1.4 / §4.1).
    #[serde(default)]
    pub reused_script: bool,
}

/// Terminal state of the agent loop, carried by [`SessionEvent::Done`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    /// Machine status: "idle", "running", "done" (structured for wire stability).
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// An event the agent loop pushes to the TUI / ACP frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    /// One agent-loop step completed.
    Step { step: AgentStep },
    /// The loop finished for the current request.
    Done { state: AgentState },
    /// The loop failed to run.
    Error { message: String },
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

/// Stable error codes for the bridge protocol (`docs/ARCHITECTURE.md` §4.1).
pub mod codes {
    /// Malformed JSON in an incoming message.
    pub const PARSE_ERROR: i32 = -32700;
    /// Request is not a well-formed protocol object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// Method name is not registered.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Params do not match the method's signature.
    pub const INVALID_PARAMS: i32 = -32602;
    /// Unrecoverable server error.
    pub const INTERNAL_ERROR: i32 = -32603;
    /// Reserved lower bound for domain-specific codes.
    pub const DOMAIN_MIN: i32 = -32099;
    /// `bridge.wait_for_load` timed out before `WEBKIT_LOAD_FINISHED`.
    pub const LOAD_TIMEOUT: i32 = -32001;
    /// Path rejected by the filesystem-tool allowlist.
    pub const PATH_NOT_ALLOWED: i32 = -32002;
    /// Reserved upper bound for domain-specific codes.
    pub const DOMAIN_MAX: i32 = -32000;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_tool_request_roundtrips() {
        let req = BrowserToolRequest {
            verb: BrowserVerb::Click,
            args: serde_json::json!({ "selector": "#login" }),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: BrowserToolRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
        assert!(json.contains("\"verb\":\"click\""));
    }

    #[test]
    fn all_thirteen_verbs_serialize_snake_case() {
        // (variant, wire name under #[serde(rename_all="snake_case")], storage canonical name)
        let cases = [
            (BrowserVerb::Navigate, "navigate", "navigate"),
            (BrowserVerb::Click, "click", "click"),
            (BrowserVerb::Fill, "fill", "fill"),
            (BrowserVerb::Hover, "hover", "hover"),
            (BrowserVerb::Drag, "drag", "drag"),
            (BrowserVerb::PressKey, "press_key", "press_key"),
            (BrowserVerb::Evaluate, "evaluate", "evaluate"),
            (BrowserVerb::Screenshot, "screenshot", "screenshot"),
            (BrowserVerb::AccessibilityTree, "accessibility_tree", "accessibility_tree"),
            (BrowserVerb::GetText, "get_text", "extract_text"),
            (BrowserVerb::GetHtml, "get_html", "extract_html"),
            (BrowserVerb::Download, "download", "download"),
            (BrowserVerb::Snapshot, "snapshot", "snapshot"),
        ];
        assert_eq!(cases.len(), 13, "13 BrowserVerb variants required");
        for (verb, wire, storage) in cases {
            // The serde wire name is pure snake_case of the variant.
            let json = serde_json::to_string(&verb).unwrap();
            assert_eq!(json, format!("\"{wire}\""), "wire name for {verb:?}");
            // The storage canonical name is a separate, stable legacy alias
            // used by script-memory entries (extract_text/extract_html).
            assert_eq!(verb.canonical_name(), storage);
            assert_eq!(BrowserVerb::from_name(storage), verb);
            // Regression: the serde wire name must also map back to itself, not
            // fall through to Evaluate. This was broken for get_text/get_html.
            assert_eq!(
                BrowserVerb::from_name(wire),
                verb,
                "from_name({wire:?}) must map back to {verb:?}"
            );
        }
    }

    #[test]
    fn from_name_maps_wire_names_for_text_and_html() {
        // The exact regression fixed for M-1 review (#23):
        // from_name("get_text") / from_name("get_html") previously returned
        // Evaluate, breaking script-memory replay semantics.
        assert_eq!(BrowserVerb::from_name("get_text"), BrowserVerb::GetText);
        assert_eq!(BrowserVerb::from_name("get_html"), BrowserVerb::GetHtml);
        // The legacy storage aliases still work unchanged.
        assert_eq!(BrowserVerb::from_name("extract_text"), BrowserVerb::GetText);
        assert_eq!(BrowserVerb::from_name("extract_html"), BrowserVerb::GetHtml);
        // Unknown names still fall back to Evaluate (escape hatch).
        assert_eq!(BrowserVerb::from_name("bogus_verb"), BrowserVerb::Evaluate);
    }

    #[test]
    fn request_and_response_roundtrip_through_envelope() {
        let envelope = Message::Request(Request {
            id: 42,
            method: "bridge.evaluate".into(),
            params: serde_json::json!({ "script": "1 + 2" }),
        });
        let line = serde_json::to_string(&envelope).unwrap();
        let parsed: Message = serde_json::from_str(&line).unwrap();
        match parsed {
            Message::Request(req) => {
                assert_eq!(req.id, 42);
                assert_eq!(req.method, "bridge.evaluate");
            }
            other => panic!("expected Request, got {other:?}"),
        }
    }

    #[test]
    fn error_response_carries_stable_code() {
        let resp = Response::err(7, codes::METHOD_NOT_FOUND, "browser.click is not a method");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"code\":-32601"));
        assert!(json.contains("browser.click"));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn success_response_omits_error_field() {
        let resp = Response::ok(1, serde_json::json!({ "title": "Example" }));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn notification_has_no_id() {
        let envelope = Message::Notification(Notification {
            method: "page.load".into(),
            data: serde_json::json!({ "url": "https://example.com" }),
        });
        let json = serde_json::to_string(&envelope).unwrap();
        let value: Json = serde_json::from_str(&json).unwrap();
        assert!(value.get("id").is_none());
        assert_eq!(value.get("method").and_then(|m| m.as_str()), Some("page.load"));
    }

    #[test]
    fn session_event_steps_carry_image_and_reused_flag() {
        let ev = SessionEvent::Step {
            step: AgentStep {
                tool_name: "browser.snapshot".into(),
                observation: Some("page loaded".into()),
                image: Some("/tmp/shot.png".into()),
                reused_script: true,
            },
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: SessionEvent = serde_json::from_str(&json).unwrap();
        match back {
            SessionEvent::Step { step } => {
                assert_eq!(step.tool_name, "browser.snapshot");
                assert_eq!(step.image.as_deref(), Some("/tmp/shot.png"));
                assert!(step.reused_script);
            }
            other => panic!("expected Step, got {other:?}"),
        }
    }

    #[test]
    fn session_event_done_and_error_roundtrip() {
        let done = SessionEvent::Done {
            state: AgentState {
                status: "done".into(),
                message: Some("finished".into()),
            },
        };
        let done_json = serde_json::to_string(&done).unwrap();
        assert!(matches!(serde_json::from_str::<SessionEvent>(&done_json).unwrap(), SessionEvent::Done { .. }));

        let err = SessionEvent::Error {
            message: "boom".into(),
        };
        let err_json = serde_json::to_string(&err).unwrap();
        assert!(matches!(serde_json::from_str::<SessionEvent>(&err_json).unwrap(), SessionEvent::Error { .. }));
    }

    #[test]
    fn browser_tool_response_omits_image_when_absent() {
        let resp = BrowserToolResponse {
            ok: true,
            result: Some(serde_json::json!({ "text": "hi" })),
            error: None,
            image_path: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("image_path"));
        assert!(json.contains("\"ok\":true"));
    }
}

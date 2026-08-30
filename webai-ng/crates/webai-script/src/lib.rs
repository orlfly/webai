//! `script_author`: verb -> two-phase JavaScript template composition.
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). This crate is the pure,
//! I/O-free authoring layer (ARCHITECTURE.md §4.5): it composes a
//! `ScriptModule { execute_src, verify_src, args }` from a `BrowserToolRequest`
//! and has NO filesystem or network access. Real template vocabularies land in a
//! later milestone; for now every verb composes a minimal valid module so the
//! dispatch chain upstream can be exercised.

use webai_protocol::{BrowserToolRequest, BrowserVerb};

/// A composed two-phase script module (ARCHITECTURE.md §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptModule {
    /// `execute_<verb>(args)` body. Reads `window.__webkit_args__`.
    pub execute_src: String,
    /// `verify_<verb>(args)` body. Confirms the action took effect.
    pub verify_src: String,
    /// The arguments to re-inject at `window.__webkit_args__`.
    pub args: String,
}

/// Errors from composing a script module.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("unsupported verb: {0:?}")]
    UnsupportedVerb(BrowserVerb),
    #[error("missing required argument `{0}` for verb {1:?}")]
    MissingArg(&'static str, BrowserVerb),
}

/// Compose a two-phase script module for a browser-tool request.
///
/// This is the single entry point called by `webai-bridge` before it hands the
/// script to WebKit. It must remain a pure function (no I/O).
pub fn compose(request: &BrowserToolRequest) -> Result<ScriptModule, ScriptError> {
    match request.verb {
        BrowserVerb::Evaluate => {
            let script = request
                .args
                .get("script")
                .and_then(|v| v.as_str())
                .ok_or(ScriptError::MissingArg("script", BrowserVerb::Evaluate))?;
            Ok(ScriptModule {
                execute_src: format!(
                    "export const execute = function(args) {{ return eval(atob(`{b64}`)); }};",
                    b64 = base64(script)
                ),
                verify_src: "export const verify = function() { return { ok: true }; };".into(),
                args: request.args.to_string(),
            })
        }
        BrowserVerb::Navigate | BrowserVerb::Click | BrowserVerb::Fill | BrowserVerb::Hover
        | BrowserVerb::Drag | BrowserVerb::PressKey | BrowserVerb::Screenshot
        | BrowserVerb::AccessibilityTree | BrowserVerb::GetText | BrowserVerb::GetHtml
        | BrowserVerb::Download | BrowserVerb::Snapshot => Ok(stub_module(&request.verb, &request.args)),
    }
}

/// Build a generic stub module for every non-evaluate verb. Later milestones
/// replace this with real per-verb templates (`execute_<verb>`/`verify_<verb>`).
fn stub_module(verb: &BrowserVerb, args: &serde_json::Value) -> ScriptModule {
    ScriptModule {
        execute_src: format!(
            "export const execute = function(args) {{ \
             window.__webkit_result__ = {{ ok: true, verb: \"{name}\" }}; return window.__webkit_result__; }};",
            name = verb.canonical_name()
        ),
        verify_src: "export const verify = function() { return { ok: true }; };".into(),
        args: args.to_string(),
    }
}

/// Minimal base64 encoder (avoids a dependency in this pure crate).
fn base64(input: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    fn enc(idx: usize) -> char {
        ALPHABET[idx & 63] as char
    }
    let bytes = input.as_bytes();
    let chunks = bytes.len().div_ceil(3);
    let mut out = String::with_capacity(chunks * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(enc((n >> 18) as usize));
        out.push(enc((n >> 12) as usize));
        if chunk.len() > 1 {
            out.push(enc((n >> 6) as usize));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(enc(n as usize));
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use webai_protocol::BrowserVerb;
    use serde_json::json;

    #[test]
    fn evaluate_composes_module_and_roundtrips_script() {
        let req = BrowserToolRequest {
            verb: BrowserVerb::Evaluate,
            args: json!({ "script": "1 + 2" }),
        };
        let m = compose(&req).unwrap();
        assert!(m.execute_src.contains("export const execute"));
        assert!(m.verify_src.contains("verify"));
        // The script text must survive the base64 round-trip.
        let decoded = decode_base64(extract_b64(&m.execute_src));
        assert_eq!(decoded, "1 + 2");
    }

    #[test]
    fn evaluate_missing_script_is_an_error() {
        let req = BrowserToolRequest {
            verb: BrowserVerb::Evaluate,
            args: json!({}),
        };
        assert!(matches!(compose(&req), Err(ScriptError::MissingArg("script", BrowserVerb::Evaluate))));
    }

    #[test]
    fn every_non_evaluate_verb_composes_a_stub_module() {
        let verbs = [
            BrowserVerb::Navigate,
            BrowserVerb::Click,
            BrowserVerb::Fill,
            BrowserVerb::Hover,
            BrowserVerb::Drag,
            BrowserVerb::PressKey,
            BrowserVerb::Screenshot,
            BrowserVerb::AccessibilityTree,
            BrowserVerb::GetText,
            BrowserVerb::GetHtml,
            BrowserVerb::Download,
            BrowserVerb::Snapshot,
        ];
        for verb in verbs {
            let req = BrowserToolRequest {
                verb,
                args: json!({}),
            };
            let m = compose(&req).expect("stub compose must not fail");
            assert!(m.execute_src.contains(&format!("\"{}\"", verb.canonical_name())));
        }
    }

    #[test]
    fn compose_is_pure() {
        let req_a = BrowserToolRequest {
            verb: BrowserVerb::GetText,
            args: json!({}),
        };
        let a = compose(&req_a).unwrap();
        let b = compose(&req_a).unwrap();
        assert_eq!(a.execute_src, b.execute_src);
        assert_eq!(a.verify_src, b.verify_src);
    }

    fn extract_b64(src: &str) -> &str {
        // src has `atob(\`...\`)`; pull between backticks.
        let start = src.find('`').unwrap() + 1;
        let end = src[start..].find('`').unwrap() + start;
        &src[start..end]
    }

    fn decode_base64(input: &str) -> String {
        let mut bytes = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0u32;
        for c in input.chars() {
            let v = match c {
                'A'..='Z' => c as u32 - 'A' as u32,
                'a'..='z' => c as u32 - 'a' as u32 + 26,
                '0'..='9' => c as u32 - '0' as u32 + 52,
                '+' => 62,
                '/' => 63,
                '=' => break,
                _ => continue,
            };
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                bytes.push((buf >> bits) as u8);
            }
        }
        String::from_utf8(bytes).unwrap()
    }
}

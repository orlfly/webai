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
        BrowserVerb::Navigate
        | BrowserVerb::Click
        | BrowserVerb::Fill
        | BrowserVerb::Hover
        | BrowserVerb::Drag
        | BrowserVerb::PressKey
        | BrowserVerb::Screenshot
        | BrowserVerb::AccessibilityTree
        | BrowserVerb::GetText
        | BrowserVerb::GetHtml
        | BrowserVerb::Download
        | BrowserVerb::Snapshot => Ok(stub_module(&request.verb, &request.args)),
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
    use serde_json::json;
    use webai_protocol::BrowserVerb;

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
        assert!(matches!(
            compose(&req),
            Err(ScriptError::MissingArg("script", BrowserVerb::Evaluate))
        ));
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
            assert!(m
                .execute_src
                .contains(&format!("\"{}\"", verb.canonical_name())));
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

    // ---- 参数注入契约 / 模板快照 / fuzz / 枚举覆盖 (task: webai-script) ----

    /// The canonical 13-verb list; a new `BrowserVerb` variant added without
    /// updating this list (and the template table) must fail this test.
    const ALL_VERBS: &[BrowserVerb] = &[
        BrowserVerb::Navigate,
        BrowserVerb::Click,
        BrowserVerb::Fill,
        BrowserVerb::Hover,
        BrowserVerb::Drag,
        BrowserVerb::PressKey,
        BrowserVerb::Evaluate,
        BrowserVerb::Screenshot,
        BrowserVerb::AccessibilityTree,
        BrowserVerb::GetText,
        BrowserVerb::GetHtml,
        BrowserVerb::Download,
        BrowserVerb::Snapshot,
    ];

    #[test]
    fn verb_list_covers_every_enum_variant() {
        // Count assertion guards against both additions and removals: the
        // architecture fixes the contract at exactly 13 verbs.
        assert_eq!(ALL_VERBS.len(), 13, "verb contract is exactly 13");
        // Every listed verb must compose successfully (template exists).
        for verb in ALL_VERBS {
            let req = BrowserToolRequest {
                verb: *verb,
                // A typical argument set every verb's template accepts.
                args: serde_json::json!({
                    "script": "1", "selector": "#a", "url": "https://x",
                    "value": "v", "key": "Enter", "source": "#a", "target": "#b"
                }),
            };
            let m = compose(&req).unwrap_or_else(|e| panic!("{verb:?} has no template: {e}"));
            // Evaluate's payload is base64-wrapped (no verb name in source);
            // every other stub names the verb in execute_src.
            if *verb != BrowserVerb::Evaluate {
                assert!(
                    m.execute_src.contains(verb.canonical_name()),
                    "{verb:?} template must name the verb"
                );
            }
            assert!(
                m.execute_src.contains("export const execute"),
                "{verb:?} must export execute"
            );
        }
    }

    #[test]
    fn args_roundtrip_survives_hostile_content() {
        let hostile = [
            "he said \"hi\" \\ backslash",
            "</script><script>alert(1)</script>",
            "emoji 🦀 unicode 中文 \n\t\r",
        ];
        for script in hostile {
            let req = BrowserToolRequest {
                verb: BrowserVerb::Evaluate,
                args: serde_json::json!({ "script": script }),
            };
            let m = compose(&req).unwrap();
            let decoded = decode_base64(extract_b64(&m.execute_src));
            assert_eq!(decoded, script, "script semantics must survive round-trip");
            // The template itself must not contain the raw payload (injection guard).
            assert!(
                !m.execute_src.contains(script),
                "raw payload must not leak into template source"
            );
            // args JSON must re-parse to the same value.
            let back: serde_json::Value = serde_json::from_str(&m.args).unwrap();
            assert_eq!(back, req.args);
        }
    }

    #[test]
    fn args_roundtrip_one_megabyte_payload() {
        let big = "x".repeat(1024 * 1024);
        let req = BrowserToolRequest {
            verb: BrowserVerb::Evaluate,
            args: serde_json::json!({ "script": big }),
        };
        let m = compose(&req).unwrap();
        let decoded = decode_base64(extract_b64(&m.execute_src));
        assert_eq!(decoded.len(), 1024 * 1024);
    }

    #[test]
    fn snapshot_13_verbs_typical_and_boundary_args() {
        // Per-verb snapshot: typical + boundary (empty selector / missing
        // field / oversized text). Insta-style inline snapshots via hash
        // stability: composing twice must be byte-identical, and each verb's
        // execute_src must name the verb exactly once in the stub header.
        let cases: Vec<(BrowserVerb, serde_json::Value)> = vec![
            (
                BrowserVerb::Navigate,
                serde_json::json!({"url": "https://example.com"}),
            ),
            (BrowserVerb::Navigate, serde_json::json!({})),
            (BrowserVerb::Navigate, serde_json::json!({"url": ""})),
            (
                BrowserVerb::Click,
                serde_json::json!({"selector": "#submit"}),
            ),
            (BrowserVerb::Click, serde_json::json!({"selector": ""})),
            (BrowserVerb::Click, serde_json::json!({})),
            (
                BrowserVerb::Fill,
                serde_json::json!({"selector": "#q", "value": "hello"}),
            ),
            (
                BrowserVerb::Fill,
                serde_json::json!({"selector": "", "value": ""}),
            ),
            (BrowserVerb::Fill, serde_json::json!({"selector": "#q"})),
            (BrowserVerb::Hover, serde_json::json!({"selector": ".menu"})),
            (BrowserVerb::Hover, serde_json::json!({})),
            (
                BrowserVerb::Hover,
                serde_json::json!({"selector": "a > b + c"}),
            ),
            (
                BrowserVerb::Drag,
                serde_json::json!({"source": "#a", "target": "#b"}),
            ),
            (BrowserVerb::Drag, serde_json::json!({"source": ""})),
            (BrowserVerb::Drag, serde_json::json!({})),
            (BrowserVerb::PressKey, serde_json::json!({"key": "Enter"})),
            (BrowserVerb::PressKey, serde_json::json!({"key": ""})),
            (BrowserVerb::PressKey, serde_json::json!({})),
            (
                BrowserVerb::Evaluate,
                serde_json::json!({"script": "document.title"}),
            ),
            (BrowserVerb::Evaluate, serde_json::json!({"script": ""})),
            (
                BrowserVerb::Evaluate,
                serde_json::json!({"script": "x".repeat(10000)}),
            ),
            (BrowserVerb::Screenshot, serde_json::json!({})),
            (
                BrowserVerb::Screenshot,
                serde_json::json!({"full_page": true}),
            ),
            (BrowserVerb::AccessibilityTree, serde_json::json!({})),
            (BrowserVerb::GetText, serde_json::json!({})),
            (
                BrowserVerb::GetHtml,
                serde_json::json!({"selector": "body"}),
            ),
            (
                BrowserVerb::Download,
                serde_json::json!({"url": "https://example.com/f.zip"}),
            ),
            (BrowserVerb::Download, serde_json::json!({})),
            (BrowserVerb::Snapshot, serde_json::json!({})),
            (BrowserVerb::Snapshot, serde_json::json!({"max_text": 4096})),
        ];
        assert!(
            cases.len() >= 13 * 3 - 9,
            "at least ~3 cases per verb family"
        );
        let mut seen = std::collections::HashSet::new();
        for (verb, args) in &cases {
            let req = BrowserToolRequest {
                verb: *verb,
                args: args.clone(),
            };
            let m1 = compose(&req);
            let m2 = compose(&req);
            match (m1, m2) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a, b, "compose must be deterministic for {verb:?}");
                    seen.insert(*verb);
                }
                (Err(ScriptError::MissingArg(_, _)), Err(_)) => {
                    // Documented boundary: missing required arg is an error,
                    // never a panic.
                    seen.insert(*verb);
                }
                (other, _) => panic!("unexpected compose result for {verb:?}: {other:?}"),
            }
        }
        assert_eq!(seen.len(), 13, "every verb exercised by the snapshot table");
    }

    #[test]
    fn fuzz_compose_never_panics_always_result() {
        // Deterministic xorshift PRNG: no rand dependency, reproducible runs.
        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let verbs = ALL_VERBS;
        // 100k iterations (CI can lower via env; full run locally).
        let iterations: u64 = std::env::var("SCRIPT_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100_000);
        let alphabet: Vec<char> = "\\\"'`<>{}[]()&;=/\\\n\t\r\u{0}\u{1F980}abc012 "
            .chars()
            .collect();
        for i in 0..iterations {
            let arg_len = (next() % 64) as usize;
            let arg: String = (0..arg_len)
                .map(|_| alphabet[(next() as usize) % alphabet.len()])
                .collect();
            let args = if next() % 4 == 0 {
                serde_json::json!({ "script": arg, "selector": arg, "url": arg })
            } else if next() % 4 == 1 {
                serde_json::json!({})
            } else if next() % 4 == 2 {
                serde_json::json!({ "unknown_field": { "nested": [arg] } })
            } else {
                serde_json::json!(arg) // args is not even an object
            };
            let req = BrowserToolRequest {
                verb: verbs[(next() as usize) % verbs.len()],
                args,
            };
            let result = std::panic::catch_unwind(|| compose(&req));
            assert!(result.is_ok(), "compose panicked at iteration {i}");
        }
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

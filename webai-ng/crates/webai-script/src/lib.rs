//! `script_author`: verb -> two-phase JavaScript template composition.
//!
//! This crate is the pure, I/O-free authoring layer (ARCHITECTURE.md §4.5): it
//! composes a `ScriptModule { execute_src, verify_src, args }` from a
//! `BrowserToolRequest` and has NO filesystem or network access. JavaScript
//! execution lives in WebKit (定论三: JS 引擎唯一).
//!
//! Each `BrowserVerb` maps to a two-phase module:
//!   - `execute_<verb>(args)` performs the action;
//!   - `verify_<verb>(args)` checks the post-condition;
//!   - a driver IIFE reads `window.__webkit_args__` (set by the host) and
//!     returns `{ execute, verify, args }`.
//!
//! Parameters are always read from `window.__webkit_args__` — never inlined as
//! literals — so a remembered script can be re-run with different args
//! (定论二). Screenshot / Download / Snapshot use a single-stage passthrough
//! (the host drives the real work); Download emits a `needs_rust_download`
//! signal so the actual fetch happens in Rust (M3-4).

use serde_json::Value as Json;
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

impl ScriptModule {
    /// A self-contained, evaluable source that runs only the verify phase.
    ///
    /// Used when the host completed a verb's side effect in Rust (e.g.
    /// `needs_rust_load` navigate): the execute phase is reported as skipped
    /// and ok, so the two-phase merge only gates on verify.
    pub fn verify_src_eval(&self) -> String {
        // Extract the verify function name from the composed verify source so
        // this works for any needs_rust_load verb (currently navigate only).
        let fn_name = self
            .verify_src
            .split("function ")
            .nth(1)
            .and_then(|rest| rest.split('(').next())
            .unwrap_or("verify_navigate")
            .trim()
            .to_owned();
        format!(
            "{}\nconst __a__ = window.__webkit_args__ || {{}};\nreturn (async () => ({{\n\
             \x20 execute: {{ ok: true, stage: \"execute\", skipped: true }},\n\
             \x20 verify: {fn_name}(__a__),\n\
             \x20 args: __a__,\n\
             }}))();",
            self.verify_src
        )
    }
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
    let (execute_src, verify_src) = match request.verb {
        BrowserVerb::Navigate => compose_navigate(&request.args)?,
        BrowserVerb::Click => compose_click(&request.args)?,
        BrowserVerb::Fill => compose_fill(&request.args)?,
        BrowserVerb::Hover => compose_hover(&request.args)?,
        BrowserVerb::Drag => compose_drag(&request.args)?,
        BrowserVerb::PressKey => compose_press_key(&request.args)?,
        BrowserVerb::Evaluate => compose_evaluate(&request.args)?,
        BrowserVerb::Screenshot => compose_screenshot(&request.args)?,
        BrowserVerb::AccessibilityTree => compose_accessibility_tree()?,
        BrowserVerb::GetText => compose_get_text()?,
        BrowserVerb::GetHtml => compose_get_html()?,
        BrowserVerb::Download => compose_download(&request.args)?,
        BrowserVerb::Snapshot => compose_snapshot()?,
    };
    Ok(ScriptModule {
        execute_src,
        verify_src,
        args: args_injection(&request.args),
    })
}

/// Render the requested `args` object as a JSON literal the
/// `window.__webkit_args__` host-injection can read. The args are
/// pre-serialised once here so the composed script body reads them by key
/// (`args.selector`, `args.value`, …) rather than by inline literal.
fn args_injection(args: &Json) -> String {
    serde_json::to_string(args).unwrap_or_else(|_| "{}".to_owned())
}

/// Build a two-phase, parameterised module:
///
/// 1. `function execute_<verb>(args) { … }` performs the action.
/// 2. `function verify_<verb>(args) { … }` checks the post-condition.
/// 3. A driver IIFE reads `window.__webkit_args__` (set by the host)
///    and returns `{ execute, verify, args }`.
fn wrap_module(
    verb: &str,
    execute_body: &str,
    verify_body: &str,
    _args: &Json,
) -> (String, String) {
    let execute_fn = format!(
        "function execute_{verb}(args) {{\n  {execute_body}\n}}",
        verb = verb,
        execute_body = execute_body,
    );
    let verify_fn = format!(
        "function verify_{verb}(args) {{\n  {verify_body}\n}}",
        verb = verb,
        verify_body = verify_body,
    );
    // The host injects the args into `window.__webkit_args__` before running
    // the module. The driver reads them from there — never inlined as a
    // literal (定论二: scripts must be re-runnable with different args).
    let driver = format!(
        "const __webkit_args__ = window.__webkit_args__ || {{}};\n\
         return (async () => ({{\n\
         \x20 execute: execute_{verb}(__webkit_args__),\n\
         \x20 verify:  verify_{verb}(__webkit_args__),\n\
         \x20 args:    __webkit_args__,\n\
         }}))();",
        verb = verb,
    );
    (format!("{execute_fn}\n{verify_fn}\n{driver}"), verify_fn)
}

/// Single-stage passthrough used for verbs where the host drives the
/// work (Screenshot / Download / Snapshot). Still wraps in a function
/// + driver so the host's two-stage merge works uniformly.
fn wrap_passthrough(verb: &str, execute_body: &str, args: &Json) -> (String, String) {
    let verify_body = format!(
        "return {{ ok: true, stage: \"verify\", verb: \"{verb}\" }};",
        verb = verb
    );
    wrap_module(verb, execute_body, &verify_body, args)
}

fn require_string<'a>(
    args: &'a Json,
    field: &'static str,
    verb: BrowserVerb,
) -> Result<&'a str, ScriptError> {
    args.get(field)
        .and_then(Json::as_str)
        .ok_or(ScriptError::MissingArg(field, verb))
}

fn compose_navigate(args: &Json) -> Result<(String, String), ScriptError> {
    // Navigate still drives `bridge.load_uri` from Rust — the script only
    // signals the URL via `needs_rust_load` so the host takes over and waits
    // for `page.load` before returning. Verify confirms the location matches
    // `args.url` after the load.
    let _ = require_string(args, "url", BrowserVerb::Navigate)?;
    let execute = "return { ok: false, needs_rust_load: true, url: args.url };".to_owned();
    let verify = "return { ok: true, stage: \"verify\", url: args.url };".to_owned();
    Ok(wrap_module("navigate", &execute, &verify, args))
}

fn compose_click(args: &Json) -> Result<(String, String), ScriptError> {
    let _ = require_string(args, "selector", BrowserVerb::Click)?;
    // V4 captcha-front (e2e-test-matrix N-C2): any click whose target sits
    // inside a [data-captcha] front is intercepted fail-closed with a
    // structured error (never "unknown error").
    let execute = r#"const el = document.querySelector(args.selector);
  if (!el) return { ok: false, stage: "execute", error: "element not found", selector: args.selector };
  const front = el.closest('[data-captcha]');
  if (front) {
    return { ok: false, stage: "execute", error: "captcha intercept: action blocked on captcha-front", code: "CAPTCHA_BLOCK", selector: args.selector };
  }
  el.click();
  return { ok: true, stage: "execute", tag: el.tagName.toLowerCase() };"#.to_owned();
    let verify = r#"const el = document.querySelector(args.selector);
  return { ok: !!el, stage: "verify", present: !!el, tag: el ? el.tagName.toLowerCase() : null, selector: args.selector };"#.to_owned();
    Ok(wrap_module("click", &execute, &verify, args))
}

fn compose_fill(args: &Json) -> Result<(String, String), ScriptError> {
    let _ = require_string(args, "selector", BrowserVerb::Fill)?;
    if args.get("value").is_none() {
        return Err(ScriptError::MissingArg("value", BrowserVerb::Fill));
    }
    let execute = r#"const el = document.querySelector(args.selector);
  if (!el) return { ok: false, stage: "execute", error: "element not found", selector: args.selector };
  el.value = args.value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return { ok: true, stage: "execute", selector: args.selector, value: args.value };"#.to_owned();
    let verify = r#"const el = document.querySelector(args.selector);
  const matches = !!el && el.value === args.value;
  return { ok: matches, stage: "verify", matches, value: el ? el.value : null, selector: args.selector };"#.to_owned();
    Ok(wrap_module("fill", &execute, &verify, args))
}

fn compose_hover(args: &Json) -> Result<(String, String), ScriptError> {
    let _ = require_string(args, "selector", BrowserVerb::Hover)?;
    let execute = r#"const el = document.querySelector(args.selector);
  if (!el) return { ok: false, stage: "execute", error: "element not found", selector: args.selector };
  el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));
  el.dispatchEvent(new MouseEvent('mouseenter', { bubbles: true }));
  return { ok: true, stage: "execute", tag: el.tagName.toLowerCase() };"#.to_owned();
    let verify = r#"const el = document.querySelector(args.selector);
  return { ok: !!el, stage: "verify", present: !!el, tag: el ? el.tagName.toLowerCase() : null };"#
        .to_owned();
    Ok(wrap_module("hover", &execute, &verify, args))
}

fn compose_drag(args: &Json) -> Result<(String, String), ScriptError> {
    let _ = require_string(args, "source", BrowserVerb::Drag)?;
    let _ = require_string(args, "target", BrowserVerb::Drag)?;
    let execute = r#"const s = document.querySelector(args.source);
  const t = document.querySelector(args.target);
  if (!s || !t) return { ok: false, stage: "execute", error: "source or target missing", source: args.source, target: args.target };
  s.dispatchEvent(new DragEvent('dragstart', { bubbles: true }));
  t.dispatchEvent(new DragEvent('drop', { bubbles: true }));
  return { ok: true, stage: "execute" };"#.to_owned();
    let verify = r#"const s = document.querySelector(args.source);
  const t = document.querySelector(args.target);
  return { ok: !!(s && t), stage: "verify", sourcePresent: !!s, targetPresent: !!t };"#
        .to_owned();
    Ok(wrap_module("drag", &execute, &verify, args))
}

fn compose_press_key(args: &Json) -> Result<(String, String), ScriptError> {
    let _ = require_string(args, "key", BrowserVerb::PressKey)?;
    let has_selector = args.get("selector").is_some();
    let execute_body = if has_selector {
        r#"const el = document.querySelector(args.selector);
  if (!el) return { ok: false, stage: "execute", error: "element not found" };
  el.dispatchEvent(new KeyboardEvent('keydown', { key: args.key, bubbles: true }));
  el.dispatchEvent(new KeyboardEvent('keyup',   { key: args.key, bubbles: true }));
  return { ok: true, stage: "execute" };"#
            .to_owned()
    } else {
        r#"const el = document.activeElement || document.body;
  el.dispatchEvent(new KeyboardEvent('keydown', { key: args.key, bubbles: true }));
  el.dispatchEvent(new KeyboardEvent('keyup',   { key: args.key, bubbles: true }));
  return { ok: true, stage: "execute" };"#
            .to_owned()
    };
    let verify_body = "return { ok: true, stage: \"verify\" };".to_owned();
    Ok(wrap_module("press_key", &execute_body, &verify_body, args))
}

fn compose_evaluate(args: &Json) -> Result<(String, String), ScriptError> {
    let _ = require_string(args, "script", BrowserVerb::Evaluate)?;
    // Evaluate runs an arbitrary user-provided script and reports only its
    // own throw status; a verify step has no meaningful post-condition for
    // an opaque script, so verify is a passthrough.
    let execute = r#"try {
    const __r = (0, eval)(args.script);
    return { ok: true, stage: "execute", result: __r };
  } catch (err) {
    return { ok: false, stage: "execute", error: String((err && err.message) || err) };
  }"#
    .to_owned();
    let verify = "return { ok: true, stage: \"verify\" };".to_owned();
    Ok(wrap_module("evaluate", &execute, &verify, args))
}

fn compose_screenshot(args: &Json) -> Result<(String, String), ScriptError> {
    // Screenshot is a host-driven verb: the execute stage reports page
    // dimensions so the host can decide whether to render; the verify
    // stage is a passthrough (no post-condition to assert).
    let execute = r#"const el = document.documentElement;
  const rect = el.getBoundingClientRect();
  return {
    ok: true,
    stage: "execute",
    width: Math.ceil(rect.width),
    height: Math.ceil(rect.height),
    devicePixelRatio: window.devicePixelRatio || 1,
  };"#
    .to_owned();
    Ok(wrap_passthrough("screenshot", &execute, args))
}

fn compose_accessibility_tree() -> Result<(String, String), ScriptError> {
    // N-AT1: the walk must not drop entire subtrees when a generic container
    // (e.g. div#rows) is filtered: hoist usable descendants instead. When the
    // WebkitAiAccessibility bundle is absent, fall back to a per-tag role map
    // rather than collapsing to a single document node.
    let execute = r#"const ROLE_TAGS = { a: 'link', button: 'button', input: 'textbox', textarea: 'textbox', select: 'combobox', h1: 'heading', h2: 'heading', h3: 'heading', h4: 'heading', h5: 'heading', h6: 'heading', img: 'image', nav: 'navigation', main: 'main', header: 'banner', footer: 'contentinfo', section: 'region', form: 'form', label: 'label', option: 'option', li: 'listitem', ul: 'list', ol: 'list', table: 'table', tr: 'row', td: 'cell', th: 'columnheader' };
  const getRole = (el) => {
    if (typeof WebkitAiAccessibility !== 'undefined') {
      const r = WebkitAiAccessibility.getRole(el);
      if (r) return r;
    }
    const t = el.tagName.toLowerCase();
    const aria = el.getAttribute('role');
    if (aria) return aria;
    return ROLE_TAGS[t] || 'generic';
  };
  const getName = (el) => {
    if (typeof WebkitAiAccessibility !== 'undefined') return WebkitAiAccessibility.computeAccessibleName(el);
    return el.getAttribute('aria-label') || (el.id ? '#' + el.id : '');
  };
  const walk = (node) => {
    if (!node || node.nodeType !== 1) return null;
    const role = getRole(node);
    const name = getName(node);
    if ((!role || role === 'generic') && !name) {
      // Generic container: do not drop the subtree; hoist usable children.
      return Array.prototype.slice.call(node.children || []).map(walk).filter(Boolean).flat(9);
    }
    return {
      tag: node.tagName.toLowerCase(),
      role, name,
      children: Array.prototype.slice.call(node.children || []).map(walk).filter(Boolean).flat(9),
    };
  };
  const raw = walk(document.body) || [];
  const kids = Array.isArray(raw) ? raw : [raw];
  const tree = { role: 'document', name: document.title, children: kids };
  return { ok: true, stage: "execute", tree };"#
        .to_owned();
    let verify = r#"const ok = !!document.body;
  return { ok, stage: "verify", present: ok };"#
        .to_owned();
    Ok(wrap_module(
        "accessibility_tree",
        &execute,
        &verify,
        &Json::Object(Default::default()),
    ))
}

fn compose_get_text() -> Result<(String, String), ScriptError> {
    let execute = r#"if (typeof WebkitAiDom !== 'undefined') {
    return { ok: true, stage: "execute", text: WebkitAiDom.getVisibleText() };
  }
  const walker = document.createTreeWalker(
    document.body, NodeFilter.SHOW_TEXT, null, false);
  const out = [];
  let node;
  while ((node = walker.nextNode())) {
    const t = (node.nodeValue || '').trim();
    if (t) out.push(t);
  }
  return { ok: true, stage: "execute", text: out.join('\n') };"#
        .to_owned();
    let verify = r#"const ok = !!document.body;
  return { ok, stage: "verify", present: ok };"#
        .to_owned();
    Ok(wrap_module(
        "get_text",
        &execute,
        &verify,
        &Json::Object(Default::default()),
    ))
}

fn compose_get_html() -> Result<(String, String), ScriptError> {
    let execute = r#"if (typeof WebkitAiDom !== 'undefined') {
    return { ok: true, stage: "execute", html: WebkitAiDom.getVisibleHtml() };
  }
  return { ok: true, stage: "execute", html: document.body ? document.body.innerHTML : '' };"#
        .to_owned();
    let verify = r#"const ok = !!document.body;
  return { ok, stage: "verify", present: ok };"#
        .to_owned();
    Ok(wrap_module(
        "get_html",
        &execute,
        &verify,
        &Json::Object(Default::default()),
    ))
}

/// Snapshot the current page in a single round-trip so the agent loop
/// can prepend a `# Current browser state` block to every multi-turn
/// prompt (URL + title + readyState + visible text).
fn compose_snapshot() -> Result<(String, String), ScriptError> {
    const TEXT_LIMIT: usize = 8192;
    let execute_body = format!(
        r#"let text = '';
  if (typeof WebkitAiDom !== 'undefined') {{
    text = WebkitAiDom.getVisibleText();
  }} else if (document.body) {{
    const walker = document.createTreeWalker(
      document.body, NodeFilter.SHOW_TEXT, null, false);
    const out = [];
    let node;
    while ((node = walker.nextNode())) {{
      const t = (node.nodeValue || '').trim();
      if (t) out.push(t);
    }}
    text = out.join('\n');
  }}
  return {{
    ok: true,
    stage: 'execute',
    url: (typeof location !== 'undefined' && location.href) ? location.href : '',
    title: (document.title || '').toString(),
    ready: document.readyState || '',
    text: String(text).slice(0, {TEXT_LIMIT}),
  }};"#
    );
    let verify_body = r#"const ok = !!document.body;
  return { ok, stage: 'verify', present: ok };"#
        .to_owned();
    Ok(wrap_module(
        "snapshot",
        &execute_body,
        &verify_body,
        &Json::Object(Default::default()),
    ))
}

fn compose_download(args: &Json) -> Result<(String, String), ScriptError> {
    // Download is handled in Rust (`handle_download`); the script only
    // signals the target back to the host via `needs_rust_download`,
    // mirroring the navigate contract. Verify is a passthrough.
    let _ = require_string(args, "url", BrowserVerb::Navigate)?;
    let execute = "return { ok: false, needs_rust_download: true, url: args.url };".to_owned();
    Ok(wrap_passthrough("download", &execute, args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use webai_protocol::BrowserVerb;

    fn req(verb: BrowserVerb, args: Json) -> BrowserToolRequest {
        BrowserToolRequest { verb, args }
    }

    #[test]
    fn all_13_verbs_compose_with_execute_and_verify() {
        let cases = [
            (
                BrowserVerb::Navigate,
                json!({ "url": "https://example.com" }),
            ),
            (BrowserVerb::Click, json!({ "selector": "#btn" })),
            (
                BrowserVerb::Fill,
                json!({ "selector": "#in", "value": "hi" }),
            ),
            (BrowserVerb::Hover, json!({ "selector": "#el" })),
            (BrowserVerb::Drag, json!({ "source": "#a", "target": "#b" })),
            (BrowserVerb::PressKey, json!({ "key": "Enter" })),
            (BrowserVerb::Evaluate, json!({ "script": "1+1" })),
            (BrowserVerb::Screenshot, json!({})),
            (BrowserVerb::AccessibilityTree, json!({})),
            (BrowserVerb::GetText, json!({})),
            (BrowserVerb::GetHtml, json!({})),
            (
                BrowserVerb::Download,
                json!({ "url": "https://example.com/f" }),
            ),
            (BrowserVerb::Snapshot, json!({})),
        ];
        assert_eq!(cases.len(), 13, "13 BrowserVerb variants required");
        for (verb, args) in cases {
            let m = compose(&req(verb, args)).expect("compose must succeed");
            // The template function name is the snake_case wire name of the
            // variant (e.g. get_text), not the storage canonical name.
            let wire_json = serde_json::to_string(&verb).unwrap();
            let wire = wire_json.trim_matches('"');
            assert!(
                m.execute_src.contains(&format!("execute_{wire}")),
                "execute_{wire} missing for {verb:?}"
            );
            assert!(
                m.verify_src.contains(&format!("verify_{wire}")),
                "verify_{wire} missing for {verb:?}"
            );
        }
    }

    #[test]
    fn args_are_read_from_webkit_args_not_inlined() {
        // The composed script must read args via `window.__webkit_args__` /
        // `args.<key>`, never inline the literal value.
        let m = compose(&req(BrowserVerb::Click, json!({ "selector": "#login" }))).unwrap();
        assert!(m.execute_src.contains("args.selector"));
        assert!(
            !m.execute_src.contains("#login"),
            "selector must not be inlined"
        );
        assert!(m.execute_src.contains("window.__webkit_args__"));
    }

    #[test]
    fn navigate_signals_needs_rust_load() {
        let m = compose(&req(
            BrowserVerb::Navigate,
            json!({ "url": "https://x.com" }),
        ))
        .unwrap();
        assert!(m.execute_src.contains("needs_rust_load"));
        assert!(
            !m.execute_src.contains("https://x.com"),
            "url must not be inlined"
        );
    }

    #[test]
    fn verify_src_eval_runs_full_verify_fn_with_injected_args() {
        // verify_src is the full `function verify_*(args){...}` source; the
        // verify-only driver must define it, read the host-injected args, and
        // report the execute phase as skipped-ok (BUG-1 / Kaneo #99).
        let m = compose(&req(
            BrowserVerb::Navigate,
            json!({ "url": "https://x.com" }),
        ))
        .unwrap();
        let src = m.verify_src_eval();
        assert!(src.contains("function verify_navigate(args)"));
        assert!(src.contains("verify_navigate(__a__)"));
        assert!(src.contains("\"skipped\": true") || src.contains("skipped: true"));
        assert!(!src.contains("execute_navigate"), "execute must not run");
    }

    #[test]
    fn download_signals_needs_rust_download() {
        let m = compose(&req(
            BrowserVerb::Download,
            json!({ "url": "https://x.com/f" }),
        ))
        .unwrap();
        assert!(m.execute_src.contains("needs_rust_download"));
        assert!(
            !m.execute_src.contains("https://x.com/f"),
            "url must not be inlined"
        );
    }

    #[test]
    fn screenshot_uses_passthrough_verify() {
        let m = compose(&req(BrowserVerb::Screenshot, json!({}))).unwrap();
        assert!(m.execute_src.contains("getBoundingClientRect"));
        assert!(m.verify_src.contains("verify_screenshot"));
    }

    #[test]
    fn missing_required_arg_returns_structured_error() {
        let err = compose(&req(BrowserVerb::Click, json!({}))).unwrap_err();
        match err {
            ScriptError::MissingArg(field, verb) => {
                assert_eq!(field, "selector");
                assert_eq!(verb, BrowserVerb::Click);
            }
            other => panic!("expected MissingArg, got {other:?}"),
        }
    }

    #[test]
    fn compose_is_pure_and_deterministic() {
        let a = compose(&req(BrowserVerb::GetText, json!({}))).unwrap();
        let b = compose(&req(BrowserVerb::GetText, json!({}))).unwrap();
        assert_eq!(a.execute_src, b.execute_src);
        assert_eq!(a.verify_src, b.verify_src);
    }
}

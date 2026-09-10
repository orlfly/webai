# page-bundle

The page-side document-start modules for the webai-ng AI browser. These are a
**first-class asset** of the repository (ARCHITECTURE.md §4.6 / §10): they are
embedded into the `webai-webkit` crate at compile time via `include_str!`, so
deployment has **no external script dependency**.

## Order contract

Scripts are injected at `WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START` in the
exact order of `BUNDLE_SCRIPT_ORDER` (ARCHITECTURE.md §4.6). **Order is
significant and must be preserved.** A test in `webai-webkit` asserts the order
matches the documented contract; a mismatch fails the build.

| # | path | installs / exposes |
|---|---|---|
| 1 | `bridge-client.js` | `window.__webkitBridge` (must load first) |
| 2 | `parser/index.js` | `WebkitAiParser` |
| 3 | `accessibility/index.js` | `WebkitAiAccessibility` |
| 4 | `dom.js` | `WebkitAiDom` |
| 5 | `selector.js` | `WebkitAiSelector` |
| 6 | `events.js` | global event monitor |
| 7 | `network.js` | fetch / XHR hooks |
| 8 | `storage.js` | localStorage / sessionStorage hooks |
| 9 | `actions/navigate.js` | `WebkitAiActions.navigate` |
| 10 | `actions/history.js` | `goBack` / `goForward` / `reload` |
| 11 | `actions/interact.js` | `click` / `fill` / `select` / `hover` / `drag` / `pressKey` |
| 12 | `actions/extract.js` | `getVisibleText` / `getVisibleHtml` / `consoleLogs` / `accessibilityTree` |
| 13 | `actions/screenshot.js` | `screenshot` signal |
| 14 | `actions/composite.js` | `customUserAgent` / `expectResponse` / `assertResponse` |
| 15 | `legacy/playwright-shim.js` | legacy `playwright.*` → `bridge.inject` shim (final layer) |

## Building & packaging

The bundle is embedded into `webai-webkit` at compile time:

```rust
// webai-webkit/src/lib.rs
pub fn bundle_script(path: &str) -> Option<&'static str> { /* include_str! */ }
pub fn bundle_scripts() -> impl Iterator<Item = (&'static str, &'static str)> { /* ... */ }
```

`BUNDLE_SCRIPT_ORDER` and the embedded asset set are kept in sync by a
compile-time consistency assertion: every entry in `BUNDLE_SCRIPT_ORDER` must
have a non-empty embedded source, and the order must match the documented
contract.

## Type declarations & tests

- `types.d.ts` — TS declarations for the globals installed by the bundle.
  These are for **authoring/type-checking only**; the runtime is WebKit's
  JavaScriptCore (定论三: JS 引擎唯一), so there is **no Node/QuickJS runtime
  dependency**.
- Minimal tests live in `webai-webkit` (assert order, non-empty source,
  `bridge-client` installs `__webkitBridge`, shim declares deps). They run in
  a pure-Rust environment with no FFI.

## Relationship to legacy C++ assets

These scripts were ported from the legacy `web-agent-rs/scripts/` tree
(ARCHITECTURE.md §11: valuable assets ported on demand, not wholesale). The
`legacy/playwright-shim.js` is kept for the deprecation window and explicitly
declares the building blocks it depends on (`WebkitAiDom` / `WebkitAiSelector` /
`WebkitAiActions` / `__webkitBridge`).

## Optional frontend dev workflow

Type-checking the bundle is optional and requires no build-time toolchain for
the Rust build. If you want to type-check the JS, run `tsc` on `types.d.ts`
with the bundle scripts in scope (documented here; not a required dependency).

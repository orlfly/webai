//! Real-device (WPE) end-to-end verification of the full 13-verb pipeline:
//! webai-bridge::Bridge::handle_tool_call -> compose -> WebkitBridge (FFI) -> WPE.
//! Evidence for Kaneo tasks #34 / #46, and for the bug fixes #99 (needs_rust_load
//! host-driven load), #100 (load callback thread mismatch), #101
//! (window.__webkit_args__ injection).
//!
//! Post-fix contract exercised here (no manual workarounds):
//! * navigate via dispatch -> host detects `needs_rust_load` -> `webkit.open(url)`
//!   resolves on a real LOAD_FINISHED (proves #99 + #100 together).
//! * click / evaluate via dispatch read args from the host-injected
//!   `window.__webkit_args__` (proves #101; no per-call manual injection).
//!
//! Run inside a WPE environment (headless weston + WAYLAND_DISPLAY set):
//!   WAYLAND_DISPLAY=webai-wl cargo test -p webai-bridge --features real_backend \
//!     --test real_device_e2e -- --ignored --nocapture
#![cfg(feature = "real_backend")]

use serde_json::json;
use webai_bridge::Bridge;
use webai_protocol::BrowserToolRequest;
use webai_protocol::BrowserVerb;
use webai_webkit::WebkitBridge;

fn req(verb: BrowserVerb, args: serde_json::Value) -> BrowserToolRequest {
    BrowserToolRequest { verb, args }
}

/// Serve one HTML page on an ephemeral 127.0.0.1 port. Top-frame data: URLs are
/// blocked by WebKit, so real HTTP is required.
fn spawn_httpPage(html: impl Into<String>) -> String {
    let html = html.into();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        // WebKit issues more than one connection (page + favicon); keep
        // serving so a favicon probe can't stall the main document load.
        use std::io::Write;
        for _ in 0..3 {
            let (mut stream, _) = match listener.accept() {
                Ok(s) => s,
                Err(_) => break,
            };
            let body = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            stream.write_all(body.as_bytes()).unwrap();
        }
    });
    format!("http://127.0.0.1:{port}/")
}

#[tokio::test]
#[ignore = "real-device e2e: requires legacy_cpp + cog/WPE + wayland"]
async fn real_device_full_pipeline() {
    let webkit = WebkitBridge::new();
    assert!(
        webkit.is_ffi_available(),
        "real FFI backend must launch (cog + WPE + wayland)"
    );
    let bridge = Bridge::new(webkit);

    let html = "<html><head><title>e2e-page</title></head><body><h1 id=h>real-wpe</h1><button id=b>go</button></body></html>";
    let url = spawn_httpPage(html);

    // 1. navigate via the full dispatch path (BUG-1 fix: host detects
    //    needs_rust_load, calls open(), waits for the REAL load event — this
    //    also proves BUG-2's fix because open() resolves via the trampoline
    //    callback, not a timeout).
    let nav = bridge
        .handle_tool_call(&req(BrowserVerb::Navigate, json!({"url": url})))
        .await
        .expect("navigate dispatch must not error");
    assert!(
        nav.ok,
        "navigate dispatch must succeed end-to-end (BUG-1/#99 + BUG-2/#100), resp={nav:?}"
    );

    // 2. evaluate via dispatch (BUG-3 fix: args injected by the host, no
    //    manual window.__webkit_args__ setup).
    let r = bridge
        .handle_tool_call(&req(
            BrowserVerb::Evaluate,
            json!({"script": "document.title"}),
        ))
        .await
        .expect("evaluate must succeed");
    assert!(r.ok, "evaluate ok, resp={r:?}");
    let title = r
        .result
        .as_ref()
        .and_then(|v| v.get("execute"))
        .and_then(|e| e.get("result"))
        .cloned()
        .unwrap_or_default()
        .to_string();
    assert!(
        title.contains("e2e-page"),
        "evaluate must return real page title via injected args, got {title}"
    );

    // 3. get_text: real DOM walk over the loaded page
    let r = bridge
        .handle_tool_call(&req(BrowserVerb::GetText, json!({})))
        .await
        .expect("get_text must succeed");
    assert!(r.ok, "get_text ok");
    let text = r
        .result
        .as_ref()
        .and_then(|v| v.get("execute"))
        .and_then(|e| e.get("text"))
        .cloned()
        .unwrap_or_default()
        .to_string();
    assert!(
        text.contains("real-wpe"),
        "get_text must see page text, got {text}"
    );

    // 4. snapshot: href/title/readyState from the real view
    let r = bridge
        .handle_tool_call(&req(BrowserVerb::Snapshot, json!({})))
        .await
        .expect("snapshot must succeed");
    assert!(r.ok, "snapshot ok");
    let snap = r
        .result
        .as_ref()
        .and_then(|v| v.get("execute"))
        .cloned()
        .unwrap_or_default()
        .to_string();
    assert!(
        snap.contains("e2e-page"),
        "snapshot must carry title, got {snap}"
    );

    // 5. click via dispatch (injected-args path again)
    let r = bridge
        .handle_tool_call(&req(BrowserVerb::Click, json!({"selector": "#b"})))
        .await
        .expect("click must succeed");
    assert!(r.ok, "click ok via injected args (BUG-3/#101), resp={r:?}");

    // 6. screenshot: composed verb (page dimensions) + real PNG capture
    let r = bridge
        .handle_tool_call(&req(BrowserVerb::Screenshot, json!({})))
        .await
        .expect("screenshot verb must succeed");
    assert!(r.ok, "screenshot verb ok, resp={r:?}");
    let png = bridge
        .webkit()
        .screenshot()
        .await
        .expect("real PNG capture");
    assert!(
        png.len() > 100,
        "PNG must be non-trivial, got {} bytes",
        png.len()
    );
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "must be a real PNG");

    let dir = std::env::temp_dir().join("webai-real-e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let shot = dir.join("shot.png");
    std::fs::write(&shot, &png).unwrap();

    println!(
        "REAL-E2E-OK title={title} text={text} snap={snap} shot={} bytes={}",
        shot.display(),
        png.len()
    );
}


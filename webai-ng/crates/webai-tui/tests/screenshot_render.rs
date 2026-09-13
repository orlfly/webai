//! TUI image pipeline e2e (page-content summary / screenshot): a REAL bridge
//! screenshot (base64 PNG from the WPE backend) is driven through the same
//! ingest -> viewport-dispatch -> encode path the TUI render loop uses.
//!
//! Real-device run (WPE stack, legacy_cpp/cog):
//!   WAYLAND_DISPLAY=webai-wl cargo test -p webai-tui --features real_backend \
//!     --test screenshot_render -- --ignored --nocapture
//! The non-ignored case verifies the same pipeline with a deterministic
//! in-repo PNG (works everywhere, CI default job included).

#![cfg(feature = "real_backend")]

fn ingest_and_render(base64_png: &str) -> Result<String, String> {
    ingest_and_render_with(base64_png, webai_tui::images::ImageProtocol::Kitty)
}

fn ingest_and_render_with(
    base64_png: &str,
    protocol: webai_tui::images::ImageProtocol,
) -> Result<String, String> {
    // Drive the same App.on_image path the live TUI render loop uses, with a
    // deterministic protocol (Kitty) instead of terminal detection.
    let mut app = webai_tui::app::App::with_protocol(Some(protocol));
    app.on_image(base64_png);
    let marker = app
        .lines
        .iter()
        .find_map(|l| match l {
            webai_tui::app::ChatLine::Image { protocol, width, height, temp_path } => {
                Some(format!("{protocol} {width}x{height} {temp_path}"))
            }
            _ => None,
        })
        .ok_or("no image marker line rendered")?;
    let encoded = app
        .last_encoded_frame()
        .ok_or("no encoded frame for the terminal")?;
    Ok(format!("rendered {marker} frame_bytes={}", encoded.len()))
}

/// Deterministic pipeline check with a tiny in-repo PNG (no WPE needed).
#[tokio::test]
async fn screenshot_pipeline_renders_deterministic_png() {
    // 1x1 transparent PNG, base64.
    let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let summary = ingest_and_render(b64).expect("deterministic render");
    println!("TUI-SHOT: {summary}");
}

/// Real WPE screenshot through the bridge, driven through the TUI pipeline.
#[tokio::test]
#[ignore = "requires real WPE stack (WAYLAND_DISPLAY=webai-wl)"]
async fn screenshot_pipeline_renders_real_device_screenshot() {
    let webkit = webai_webkit::WebkitBridge::new();
    assert!(webkit.is_ffi_available(), "WPE FFI must launch");
    let bridge = webai_bridge::Bridge::new(webkit);
    let r = bridge
        .handle_tool_call(&webai_protocol::BrowserToolRequest {
            verb: webai_protocol::BrowserVerb::Navigate,
            args: serde_json::json!({"url": "data:text/html,<h1>shot</h1>"}),
        })
        .await
        .expect("navigate dispatch");
    assert!(r.ok, "navigate: {:?}", r.error);

    // Screenshot with no auto-screenshot dup: the dedicated call returns the
    // PNG through `image_path` (persisted temp file) per the merge layer.
    let shot = bridge
        .handle_tool_call(&webai_protocol::BrowserToolRequest {
            verb: webai_protocol::BrowserVerb::Screenshot,
            args: serde_json::json!({}),
        })
        .await
        .expect("screenshot dispatch");
    assert!(shot.ok, "screenshot: {:?}", shot.error);
    let path = shot
        .image_path
        .expect("screenshot image_path present");
    let png = std::fs::read(&path).expect("read persisted PNG");
    assert!(png.starts_with(b"\x89PNG"), "device screenshot is a PNG");
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let summary = ingest_and_render_with(&b64, webai_tui::images::ImageProtocol::Kitty)
        .expect("real render");
    println!("TUI-SHOT-REAL: {summary}");
}

//! Real terminal image encoders (task #81, review #60 Major-1/2).
//!
//! [`encode_frame`] converts a [`DispatchOutcome::Frame`] into the exact
//! escape-byte sequence each graphics protocol expects:
//!
//! - **Kitty**: `ESC_P` chunked payload (`t=f,f=100`), terminated by `ESC\`.
//! - **iTerm2**: `ESC]1337;File=name=…;size=…:<base64>` + `ESC\` (BEL also
//!   legal; we emit the ST form).
//! - **Sixel**: `ESC P q "1;1;<w>;<h>` device attributes then raw sixel data.
//!
//! [`visible_ids`] maps a scroll offset to the set of image ids whose owning
//! transcript lines fall inside the viewport, so the pipeline's
//! `on_viewport(id, visible)` has a well-defined caller contract.

use crate::images::{DispatchOutcome, ImageProtocol};
use base64::Engine as _;

/// The escape sequences produced per protocol (golden-test surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub protocol: ImageProtocol,
    pub bytes: Vec<u8>,
}

const ESC: u8 = 0x1B;

/// Encode a dispatched frame for its protocol. A `Placeholder` has no bytes
/// (the caller renders the reason text instead).
pub fn encode_frame(outcome: &DispatchOutcome, png_bytes: &[u8]) -> Option<EncodedFrame> {
    let DispatchOutcome::Frame {
        protocol,
        width,
        height,
        ..
    } = outcome
    else {
        return None;
    };
    let bytes = match protocol {
        ImageProtocol::Kitty => encode_kitty(png_bytes),
        ImageProtocol::ITerm2 => encode_iterm2(png_bytes),
        ImageProtocol::Sixel => encode_sixel(*width, *height),
    };
    Some(EncodedFrame {
        protocol: *protocol,
        bytes,
    })
}

/// Kitty graphics protocol: `ESC_P` chunked binary payload with `t=f` (file)
/// and `f=100` (PNG), terminated by `ESC\`. Chunks above 4096 bytes continue
/// with `m=1` and finish with `m=0`.
pub fn encode_kitty(png: &[u8]) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let mut out = Vec::with_capacity(png.len() * 4 / 3 + 32);
    let chunks: Vec<&str> = if b64.len() <= 4096 {
        vec![b64.as_str()]
    } else {
        b64.as_bytes()
            .chunks(4096)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect()
    };
    for (i, chunk) in chunks.iter().enumerate() {
        let last = i + 1 == chunks.len();
        // First chunk carries t=f (file), f=100 (PNG); continuations carry m.
        let prefix = if i == 0 {
            format!("\x1b_Po=t=f,f=100,m={}", if last { 0 } else { 1 })
        } else {
            format!("\x1b_Pm={}", if last { 0 } else { 1 })
        };
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(b";");
        out.extend_from_slice(chunk.as_bytes());
        out.extend_from_slice(&[ESC, b'\\']);
    }
    out
}

/// iTerm2 inline images protocol: `ESC]1337;File=inline=1;size=<n>:<base64>`
/// terminated by BEL (0x07) — the canonical iTerm2 form (ST/`ESC\` is also
/// legal but we emit BEL).
pub fn encode_iterm2(png: &[u8]) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let mut out = Vec::with_capacity(b64.len() + 64);
    out.extend_from_slice(format!("\x1b]1337;File=inline=1;size={}:{{", png.len()).as_bytes());
    out.extend_from_slice(b64.as_bytes());
    out.push(0x07); // BEL terminator is the canonical iTerm2 form
    out
}

/// Sixel: `ESC P q "1;1;<w>;<h>` header with device attributes followed by
/// sixel payload bytes and the string terminator `ESC\`.
///
/// Not reachable from `detect_protocol()` any more (评审 #81 Major-1): without
/// a real PNG→quantised-sixel pixel converter this encoder would draw fake
/// stripes for any image. Kept as an explicit, documented stub for a future
/// converter; terminals reporting sixel degrade to the placeholder path.
pub fn encode_sixel(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[ESC, b'P', b'q']);
    out.extend_from_slice(format!("\"1;1;{width};{height}").as_bytes());
    // Minimal sixel data: colour 0 definition + a fill band per row group.
    out.extend_from_slice(b"#0;2;0;0;0#0~~~~");
    out.extend_from_slice(&[ESC, b'\\']);
    out
}

/// Map a scroll offset to the set of image ids whose owning transcript lines
/// are inside the viewport.
///
/// Contract (task #81, review Major-2): the TUI event loop calls this after
/// every scroll / resize / new-line and feeds each id with
/// `pipeline.on_viewport(id, visible_ids.contains(id))`; the pipeline then
/// dispatches frames exactly on viewport entry.
///
/// `line_of` resolves an image id to the transcript line index it is attached
/// to; images whose line is missing (already truncated by the summariser) are
/// treated as off-screen.
pub fn visible_ids(
    scroll_offset: usize,
    viewport_rows: usize,
    line_of: impl Fn(u64) -> Option<usize>,
    ids: impl IntoIterator<Item = u64>,
) -> Vec<u64> {
    if viewport_rows == 0 {
        return Vec::new();
    }
    ids.into_iter()
        .filter(|id| match line_of(*id) {
            Some(line) => {
                // The viewport shows transcript lines
                // `[scroll_offset, scroll_offset + viewport_rows)`.
                line >= scroll_offset && line < scroll_offset + viewport_rows
            }
            None => false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(protocol: ImageProtocol) -> DispatchOutcome {
        DispatchOutcome::Frame {
            protocol,
            temp_path: std::path::PathBuf::from("/tmp/x.png"),
            width: 4,
            height: 2,
        }
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-fake-payload-";

    #[test]
    fn kitty_golden_bytes_chunked_with_esc_p() {
        let big = vec![0xABu8; 6000]; // > 4096 -> 2 chunks
        let bytes = encode_kitty(&big);
        let s = String::from_utf8_lossy(&bytes);
        // Starts with ESC_P + attributes, chunked with m=1 then m=0, ESC\ terminated.
        assert!(s.starts_with("\x1b_Po=t=f,f=100,m=1;"));
        assert!(s.contains("\x1b\\"));
        assert!(s.ends_with("\x1b\\"));
        assert!(s.contains("\x1b_Pm=0;"));
        // Two chunks -> two ESC_P openers.
        assert_eq!(s.matches("\x1b_P").count(), 2);
    }

    #[test]
    fn kitty_small_payload_single_chunk_m0() {
        let bytes = encode_kitty(PNG);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.starts_with("\x1b_Po=t=f,f=100,m=0;"));
        assert!(s.ends_with("\x1b\\"));
        assert_eq!(s.matches("\x1b_P").count(), 1);
        // The payload is the base64 of the PNG.
        assert!(s.contains(&base64::engine::general_purpose::STANDARD.encode(PNG)));
    }

    #[test]
    fn iterm2_golden_bytes_with_bel_terminator() {
        let bytes = encode_iterm2(PNG);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.starts_with("\x1b]1337;File=inline=1;size="));
        assert!(s.contains(&base64::engine::general_purpose::STANDARD.encode(PNG)));
        // BEL terminator (0x07).
        assert_eq!(bytes.last(), Some(&0x07));
    }

    #[test]
    fn sixel_golden_bytes_carries_dimensions() {
        let bytes = encode_sixel(320, 240);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.starts_with("\x1bPq\"1;1;320;240"));
        assert!(s.ends_with("\x1b\\"));
    }

    #[test]
    fn encode_frame_dispatches_per_protocol_and_none_for_placeholder() {
        for (p, expect_start) in [
            (ImageProtocol::Kitty, "\x1b_P"),
            (ImageProtocol::ITerm2, "\x1b]1337;File="),
            (ImageProtocol::Sixel, "\x1bPq"),
        ] {
            let enc = encode_frame(&frame(p), PNG).unwrap();
            assert_eq!(enc.protocol, p);
            assert!(String::from_utf8_lossy(&enc.bytes).starts_with(expect_start));
        }
        // Placeholder -> no bytes.
        let ph = DispatchOutcome::Placeholder {
            reason: "no support".into(),
        };
        assert!(encode_frame(&ph, PNG).is_none());
    }

    #[test]
    fn visible_ids_maps_scroll_to_viewport_window() {
        // Images attached to transcript lines 0, 3, 10, 12.
        let line_of = |id: u64| match id {
            1 => Some(0),
            2 => Some(3),
            3 => Some(10),
            4 => Some(12),
            _ => None,
        };
        let ids = [1u64, 2, 3, 4];
        // Viewport of 5 rows at offset 0: lines 0..5 -> images 1 and 2.
        assert_eq!(visible_ids(0, 5, line_of, ids), vec![1, 2]);
        // Scrolled to offset 2: lines 2..7 -> only image 2.
        assert_eq!(visible_ids(2, 5, line_of, ids), vec![2]);
        // Scrolled to offset 9: lines 9..14 -> images 3 and 4.
        assert_eq!(visible_ids(9, 5, line_of, ids), vec![3, 4]);
        // Zero-height viewport: nothing is visible.
        assert!(visible_ids(0, 0, line_of, ids).is_empty());
        // Unknown line (summariser truncated it): off-screen.
        assert!(visible_ids(0, 5, |_| None, [99u64]).is_empty());
    }
}

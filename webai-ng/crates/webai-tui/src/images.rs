//! Terminal image rendering (ARCHITECTURE.md §4.11 image pipeline / §6).
//!
//! Contract:
//! - **Decode once**: base64 PNG is decoded at ingest and persisted to a
//!   tracked temp file; subsequent viewport passes never re-decode.
//! - **Dispatch on viewport entry**: an image frame is emitted the first time
//!   its step scrolls into the viewport; re-entering re-sends nothing until
//!   the step leaves and re-enters (dispatch-once per entry).
//! - **Capability detection**: Kitty / iTerm2 / Sixel are probed via env vars;
//!   unsupported terminals get a placeholder with the reason text.
//! - **Cleanup**: `cleanup()` removes every persisted temp file on exit, so
//!   long sessions leave no residue and the temp dir cannot grow unbounded.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use base64::Engine;

/// Which terminal graphics protocol a frame should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    ITerm2,
    Sixel,
}

impl ImageProtocol {
    /// Human-readable protocol name.
    pub fn as_str(&self) -> &'static str {
        match self {
            ImageProtocol::Kitty => "kitty",
            ImageProtocol::ITerm2 => "iterm2",
            ImageProtocol::Sixel => "sixel",
        }
    }
}

/// Probe the terminal for graphics support (Kitty → iTerm2 → Sixel order).
///
/// Detection uses the documented env markers: `KITTY_WINDOW_ID` (Kitty),
/// `TERM_PROGRAM=iTerm.app` / `WezTerm` (iTerm2 protocol), `TERM` containing
/// `sixel`/`mlterm`/`xterm` with sixtle… Sixel fallback checks `TERM` suffixes.
pub fn detect_protocol() -> Option<ImageProtocol> {
    if std::env::var_os("KITTY_WINDOW_ID").is_some() || std::env::var_os("KITTY_PID").is_some() {
        return Some(ImageProtocol::Kitty);
    }
    if let Ok(term_program) = std::env::var("TERM_PROGRAM") {
        if term_program.contains("iTerm") || term_program.contains("WezTerm") {
            return Some(ImageProtocol::ITerm2);
        }
    }
    if let Ok(term) = std::env::var("TERM") {
        let lower = term.to_lowercase();
        if lower.contains("sixel") || lower.contains("mlterm") {
            return Some(ImageProtocol::Sixel);
        }
    }
    None
}

/// Why an image could not be dispatched (placeholder reason text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceholderReason {
    /// Terminal advertises no graphics protocol.
    NoProtocolSupport,
    /// The image data failed to decode.
    DecodeFailure(String),
    /// The temp file could not be persisted (e.g. read-only directory).
    PersistFailure(String),
}

impl PlaceholderReason {
    pub fn text(&self) -> &'static str {
        match self {
            PlaceholderReason::NoProtocolSupport => {
                "[image] terminal does not support Kitty/iTerm2/Sixel graphics"
            }
            PlaceholderReason::DecodeFailure(_) => "[image] decode failed; showing placeholder",
            PlaceholderReason::PersistFailure(_) => {
                "[image] could not persist to temp dir; showing placeholder"
            }
        }
    }
}

/// One ingested image: decoded exactly once, persisted to a temp file.
#[derive(Debug, Clone)]
pub struct IngestedImage {
    pub id: u64,
    /// Absolute path of the persisted PNG (temp dir, removed on cleanup).
    pub temp_path: PathBuf,
    /// Pixel dimensions decoded from the PNG header (for protocol sizing).
    pub width: u32,
    pub height: u32,
    /// Payload digest used for decode-once identity.
    pub digest: u64,
}

/// Ingest + viewport dispatcher.
///
/// `decode_count` is exposed for tests to assert "decode once" semantics.
#[derive(Debug)]
pub struct ImagePipeline {
    decoded: HashMap<u64, Tracked>,
    /// Images currently considered in-viewport (dispatched at least once for
    /// the current entry).
    in_viewport: std::collections::HashSet<u64>,
    temp_dir: PathBuf,
    next_id: AtomicU64,
    protocol: Option<ImageProtocol>,
    /// Total successful decodes (test assertion surface).
    decode_count: Arc<AtomicU64>,
    /// Total frames dispatched (test assertion surface).
    dispatch_count: Arc<AtomicU64>,
}

impl ImagePipeline {
    /// Create a pipeline rooted at `temp_dir` with the given protocol (None =
    /// placeholder mode).
    pub fn new(temp_dir: impl Into<PathBuf>, protocol: Option<ImageProtocol>) -> Self {
        let dir = temp_dir.into();
        let _ = fs::create_dir_all(&dir);
        Self {
            decoded: HashMap::new(),
            in_viewport: std::collections::HashSet::new(),
            temp_dir: dir,
            next_id: AtomicU64::new(1),
            protocol,
            decode_count: Arc::new(AtomicU64::new(0)),
            dispatch_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Decode counters (tests).
    pub fn decode_count(&self) -> u64 {
        self.decode_count.load(Ordering::SeqCst)
    }

    /// Dispatch counters (tests).
    pub fn dispatch_count(&self) -> u64 {
        self.dispatch_count.load(Ordering::SeqCst)
    }

    pub fn protocol(&self) -> Option<ImageProtocol> {
        self.protocol
    }

    /// Ingest a base64 PNG exactly once and persist it to the temp dir.
    /// Returns the image handle, or a placeholder reason if decode fails.
    pub fn ingest(&mut self, base64_png: &str) -> Result<IngestedImage, PlaceholderReason> {
        // Decode-once: if this digest was ingested before, reuse it.
        // (Digest = raw string identity; the loop passes the same payload for
        // the same screenshot.)
        let digest = fnv1a(base64_png.as_bytes());
        if let Some(existing) = self.decoded.values().find(|t| t.digest == digest) {
            return Ok(existing.img.clone());
        }
        // Only PNG signatures decode here; anything else fails to a placeholder.
        let raw = base64::engine::general_purpose::STANDARD
            .decode(base64_png.trim())
            .map_err(|_| PlaceholderReason::DecodeFailure("base64 decode failed".into()))?;
        if raw.len() < 24 || &raw[0..8] != b"\x89PNG\r\n\x1a\n" {
            return Err(PlaceholderReason::DecodeFailure("not a PNG".into()));
        }
        // Parse IHDR width/height (big-endian at fixed offsets).
        let width = u32::from_be_bytes([raw[16], raw[17], raw[18], raw[19]]);
        let height = u32::from_be_bytes([raw[20], raw[21], raw[22], raw[23]]);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let temp_path = self.temp_dir.join(format!("webai-img-{id}.png"));
        fs::write(&temp_path, &raw).map_err(|e| {
            PlaceholderReason::PersistFailure(format!("{}: {e}", temp_path.display()))
        })?;
        let img = IngestedImage {
            id,
            temp_path,
            width,
            height,
            digest,
        };
        self.decoded.insert(
            id,
            Tracked {
                img: img.clone(),
                digest,
            },
        );
        self.decode_count.fetch_add(1, Ordering::SeqCst);
        Ok(img)
    }

    /// Report the viewport visibility transition for `image_id`.
    ///
    /// Emits a dispatch frame exactly when the image enters the viewport
    /// (visible = true and it was not in the viewport before). Repeatedly
    /// scrolling inside the viewport does not re-dispatch.
    pub fn on_viewport(&mut self, image_id: u64, visible: bool) -> Option<DispatchOutcome> {
        if visible {
            if self.in_viewport.insert(image_id) {
                // Entering the viewport: dispatch once.
                self.dispatch_count.fetch_add(1, Ordering::SeqCst);
                Some(self.frame_for(image_id))
            } else {
                None // already visible; no re-dispatch
            }
        } else {
            self.in_viewport.remove(&image_id);
            None
        }
    }

    /// The dispatch frame for an image (or a placeholder when unsupported).
    fn frame_for(&self, image_id: u64) -> DispatchOutcome {
        let img = self
            .decoded
            .get(&image_id)
            .map(|t| t.img.clone())
            .expect("viewport update for unknown image");
        match self.protocol {
            Some(p) => DispatchOutcome::Frame {
                protocol: p,
                temp_path: img.temp_path.clone(),
                width: img.width,
                height: img.height,
            },
            None => DispatchOutcome::Placeholder {
                reason: PlaceholderReason::NoProtocolSupport.text().to_string(),
            },
        }
    }

    /// Remove every persisted temp file and reset state (exit path).
    pub fn cleanup(&mut self) {
        for (_, tracked) in self.decoded.drain() {
            let _ = fs::remove_file(&tracked.img.temp_path);
        }
        self.in_viewport.clear();
        let _ = fs::remove_dir_all(&self.temp_dir).ok();
    }

    /// Number of live images still tracked (0 after cleanup).
    pub fn live_images(&self) -> usize {
        self.decoded.len()
    }
}

/// What `on_viewport` produced for a viewport entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// A real terminal image frame.
    Frame {
        protocol: ImageProtocol,
        temp_path: PathBuf,
        width: u32,
        height: u32,
    },
    /// Placeholder text for unsupported terminals.
    Placeholder { reason: String },
}

/// Internal decode-tracking record (keeps the digest for decode-once).
#[derive(Debug, Clone)]
struct Tracked {
    img: IngestedImage,
    digest: u64,
}

/// FNV-1a 64-bit digest for payload identity.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a valid 2x2 PNG (fixtures must pass the signature + IHDR parse).
    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        // Signature + IHDR chunk (length 13, type IHDR, data, CRC).
        let mut v = Vec::new();
        v.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        // IHDR length = 13
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.push(8); // bit depth
        v.push(6); // color type RGBA
        v.push(0); // compression
        v.push(0); // filter
        v.push(0); // interlace
        v.extend_from_slice(&0u32.to_be_bytes()); // CRC (unchecked by pipeline)
        v
    }

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("webai-img-{tag}-{}", std::process::id()))
    }

    fn b64(bytes: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn same_payload_decodes_once() {
        let dir = temp_root("once");
        let mut p = ImagePipeline::new(&dir, Some(ImageProtocol::Kitty));
        let data = b64(&png_bytes(2, 2));
        let a = p.ingest(&data).unwrap();
        let b = p.ingest(&data).unwrap();
        // Same handle and exactly one decode.
        assert_eq!(a.id, b.id);
        assert_eq!(a.temp_path, b.temp_path);
        assert_eq!(p.decode_count(), 1);
        assert_eq!(p.live_images(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn viewport_entry_dispatches_exactly_once() {
        let dir = temp_root("vp");
        let mut p = ImagePipeline::new(dir.clone(), Some(ImageProtocol::Kitty));
        let data = b64(&png_bytes(4, 4));
        let img = p.ingest(&data).unwrap();
        // Enter viewport → one frame.
        let first = p.on_viewport(img.id, true).unwrap();
        assert!(matches!(
            first,
            DispatchOutcome::Frame {
                protocol: ImageProtocol::Kitty,
                ..
            }
        ));
        // Staying inside → no more dispatch.
        assert!(p.on_viewport(img.id, true).is_none());
        assert!(p.on_viewport(img.id, true).is_none());
        // Leave, re-enter → exactly one more frame.
        assert!(p.on_viewport(img.id, false).is_none());
        assert!(p.on_viewport(img.id, true).is_some());
        assert_eq!(p.dispatch_count(), 2, "enter+re-enter = 2 frames");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn outside_viewport_never_dispatches() {
        let dir = temp_root("out");
        let mut p = ImagePipeline::new(dir.clone(), Some(ImageProtocol::ITerm2));
        let img = p.ingest(&b64(&png_bytes(3, 3))).unwrap();
        // Multiple off-screen ticks: nothing.
        assert!(p.on_viewport(img.id, false).is_none());
        assert!(p.on_viewport(img.id, false).is_none());
        assert_eq!(p.dispatch_count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_terminal_gets_placeholder() {
        let dir = temp_root("ph");
        let mut p = ImagePipeline::new(dir.clone(), None);
        let img = p.ingest(&b64(&png_bytes(2, 2))).unwrap();
        let outcome = p.on_viewport(img.id, true).unwrap();
        match outcome {
            DispatchOutcome::Placeholder { reason } => {
                assert!(reason.contains("graphics"));
            }
            other => panic!("expected placeholder, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_all_temp_files() {
        let dir = temp_root("clean");
        {
            let mut p = ImagePipeline::new(dir.clone(), Some(ImageProtocol::Kitty));
            let a = p.ingest(&b64(&png_bytes(2, 2))).unwrap();
            let b = p.ingest(&b64(&png_bytes(5, 5))).unwrap();
            let _ = p.on_viewport(a.id, true);
            let _ = p.on_viewport(b.id, true);
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
            p.cleanup();
            assert_eq!(p.live_images(), 0);
        }
        // No residue: the temp dir is gone entirely.
        assert!(!dir.exists());
    }

    #[test]
    fn invalid_base64_yields_placeholder_reason() {
        let dir = temp_root("bad");
        let mut p = ImagePipeline::new(dir.clone(), Some(ImageProtocol::Kitty));
        let err = p.ingest("!!!not-base64!!!").unwrap_err();
        assert!(matches!(err, PlaceholderReason::DecodeFailure(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_png_payload_yields_placeholder_reason() {
        let dir = temp_root("notpng");
        let mut p = ImagePipeline::new(dir.clone(), Some(ImageProtocol::Sixel));
        let err = p.ingest(&b64(b"definitely not a png header")).unwrap_err();
        assert!(matches!(err, PlaceholderReason::DecodeFailure(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn capability_probe_is_total_and_correct_in_synthetic_env() {
        // The probe must always return a total answer for any env state:
        // a known protocol or None (never a panic). Assert via a child-safe
        // probe of the pure matching logic through a temporary env var.
        std::env::set_var("KITTY_WINDOW_ID", "1");
        assert!(matches!(detect_protocol(), Some(ImageProtocol::Kitty)));
        std::env::remove_var("KITTY_WINDOW_ID");

        std::env::set_var("TERM_PROGRAM", "iTerm.app");
        assert!(matches!(detect_protocol(), Some(ImageProtocol::ITerm2)));
        std::env::remove_var("TERM_PROGRAM");

        std::env::set_var("TERM", "xterm-256color-sixel");
        assert!(matches!(detect_protocol(), Some(ImageProtocol::Sixel)));
        std::env::remove_var("TERM");
    }

    /// Task #81: a read-only temp dir makes ingest return PersistFailure
    /// (never the misleading "base64 decode failed").
    #[test]
    fn read_only_dir_yields_persist_failure() {
        let dir = temp_root("readonly");
        std::fs::create_dir_all(&dir).unwrap();
        // Drop write permission on the directory (unix).
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o555);
        std::fs::set_permissions(&dir, perms.clone()).unwrap();

        let mut p = ImagePipeline::new(&dir, Some(ImageProtocol::Kitty));
        // The ImagePipeline::new re-creates the dir with default perms; make
        // it read-only again AFTER construction.
        let mut p2perms = std::fs::metadata(&dir).unwrap().permissions();
        p2perms.set_mode(0o555);
        std::fs::set_permissions(&dir, p2perms).unwrap();

        let err = p.ingest(&b64(&png_bytes(2, 2))).unwrap_err();
        assert!(
            matches!(err, PlaceholderReason::PersistFailure(_)),
            "expected PersistFailure, got {err:?}"
        );
        // Restore permissions so cleanup can remove the dir.
        perms.set_mode(0o755);
        std::fs::set_permissions(&dir, perms).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

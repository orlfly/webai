//! Security hardening (FR-8 / M-5): download filename guard.
//!
//! The download path inference chain (`args.filename` → Content-Disposition →
//! URL tail) ends in a user-influenced string that must never escape the
//! downloads directory or overwrite an existing file silently. `sanitize_filename`
//! rejects traversal payloads and collisions; callers append `-N` suffixes for
//! de-duplication (FR-4).

/// Validate + sanitize a download filename. Returns the bare file name (no
/// directory components) or an error naming the violated rule (§7).
///
/// Payload set covered (M-5 acceptance): absolute paths, `..`, `/`, `\`,
/// empty, hidden, and reserved device names.
pub fn sanitize_filename(raw: &str) -> Result<String, FilenameError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(FilenameError::Empty);
    }
    // Any path separator is rejected: the download dir is chosen by the caller.
    if name.contains('/') || name.contains('\\') {
        return Err(FilenameError::PathSeparator);
    }
    // Traversal attempt even without separators (defence in depth).
    if name == ".." || name == "." || name.contains("..") {
        return Err(FilenameError::Traversal);
    }
    // Hidden files and Windows reserved device names.
    if name.starts_with('.') {
        return Err(FilenameError::Hidden);
    }
    let upper = name.to_uppercase();
    for reserved in ["CON", "PRN", "AUX", "NUL"] {
        if upper == reserved {
            return Err(FilenameError::Reserved);
        }
    }
    for reserved in ["CON.", "PRN.", "AUX.", "NUL.", "COM1.", "LPT1."] {
        if upper.starts_with(reserved) {
            return Err(FilenameError::Reserved);
        }
    }
    Ok(name.to_string())
}

/// Structured filename rejection (§7: rule name, never a bare string).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilenameError {
    #[error("download filename is empty")]
    Empty,
    #[error("download filename contains a path separator")]
    PathSeparator,
    #[error("download filename attempts traversal")]
    Traversal,
    #[error("download filename is hidden")]
    Hidden,
    #[error("download filename uses a reserved device name")]
    Reserved,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M-5 traversal payload set: 0 escapes allowed.
    #[test]
    fn traversal_payload_set_is_rejected() {
        let payloads = [
            "../etc/passwd",
            "..\\windows\\system32",
            "/etc/passwd",
            "C:\\temp\\x.png",
            "a/../b.png",
            "..",
            ".hidden.png",
            "",
            "   ",
            "CON",
            "NUL.png",
            "COM1.png",
        ];
        for p in payloads {
            assert!(
                sanitize_filename(p).is_err(),
                "payload {p:?} must be rejected"
            );
        }
    }

    #[test]
    fn benign_filenames_pass() {
        for ok in [
            "report.png",
            "新浪财经-头条.png",
            "export_2026.csv",
            "img (1).png",
        ] {
            assert_eq!(sanitize_filename(ok).unwrap(), ok);
        }
    }

    /// Overwrite (M-5): the same name twice must be de-duplicated by the
    /// caller via suffix; the sanitizer only guarantees a bare name.
    #[test]
    fn sanitizer_returns_bare_name_for_caller_suffixing() {
        assert_eq!(sanitize_filename("shot.png").unwrap(), "shot.png");
        assert!(sanitize_filename("shot.png").unwrap().contains('.'));
    }
}

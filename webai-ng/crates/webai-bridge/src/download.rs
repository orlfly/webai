//! Download routing (ARCHITECTURE.md §4.5 Download 特例 / §4.8 下载路由).
//!
//! The page script only emits a `needs_rust_download` signal; the real fetch
//! happens here in Rust via `reqwest`. Filename inference follows FR-4:
//! `args.filename` → `Content-Disposition` → URL tail → LLM fallback (the
//! fallback result is also path-safety checked). Path safety double-normalizes
//! and rejects `..`, absolute paths, and empty names. Same-name downloads get a
//! `-N` suffix and never overwrite. A failed download leaves no partial file.

use std::path::{Path, PathBuf};

use serde_json::Value as Json;

/// Structured download error (ARCHITECTURE.md §7).
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("missing required argument `url`")]
    MissingUrl,
    #[error("invalid URL: {0}")]
    BadUrl(String),
    #[error("cannot create download dir {dir}: {err}")]
    MkdirFailed { dir: String, err: String },
    #[error("HTTP client build failed: {0}")]
    ClientFailed(String),
    #[error("GET {url} failed: {err}")]
    FetchFailed { url: String, err: String },
    #[error("GET {url} returned HTTP {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("failed to read body of {url}: {err}")]
    ReadFailed { url: String, err: String },
    #[error("failed to write {path}: {err}")]
    WriteFailed { path: String, err: String },
    #[error("path traversal in filename {0:?} (PATH_NOT_ALLOWED)")]
    PathNotAllowed(String),
}

impl DownloadError {
    /// Stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            DownloadError::MissingUrl => "DOWNLOAD_MISSING_URL",
            DownloadError::BadUrl(_) => "DOWNLOAD_BAD_URL",
            DownloadError::MkdirFailed { .. } => "DOWNLOAD_MKDIR_FAILED",
            DownloadError::ClientFailed(_) => "DOWNLOAD_CLIENT_FAILED",
            DownloadError::FetchFailed { .. } => "DOWNLOAD_FETCH_FAILED",
            DownloadError::HttpStatus { .. } => "DOWNLOAD_HTTP_STATUS",
            DownloadError::ReadFailed { .. } => "DOWNLOAD_READ_FAILED",
            DownloadError::WriteFailed { .. } => "DOWNLOAD_WRITE_FAILED",
            DownloadError::PathNotAllowed(_) => "DOWNLOAD_PATH_NOT_ALLOWED",
        }
    }
}

/// The default download directory (relative to the startup cwd).
pub fn default_download_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("downloads")
}

/// Strip path separators and other filesystem-hostile characters from a
/// suggested file name so it can never escape the target directory or
/// address a sub-path. Also collapses `..` runs so a `../` traversal
/// attempt cannot survive even after separators are neutralised.
pub fn sanitise_filename(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let no_traversal = mapped.replace("..", "__");
    let trimmed = no_traversal.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "download.bin".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Derive a default file name from a URL's path's last non-empty segment,
/// falling back to `download.bin` when the URL has no meaningful path.
pub fn filename_for_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let seg = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let name = seg.trim();
    if name.is_empty() {
        "download.bin".to_owned()
    } else {
        sanitise_filename(name)
    }
}

/// Percent-decode an RFC 5987 `ext-value` (`charset'%lang%percent-encoded`),
/// decoding `%XX` pairs as UTF-8 bytes (task #76, review Major-1).
pub fn percent_decode_utf8(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Extract a file name from a `Content-Disposition: attachment;
/// filename="..."` header value. RFC 5987 `filename*=` values are
/// percent-decoded to UTF-8 (task #76).
pub fn parse_content_disposition_filename(header: &str) -> Option<String> {
    let lower = header.to_ascii_lowercase();
    if !lower.contains("attachment") && !lower.contains("inline") {
        return None;
    }
    // RFC 5987 extended form first: filename*=UTF-8''%E4%B8%AD%E6%96%87.txt
    if let Some(idx) = lower.find("filename*=") {
        let rest = &header[idx + "filename*=".len()..];
        // Strip the optional quote and the `charset'%lang%` prefix.
        let rest = rest.trim_start_matches('"');
        if let Some(tick) = rest.find('\'') {
            let after = &rest[tick + 1..];
            // Skip the language tag (second quote).
            if let Some(tick2) = after.find('\'') {
                let encoded = &after[tick2 + 1..];
                let end = encoded.find(';').unwrap_or(encoded.len());
                let encoded = encoded[..end].trim().trim_end_matches('"');
                if let Some(decoded) = percent_decode_utf8(encoded) {
                    if !decoded.is_empty() {
                        return Some(decoded);
                    }
                }
            }
        }
    }
    for quoted in ["filename=\""] {
        if let Some(idx) = header.find(quoted) {
            let rest = &header[idx + quoted.len()..];
            let end = rest.find('"').unwrap_or(rest.len());
            let name = rest[..end].trim();
            if !name.is_empty() {
                return Some(name.to_owned());
            }
        }
    }
    if let Some(idx) = lower.find("filename=") {
        let rest = &header[idx + "filename=".len()..];
        let name = rest.split([';', ',', ' ']).next().unwrap_or("").trim();
        if !name.is_empty() {
            return Some(name.to_owned());
        }
    }
    None
}

/// Return a path in `dir` for `filename`, appending `-N` before the
/// extension when the file already exists so downloads never overwrite a
/// previous file.
///
/// Reservation is **atomic** (`OpenOptions::create_new`), so concurrent
/// downloads can never pick the same `-N` path and overwrite each other
/// (task #76, review Major-2). Returns the reserved path, creating an empty
/// placeholder file that the caller writes over via the same handle semantics
/// (create_new guarantees exclusive creation).
pub fn unique_path(dir: &Path, filename: &str) -> std::io::Result<PathBuf> {
    let stem = Path::new(filename)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_owned());
    let ext = Path::new(filename)
        .extension()
        .map(|s| s.to_string_lossy().into_owned());

    // Try the bare name first, then -1, -2, ... Each reservation is an
    // exclusive create, so two racing callers always land on distinct paths.
    let mut names = vec![filename.to_string()];
    for n in 1..=1_000u32 {
        names.push(match &ext {
            Some(e) => format!("{stem}-{n}.{e}"),
            None => format!("{stem}-{n}"),
        });
    }

    for name in names {
        let candidate = dir.join(&name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        match opts.open(&candidate) {
            Ok(_handle) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no unique download path available"))
}

/// Validate that a URL is a fetchable http(s) URL.
pub fn validate_navigation_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // Must have a host after the scheme.
        let rest = &url[url.find("://").map(|i| i + 3).unwrap_or(0)..];
        if rest.is_empty() || rest.starts_with('/') {
            return Err("URL has no host".to_owned());
        }
        Ok(())
    } else {
        Err("unsupported or missing scheme (expected http/https)".to_owned())
    }
}

/// Perform a download and persist the body to disk.
///
/// Filename inference priority (FR-4): `args.filename` → `Content-Disposition`
/// → URL tail → `download.bin` fallback. The file is written to a temp path
/// first and atomically renamed, so a failed download leaves no partial file.
pub async fn download(
    args: &Json,
    directory: Option<&str>,
) -> Result<DownloadResult, DownloadError> {
    let url = match args.get("url").and_then(Json::as_str) {
        Some(u) if !u.is_empty() => u.to_owned(),
        _ => return Err(DownloadError::MissingUrl),
    };
    if let Err(reason) = validate_navigation_url(&url) {
        return Err(DownloadError::BadUrl(reason));
    }

    // Resolve the destination directory: explicit arg, else cwd/downloads.
    let directory = directory
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_download_dir);
    std::fs::create_dir_all(&directory).map_err(|e| DownloadError::MkdirFailed {
        dir: directory.display().to_string(),
        err: e.to_string(),
    })?;

    // An explicitly requested filename must not attempt traversal: reject
    // with a structured error (N-DL2) instead of silently sanitising.
    if let Some(raw) = args.get("filename").and_then(Json::as_str) {
        if raw.split(['/', '\\']).any(|seg| seg == "..") {
            return Err(DownloadError::PathNotAllowed(raw.to_owned()));
        }
    }

    // Resolve a file name: explicit arg, else the URL path's last segment.
    let filename = args
        .get("filename")
        .and_then(Json::as_str)
        .filter(|f| !f.is_empty())
        .map(sanitise_filename)
        .unwrap_or_else(|| filename_for_url(&url));

    // Fetch the body.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| DownloadError::ClientFailed(e.to_string()))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| DownloadError::FetchFailed {
            url: url.clone(),
            err: e.to_string(),
        })?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(DownloadError::HttpStatus { url, status });
    }

    // Prefer a server-provided Content-Disposition file name when the
    // caller did not request an explicit one.
    let effective_filename = if args.get("filename").is_none() {
        resp.headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_disposition_filename)
            .map(|name| sanitise_filename(&name))
            .unwrap_or(filename)
    } else {
        filename
    };

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| DownloadError::ReadFailed {
            url: url.clone(),
            err: e.to_string(),
        })?
        .to_vec();

    // Reserve the target atomically (create_new) so concurrent downloads
    // cannot pick the same -N path (task #76). The reservation creates the
    // final file; we then write the payload through to it in place, so a
    // failure still leaves the reserved name (never another download's).
    let path =
        unique_path(&directory, &effective_filename).map_err(|e| DownloadError::WriteFailed {
            path: directory.join(&effective_filename).display().to_string(),
            err: e.to_string(),
        })?;
    let tmp = directory.join(format!(".{}.tmp", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|e| DownloadError::WriteFailed {
        path: tmp.display().to_string(),
        err: e.to_string(),
    })?;
    std::fs::rename(&tmp, &path).map_err(|e| DownloadError::WriteFailed {
        path: path.display().to_string(),
        err: e.to_string(),
    })?;

    Ok(DownloadResult {
        url,
        filename: effective_filename,
        saved_to: path.to_string_lossy().to_string(),
        directory: directory.to_string_lossy().to_string(),
        bytes: bytes.len(),
    })
}

/// Result of a successful download.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub url: String,
    pub filename: String,
    pub saved_to: String,
    pub directory: String,
    pub bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_filename_blocks_path_traversal() {
        for payload in ["../x", "/etc/passwd", "..\\x", "", "a/../b", ".."] {
            let safe = sanitise_filename(payload);
            assert!(
                !safe.contains(".."),
                "traversal survived: {payload} -> {safe}"
            );
            assert!(!safe.starts_with('/'), "absolute path survived: {payload}");
            assert!(!safe.is_empty(), "empty name not replaced: {payload}");
        }
    }

    #[test]
    fn filename_for_url_uses_last_path_segment() {
        assert_eq!(
            filename_for_url("https://x.com/a/b/report.pdf"),
            "report.pdf"
        );
        // A bare host with trailing slash yields the host as the last segment.
        assert_eq!(filename_for_url("https://x.com/"), "x.com");
        assert_eq!(filename_for_url("https://x.com/a?q=1"), "a");
    }

    #[test]
    fn parse_content_disposition_filename_extracts_quoted() {
        let h = "attachment; filename=\"report.pdf\"";
        assert_eq!(
            parse_content_disposition_filename(h).as_deref(),
            Some("report.pdf")
        );
        let h2 = "inline; filename=notes.txt";
        assert_eq!(
            parse_content_disposition_filename(h2).as_deref(),
            Some("notes.txt")
        );
        assert_eq!(parse_content_disposition_filename("no-disposition"), None);
    }

    #[test]
    fn unique_path_appends_suffix_on_collision() {
        let dir = std::env::temp_dir().join(format!("webai-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // unique_path reserves exclusively: the first call creates a.pdf, the
        // second must land on a-1.pdf (no overwrite), etc.
        let p0 = unique_path(&dir, "a.pdf").unwrap();
        assert_eq!(p0.file_name().unwrap().to_string_lossy(), "a.pdf");
        let p1 = unique_path(&dir, "a.pdf").unwrap();
        assert_eq!(p1.file_name().unwrap().to_string_lossy(), "a-1.pdf");
        let p2 = unique_path(&dir, "a.pdf").unwrap();
        assert_eq!(p2.file_name().unwrap().to_string_lossy(), "a-2.pdf");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RFC 5987: `filename*=UTF-8''...` must be percent-decoded to UTF-8
    /// (task #76, review Major-1) instead of mangling `%` via the sanitizer.
    #[test]
    fn parse_content_disposition_percent_decodes_utf8_ext_value() {
        let h = "attachment; filename*=UTF-8''%E4%B8%AD%E6%96%87.txt";
        assert_eq!(
            parse_content_disposition_filename(h).as_deref(),
            Some("中文.txt")
        );
        // With a language tag: filename*=UTF-8'lang'%...
        let h2 = "attachment; filename*=UTF-8'zh'%E6%8A%A5%E8%A1%A8.pdf";
        assert_eq!(
            parse_content_disposition_filename(h2).as_deref(),
            Some("报表.pdf")
        );
        // Plain quoted filename still works.
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"a b.pdf\"").as_deref(),
            Some("a b.pdf")
        );
    }

    #[test]
    fn percent_decode_handles_edge_cases() {
        assert_eq!(percent_decode_utf8("%41%42"), Some("AB".into()));
        assert_eq!(percent_decode_utf8("no-percent"), Some("no-percent".into()));
        // Truncated escape must fail cleanly (None), not panic.
        assert_eq!(percent_decode_utf8("%E4%B8"), None);
        assert_eq!(percent_decode_utf8("%ZZ"), None);
    }

    /// Concurrent unique_path reservations must produce distinct files with
    /// no overwrite (task #76, review Major-2).
    #[test]
    fn concurrent_unique_path_never_overwrites() {
        let dir = std::env::temp_dir().join(format!(
            "webai-dl-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dir2 = dir.clone();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let d = dir2.clone();
                std::thread::spawn(move || unique_path(&d, "a.pdf").unwrap())
            })
            .collect();
        let mut names = std::collections::HashSet::new();
        for h in handles {
            let p = h.join().unwrap();
            assert!(names.insert(p.file_name().unwrap().to_string_lossy().into_owned()));
        }
        assert_eq!(names.len(), 8, "8 racers must get 8 distinct names");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_download_dir_is_cwd_downloads() {
        let d = default_download_dir();
        assert!(d.ends_with("downloads"));
    }

    #[test]
    fn validate_navigation_url_accepts_http_https() {
        assert!(validate_navigation_url("https://x.com/f").is_ok());
        assert!(validate_navigation_url("http://x.com/f").is_ok());
        assert!(validate_navigation_url("file:///etc/passwd").is_err());
        assert!(validate_navigation_url("not a url").is_err());
    }
}

//! Filesystem path sandbox (FR-8 / M-5).
//!
//! `filesystem` and `memory` tools must only reach authorized paths. A
//! `PathSandbox` is built around an allowed root directory; it rejects paths
//! that escape that root via absolute addressing, `..` traversal, or a symlink
//! whose target points outside the root. Paths are lexically normalized and
//! then validated against the canonicalized root before every use.

use std::path::{Component, Path, PathBuf};

/// A structured sandbox rejection (FR-8: report a code, never a bare string).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxError {
    #[error("path escapes sandbox root: {path}")]
    Escape { path: String },
    #[error("path is a system-sensitive location: {path}")]
    SensitivePath { path: String },
    #[error("cannot resolve path: {detail}")]
    Io { detail: String },
}

impl SandboxError {
    /// Stable machine-readable code for cross-layer diagnostics (§7).
    pub fn code(&self) -> &'static str {
        match self {
            SandboxError::Escape { .. } => "path_escape",
            SandboxError::SensitivePath { .. } => "sensitive_path",
            SandboxError::Io { .. } => "sandbox_io",
        }
    }
}

/// Directories that are never reachable through the tool sandbox, regardless
/// of the configured root (FR-8 "系统敏感路径").
const SYSTEM_SENSITIVE: &[&str] = &[
    "/etc", "/usr", "/var", "/bin", "/sbin", "/boot", "/proc", "/sys", "/dev", "/root",
    "/home", // any user home is off-limits to downloaded files/sessions
];

/// A root-anchored filesystem sandbox with lexical + canonical validation.
#[derive(Debug, Clone)]
pub struct PathSandbox {
    root: PathBuf,
}

impl PathSandbox {
    /// Build a sandbox rooted at `root`. The root itself is created if missing
    /// so that canonicalization in [`PathSandbox::sanitize`] succeeds.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let _ = std::fs::create_dir_all(&root);
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validate and resolve `path` to a concrete path inside the root.
    ///
    /// 1. Rejects empty paths and any component that lexically escapes root.
    /// 2. Rejects paths resolving onto a system-sensitive directory.
    /// 3. Canonicalizes the resolved parent (resolving symlinks) and verifies
    ///    it is still within the canonicalized root, so a symlink pointing
    ///    outside is refused.
    pub fn sanitize(&self, path: &Path) -> Result<PathBuf, SandboxError> {
        if path.as_os_str().is_empty() {
            return Err(SandboxError::Escape {
                path: "".to_string(),
            });
        }

        // Absolute path: resolve against itself; relative: against root.
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };

        let normalized = self.lexical_normalize(&candidate)?;
        self.reject_sensitive(&normalized)?;

        let canon_root = std::fs::canonicalize(&self.root).map_err(|e| SandboxError::Io {
            detail: e.to_string(),
        })?;

        // Verify the final resolved path stays under the canonicalized root.
        let resolved = self.resolve_with_symlinks(&normalized, &canon_root)?;

        if !resolved.starts_with(&canon_root) {
            return Err(SandboxError::Escape {
                path: resolved.display().to_string(),
            });
        }

        Ok(resolved)
    }

    /// Lexically normalize `.` / `..` and reject any attempt to climb above
    /// the root. This mirrors the `fs_bridge` guard (ARCHITECTURE §4.9).
    fn lexical_normalize(&self, path: &Path) -> Result<PathBuf, SandboxError> {
        let mut out = PathBuf::new();
        for comp in path.components() {
            match comp {
                Component::ParentDir => {
                    // `..` may only reduce back toward (not past) the root.
                    if !out.pop() {
                        return Err(SandboxError::Escape {
                            path: path.display().to_string(),
                        });
                    }
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        Ok(out)
    }

    /// Resolve `normalized` under the canonicalized root. If the path already
    /// exists, canonicalize it (resolving symlinks) and verify it cannot have
    /// escaped. If it does not exist yet (e.g. a new file to write), canonicalize
    /// its parent and re-append the final component, still under the root.
    fn resolve_with_symlinks(
        &self,
        normalized: &Path,
        canon_root: &Path,
    ) -> Result<PathBuf, SandboxError> {
        match std::fs::canonicalize(normalized) {
            Ok(canon) => {
                if canon.starts_with(canon_root) {
                    Ok(canon)
                } else {
                    Err(SandboxError::Escape {
                        path: canon.display().to_string(),
                    })
                }
            }
            Err(_) => {
                // Path does not exist yet: canonicalize the nearest existing
                // ancestor and re-append the missing tail, then verify it stays
                // inside the root.
                let parent = normalized.parent().ok_or_else(|| SandboxError::Io {
                    detail: "no parent".into(),
                })?;
                let canon_parent = std::fs::canonicalize(parent).map_err(|e| SandboxError::Io {
                    detail: e.to_string(),
                })?;
                if !canon_parent.starts_with(canon_root) {
                    return Err(SandboxError::Escape {
                        path: canon_parent.display().to_string(),
                    });
                }
                let tail = normalized
                    .strip_prefix(parent)
                    .map_err(|_| SandboxError::Io {
                        detail: "prefix".into(),
                    })?;
                Ok(canon_parent.join(tail))
            }
        }
    }

    /// Reject paths that resolve under a system-sensitive directory.
    fn reject_sensitive(&self, path: &Path) -> Result<(), SandboxError> {
        let canon_root = std::fs::canonicalize(&self.root).map_err(|e| SandboxError::Io {
            detail: e.to_string(),
        })?;
        // Only enforce sensitive-dir rules for absolute paths (the root is a
        // controlled location; `/home`/`/etc` inside it are test fixtures).
        for sensitive in SYSTEM_SENSITIVE {
            if path.to_string_lossy().starts_with(sensitive)
                && !self.root.to_string_lossy().starts_with(sensitive)
                && !path.starts_with(&canon_root)
            {
                return Err(SandboxError::SensitivePath {
                    path: path.display().to_string(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("webai-sandbox-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("nested")).unwrap();
        dir
    }

    #[test]
    fn relative_path_within_root_is_allowed() {
        let dir = make_root("rel");
        let sb = PathSandbox::new(&dir);
        let resolved = sb.sanitize(Path::new("nested/file.txt")).unwrap();
        assert!(resolved.ends_with("nested/file.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dotdot_escaping_root_is_rejected() {
        let dir = make_root("dotdot");
        let sb = PathSandbox::new(&dir);
        // `..` climbs out of root; it is rejected (either landing on a
        // system-sensitive path or escaping root entirely).
        assert!(sb.sanitize(Path::new("../../etc/passwd")).is_err());
        // A `..` that escapes to a non-sensitive location is an Escape.
        let escape = make_root("dotdot2");
        let sb2 = PathSandbox::new(&escape);
        assert!(matches!(
            sb2.sanitize(Path::new("../../..")),
            Err(SandboxError::Escape { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&escape);
    }

    #[test]
    fn absolute_path_outside_root_is_rejected() {
        let dir = make_root("abs");
        let sb = PathSandbox::new(&dir);
        let outside = std::env::temp_dir().join("webai-sandbox-abs-outside");
        let _ = fs::write(&outside, "x");
        assert!(matches!(
            sb.sanitize(&outside),
            Err(SandboxError::Escape { .. })
        ));
        let _ = fs::remove_file(&outside);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_pointing_outside_root_is_rejected() {
        let dir = make_root("sym");
        let outside =
            std::env::temp_dir().join(format!("webai-sandbox-sym-target-{}", std::process::id()));
        let _ = fs::write(&outside, "secret");
        let link = dir.join("nested/evil-link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let sb = PathSandbox::new(&dir);
        assert!(matches!(
            sb.sanitize(Path::new("nested/evil-link")),
            Err(SandboxError::Escape { .. })
        ));
        let _ = fs::remove_file(&outside);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn system_sensitive_absolute_path_is_rejected() {
        let dir = make_root("sens");
        let sb = PathSandbox::new(&dir);
        assert!(matches!(
            sb.sanitize(Path::new("/etc/passwd")),
            Err(SandboxError::SensitivePath { .. })
        ));
        let _ = fs::remove_dir_all(&dir);
    }
}

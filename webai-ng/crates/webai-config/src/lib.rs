//! TOML configuration loading (five-file schema).
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). Defines the five-file TOML
//! schema surface (ARCHITECTURE.md §4.2): `config.toml`,
//! `config_llm.toml`, `config_embd.toml`, `config_mem.toml`, `config_vec.toml`,
//! with `WEBAI_CONFIG` path override, fail-fast on load errors, and graceful
//! degradation to no-memory when the memory backend is missing.

use std::path::{Path, PathBuf};

/// The overall agent configuration (ARCHITECTURE.md §4.2, config.toml).
#[derive(Debug, Clone, Default)]
pub struct WebaiConfig {
    pub llm: String,
    pub memory: String,
    pub tools: Vec<String>,
    pub max_steps: u32,
    pub duplicate_threshold: u32,
    pub auto_plan_on_multi_step: bool,
    pub script_memory_enabled: bool,
}

/// Loaded view of the five-file schema.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub root: PathBuf,
    pub config: WebaiConfig,
    /// Directory the config files live in (default `<root>/config` unless
    /// overridden by `WEBAI_CONFIG`).
    pub config_dir: PathBuf,
}

impl LoadedConfig {
    /// Resolve the config directory honoring `WEBAI_CONFIG`.
    pub fn resolve_config_dir(root: &Path, env: Option<&str>) -> PathBuf {
        if let Some(p) = env.filter(|s| !s.is_empty()) {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                pb.parent().map(|d| d.to_path_buf()).unwrap_or(root.to_path_buf())
            } else {
                pb
            }
        } else {
            root.join("config")
        }
    }
}

/// Configuration load errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file missing: {0}")]
    Missing(String),
    #[error("failed to parse {path}: {detail}")]
    Parse { path: PathBuf, detail: String },
    #[error("unknown LLM profile: {0}")]
    UnknownLlmProfile(String),
}

/// Load the full five-file configuration (stub: reads only `config.toml` if
/// present, otherwise returns defaults). Real file parsing lands in M1's config
/// follow-up.
pub fn load(root: &Path) -> Result<LoadedConfig, ConfigError> {
    let config_dir = LoadedConfig::resolve_config_dir(root, std::env::var("WEBAI_CONFIG").ok().as_deref());
    let config_toml = config_dir.join("config.toml");
    if config_toml.exists() {
        let raw = std::fs::read_to_string(&config_toml)
            .map_err(|e| ConfigError::Parse { path: config_toml.clone(), detail: e.to_string() })?;
        let _ = raw;
        // Full TOML mapping deferred; return defaults for the skeleton.
    }
    Ok(LoadedConfig {
        root: root.to_path_buf(),
        config: WebaiConfig {
            llm: "deepseek-v4-flash".into(),
            memory: "default".into(),
            tools: vec![],
            max_steps: 30,
            duplicate_threshold: 2,
            auto_plan_on_multi_step: true,
            script_memory_enabled: true,
        },
        config_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_config_dir_from_weba_config_env_file_uses_parent() {
        let root = Path::new("/repo");
        // Point WEBAI_CONFIG at a real file: its parent is the config dir.
        let dir = std::env::temp_dir().join(format!("webai-cfg-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        std::fs::write(&file, "").unwrap();
        let d = LoadedConfig::resolve_config_dir(root, Some(file.to_str().unwrap()));
        assert_eq!(d, dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_config_dir_treats_nonexistent_env_path_as_dir() {
        let root = Path::new("/repo");
        // A path that is not an existing file is treated as a directory.
        let d = LoadedConfig::resolve_config_dir(root, Some("/nonexistent/webai/config"));
        assert_eq!(d, Path::new("/nonexistent/webai/config"));
    }

    #[test]
    fn resolve_config_dir_defaults_to_root_config() {
        let root = Path::new("/repo");
        let d = LoadedConfig::resolve_config_dir(root, None);
        assert_eq!(d, Path::new("/repo/config"));
    }

    #[test]
    fn load_returns_defaults_without_config_dir() {
        let t = std::env::temp_dir().join(format!("webai-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&t).unwrap();
        let cfg = load(&t).unwrap();
        assert_eq!(cfg.config.max_steps, 30);
        assert_eq!(cfg.config.duplicate_threshold, 2);
        assert!(cfg.config.script_memory_enabled);
        let _ = std::fs::remove_dir_all(&t);
    }
}

//! TOML configuration loading (five-file schema).
//!
//! Implements the five-file TOML schema described in `docs/architecture/ARCHITECTURE.md` §4.2
//! and `docs/specs/PRODUCT-DESIGN.md` §7:
//!
//! | file | purpose | load failure |
//! |---|---|---|
//! | `agent.toml` | agent loop: llm profile, memory, tools, guards | **fail-fast** |
//! | `llm.toml` | LLM provider profiles | **fail-fast** |
//! | `embd.toml` | embedding backend | graceful degrade (no memory) |
//! | `mem.toml` | memory backend | graceful degrade (no memory) |
//! | `vec.toml` | vector store | graceful degrade (no memory) |
//!
//! Default config directory is `~/.webai/config/`, overridable via the
//! `WEBAI_CONFIG` environment variable (a directory, or a file whose parent is
//! used). `agent.toml` / `llm.toml` load failures abort startup (fail-fast);
//! missing or corrupt `embd.toml` / `mem.toml` / `vec.toml` degrade to a
//! no-memory mode rather than aborting.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Default config directory relative to the user's home.
pub const DEFAULT_CONFIG_DIR: &str = ".webai/config";

/// The five config file names (PRODUCT-DESIGN §7).
pub const AGENT_FILE: &str = "agent.toml";
pub const LLM_FILE: &str = "llm.toml";
pub const EMBEDDING_FILE: &str = "embd.toml";
pub const MEMORY_FILE: &str = "mem.toml";
pub const VECTOR_FILE: &str = "vec.toml";

/// Agent-loop configuration (`agent.toml`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Name of the LLM profile to use (must match a `[<name>]` table in `llm.toml`).
    pub llm: String,
    /// Memory profile selector.
    pub memory: String,
    /// Extra tool registry entries (the five core tools are always registered).
    pub tools: Vec<String>,
    /// Max plan→act→observe cycles per prompt (cost guard).
    pub max_steps: u32,
    /// Identical observations before the loop assumes it is stuck.
    pub duplicate_threshold: u32,
    /// Auto-decompose multi-step requests into a plan.
    pub auto_plan_on_multi_step: bool,
    /// Reuse remembered scripts.
    pub script_memory_enabled: bool,
}

impl AgentConfig {
    /// Sensible out-of-the-box defaults (PRODUCT-DESIGN §7: "默认值开箱即用").
    pub fn defaults() -> Self {
        Self {
            llm: "deepseek-v4-flash".into(),
            memory: "default".into(),
            tools: vec![],
            max_steps: 30,
            duplicate_threshold: 2,
            auto_plan_on_multi_step: true,
            script_memory_enabled: true,
        }
    }
}

/// A single LLM provider profile (`llm.toml` `[<name>]` table).
#[derive(Debug, Clone, Deserialize)]
pub struct LlmProfile {
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub enable_tool: bool,
}

/// LLM configuration: a map of profile name → profile.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LlmConfig {
    #[serde(flatten)]
    pub profiles: std::collections::HashMap<String, LlmProfile>,
}

/// Embedding backend configuration (`embd.toml`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    pub backend: String,
    pub model: String,
    pub dim: usize,
}

/// Memory backend configuration (`mem.toml`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub backend: String,
    pub session_dir: String,
}

/// Vector store configuration (`vec.toml`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VectorConfig {
    pub backend: String,
    pub index_path: String,
}

/// The fully-loaded five-file configuration.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// Directory the config files were read from.
    pub config_dir: PathBuf,
    pub agent: AgentConfig,
    pub llm: LlmConfig,
    pub embedding: Option<EmbeddingConfig>,
    pub memory: Option<MemoryConfig>,
    pub vector: Option<VectorConfig>,
}

impl LoadedConfig {
    /// True when the memory-related files (embd/mem/vec) were all present and
    /// parsed, i.e. memory is enabled. Missing/corrupt memory files degrade to
    /// no-memory mode.
    pub fn memory_enabled(&self) -> bool {
        self.embedding.is_some() && self.memory.is_some() && self.vector.is_some()
    }
}

/// Configuration load errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file missing: {0}")]
    Missing(String),
    #[error("failed to parse {path}: {detail}")]
    Parse { path: PathBuf, detail: String },
    #[error("unknown LLM profile `{0}` referenced by agent.toml")]
    UnknownLlmProfile(String),
}

/// Resolve the config directory honoring `WEBAI_CONFIG`.
///
/// - If `WEBAI_CONFIG` is set to an existing file, its parent directory is used.
/// - If set to a directory (existing or not), that directory is used.
/// - Otherwise default to `~/.webai/config/`.
pub fn resolve_config_dir(env: Option<&str>) -> PathBuf {
    if let Some(p) = env.filter(|s| !s.is_empty()) {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return pb.parent().map(|d| d.to_path_buf()).unwrap_or_else(default_dir);
        }
        return pb;
    }
    default_dir()
}

fn default_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(DEFAULT_CONFIG_DIR)
    } else {
        PathBuf::from(DEFAULT_CONFIG_DIR)
    }
}

/// Load the full five-file configuration.
///
/// `agent.toml` and `llm.toml` are required: a missing or corrupt file is a
/// hard error (fail-fast). `embd.toml` / `mem.toml` / `vec.toml` are optional:
/// a missing or corrupt file degrades to no-memory mode (returns `None` for that
/// file) rather than aborting.
pub fn load() -> Result<LoadedConfig, ConfigError> {
    let config_dir = resolve_config_dir(std::env::var("WEBAI_CONFIG").ok().as_deref());
    load_from(&config_dir)
}

/// Load configuration from an explicit directory (used by tests).
pub fn load_from(config_dir: &Path) -> Result<LoadedConfig, ConfigError> {
    // agent.toml — fail-fast.
    let agent = read_required::<AgentConfig>(config_dir, AGENT_FILE)?;
    // llm.toml — fail-fast.
    let llm = read_required::<LlmConfig>(config_dir, LLM_FILE)?;

    // Validate the agent's llm profile reference.
    if !agent.llm.is_empty() && !llm.profiles.contains_key(&agent.llm) {
        return Err(ConfigError::UnknownLlmProfile(agent.llm.clone()));
    }

    // embd/mem/vec — graceful degrade on missing or corrupt.
    let embedding = read_optional::<EmbeddingConfig>(config_dir, EMBEDDING_FILE);
    let memory = read_optional::<MemoryConfig>(config_dir, MEMORY_FILE);
    let vector = read_optional::<VectorConfig>(config_dir, VECTOR_FILE);

    Ok(LoadedConfig {
        config_dir: config_dir.to_path_buf(),
        agent,
        llm,
        embedding,
        memory,
        vector,
    })
}

/// Read and parse a required file; missing or corrupt is a hard error.
fn read_required<T: for<'de> Deserialize<'de>>(dir: &Path, name: &str) -> Result<T, ConfigError> {
    let path = dir.join(name);
    let raw = std::fs::read_to_string(&path).map_err(|_| ConfigError::Missing(path.display().to_string()))?;
    toml::from_str(&raw).map_err(|e| ConfigError::Parse {
        path: path.clone(),
        detail: e.to_string(),
    })
}

/// Read and parse an optional file; missing or corrupt returns `None` (degrade).
fn read_optional<T: for<'de> Deserialize<'de>>(dir: &Path, name: &str) -> Option<T> {
    let path = dir.join(name);
    let raw = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a temp config dir with the given files.
    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("webai-cfg-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn resolve_config_dir_defaults_to_home() {
        let d = resolve_config_dir(None);
        assert!(d.ends_with(".webai/config"));
    }

    #[test]
    fn resolve_config_dir_uses_env_dir() {
        let d = resolve_config_dir(Some("/tmp/webai-custom"));
        assert_eq!(d, PathBuf::from("/tmp/webai-custom"));
    }

    #[test]
    fn resolve_config_dir_uses_env_file_parent() {
        let dir = temp_dir();
        let file = dir.join("agent.toml");
        fs::write(&file, "").unwrap();
        let d = resolve_config_dir(Some(file.to_str().unwrap()));
        assert_eq!(d, dir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_normal_config_succeeds() {
        let dir = temp_dir();
        write(&dir, AGENT_FILE, r#"
llm = "deepseek-v4-flash"
memory = "default"
max_steps = 30
duplicate_threshold = 2
auto_plan_on_multi_step = true
script_memory_enabled = true
"#);
        write(&dir, LLM_FILE, r#"
[deepseek-v4-flash]
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"
endpoint = "/v1/chat/completions"
"#);
        write(&dir, EMBEDDING_FILE, "backend = \"bge-m3\"\nmodel = \"BAAI/bge-m3\"\ndim = 1024\n");
        write(&dir, MEMORY_FILE, "backend = \"kuzu\"\nsession_dir = \"~/.webai/sessions\"\n");
        write(&dir, VECTOR_FILE, "backend = \"hnsw\"\nindex_path = \"~/.webai/vec\"\n");

        let cfg = load_from(&dir).unwrap();
        assert_eq!(cfg.agent.llm, "deepseek-v4-flash");
        assert_eq!(cfg.agent.max_steps, 30);
        assert_eq!(cfg.llm.profiles.len(), 1);
        assert!(cfg.memory_enabled());
        assert_eq!(cfg.embedding.as_ref().unwrap().dim, 1024);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_agent_fails_fast() {
        let dir = temp_dir();
        write(&dir, LLM_FILE, "[deepseek-v4-flash]\nmodel = \"m\"\n");
        let err = load_from(&dir).unwrap_err();
        assert!(matches!(err, ConfigError::Missing(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_corrupt_agent_fails_fast() {
        let dir = temp_dir();
        write(&dir, AGENT_FILE, "not = = valid toml [[[");
        write(&dir, LLM_FILE, "[deepseek-v4-flash]\nmodel = \"m\"\n");
        let err = load_from(&dir).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_llm_fails_fast() {
        let dir = temp_dir();
        write(&dir, AGENT_FILE, "llm = \"deepseek-v4-flash\"\n");
        let err = load_from(&dir).unwrap_err();
        assert!(matches!(err, ConfigError::Missing(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_unknown_llm_profile_is_error() {
        let dir = temp_dir();
        write(&dir, AGENT_FILE, "llm = \"nope\"\n");
        write(&dir, LLM_FILE, "[deepseek-v4-flash]\nmodel = \"m\"\n");
        let err = load_from(&dir).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownLlmProfile(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_memory_files_degrades_gracefully() {
        let dir = temp_dir();
        write(&dir, AGENT_FILE, "llm = \"deepseek-v4-flash\"\n");
        write(&dir, LLM_FILE, "[deepseek-v4-flash]\nmodel = \"m\"\n");
        // No embd/mem/vec files.
        let cfg = load_from(&dir).unwrap();
        assert!(!cfg.memory_enabled());
        assert!(cfg.embedding.is_none());
        assert!(cfg.memory.is_none());
        assert!(cfg.vector.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_corrupt_memory_file_degrades_gracefully() {
        let dir = temp_dir();
        write(&dir, AGENT_FILE, "llm = \"deepseek-v4-flash\"\n");
        write(&dir, LLM_FILE, "[deepseek-v4-flash]\nmodel = \"m\"\n");
        write(&dir, EMBEDDING_FILE, "not = = valid [[[");
        // mem/vec missing.
        let cfg = load_from(&dir).unwrap();
        assert!(!cfg.memory_enabled());
        assert!(cfg.embedding.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_partial_memory_files_degrades() {
        let dir = temp_dir();
        write(&dir, AGENT_FILE, "llm = \"deepseek-v4-flash\"\n");
        write(&dir, LLM_FILE, "[deepseek-v4-flash]\nmodel = \"m\"\n");
        write(&dir, EMBEDDING_FILE, "backend = \"bge-m3\"\n");
        // mem/vec missing → memory disabled.
        let cfg = load_from(&dir).unwrap();
        assert!(!cfg.memory_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_defaults_are_sane() {
        let d = AgentConfig::defaults();
        assert_eq!(d.max_steps, 30);
        assert_eq!(d.duplicate_threshold, 2);
        assert!(d.auto_plan_on_multi_step);
        assert!(d.script_memory_enabled);
    }
}

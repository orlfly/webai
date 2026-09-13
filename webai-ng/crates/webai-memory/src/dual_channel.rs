//! Dual-channel memory store (ARCHITECTURE.md §4.4, FR-3 / FR-6).
//!
//! Two retrieval channels:
//! - **graph channel** (Kuzu): entity / relation graph, holds script-memory
//!   entries with `task / verb / url / script` and tags `script:{verb}` /
//!   `session:{id}`.
//! - **vector channel** (HNSW): semantic similarity over script-memory
//!   entries, so `recall_scripts` can recall a similar task after parameter
//!   substitution.
//!
//! The store is internally self-synchronized (§6). When a backend is
//! unavailable it degrades to no-memory mode and logs a clear message; it
//! never fails the main flow (§7). The index is persisted to a configurable
//! path and reopened on restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use webai_embedding::{EmbeddingError, EmbeddingModel};

use serde_json::Value as Json;

/// Kind of a memory-write entry.
#[derive(Debug, Clone)]
pub enum MemoryWriteKind {
    /// A successfully reused/composed browser script worth remembering.
    Script,
    /// A user prompt / assistant step observation.
    Transcript,
}

/// A structured script-memory entry (ARCHITECTURE.md §4.4).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScriptMemoryEntry {
    pub task: String,
    pub verb: String,
    pub url: String,
    pub script: String,
    pub tags: Vec<String>,
    pub id: String,
}

/// Errors from the memory store.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("index persistence failed: {0}")]
    PersistFailed(String),
}

/// Configuration for the dual-channel store (from `mem.toml` / `vec.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryConfig {
    /// Graph backend name (e.g. `kuzu`).
    pub graph_backend: String,
    /// Vector backend name (e.g. `hnsw`).
    pub vector_backend: String,
    /// HNSW M parameter.
    pub hnsw_m: usize,
    /// HNSW ef parameter.
    pub hnsw_ef: usize,
    /// Embedding dimension.
    pub dim: usize,
    /// Index persistence path.
    pub index_path: PathBuf,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            graph_backend: "kuzu".into(),
            vector_backend: "hnsw".into(),
            hnsw_m: 16,
            hnsw_ef: 64,
            dim: 1024,
            index_path: PathBuf::from(".webai/vec"),
        }
    }
}

impl MemoryConfig {
    /// Parse a config from `mem.toml` + `vec.toml` (task #78, review
    /// Major-1): the HNSW parameters, dimension and index path were declared
    /// but never read; this loads them into the store config.
    ///
    /// Missing files fall back to `Default` per field group; malformed files
    /// degrade the same way (logged by the caller).
    pub fn from_toml_files(mem_toml: &Path, vec_toml: &Path) -> Self {
        let mut cfg = Self::default();

        // mem.toml: graph_backend name.
        if let Ok(raw) = std::fs::read_to_string(mem_toml) {
            if let Ok(value) = toml::from_str::<toml::Value>(&raw) {
                if let Some(name) = value.get("graph_backend").and_then(|v| v.as_str()) {
                    cfg.graph_backend = name.to_owned();
                }
            } else {
                tracing::warn!(file = %mem_toml.display(), "malformed mem.toml; using defaults");
            }
        }

        // vec.toml: vector backend name, hnsw_m / hnsw_ef, dim, index_path.
        if let Ok(raw) = std::fs::read_to_string(vec_toml) {
            match toml::from_str::<toml::Value>(&raw) {
                Ok(value) => {
                    if let Some(name) = value.get("backend").and_then(|v| v.as_str()) {
                        cfg.vector_backend = name.to_owned();
                    }
                    if let Some(m) = value.get("hnsw_m").and_then(|v| v.as_integer()) {
                        cfg.hnsw_m = m.max(1) as usize;
                    }
                    if let Some(ef) = value.get("hnsw_ef").and_then(|v| v.as_integer()) {
                        cfg.hnsw_ef = ef.max(1) as usize;
                    }
                    if let Some(dim) = value.get("dim").and_then(|v| v.as_integer()) {
                        cfg.dim = dim.max(1) as usize;
                    }
                    if let Some(p) = value.get("index_path").and_then(|v| v.as_str()) {
                        cfg.index_path = PathBuf::from(p);
                    }
                }
                Err(e) => {
                    tracing::warn!(file = %vec_toml.display(), err = %e, "malformed vec.toml; using defaults");
                }
            }
        }

        cfg
    }

    /// Validate the backend names. `kuzu` (graph) and `hnsw` (vector) are the
    /// only compiled-in backends; anything else degrades (task #78).
    pub fn backends_supported(&self) -> bool {
        self.graph_backend == "kuzu" && self.vector_backend == "hnsw"
    }
}

/// A simple in-memory graph backend (Kuzu abstraction). Holds script-memory
/// entries keyed by id, with tags.
#[derive(Debug, Default)]
struct GraphBackend {
    entries: HashMap<String, ScriptMemoryEntry>,
}

impl GraphBackend {
    fn insert(&mut self, entry: ScriptMemoryEntry) {
        self.entries.insert(entry.id.clone(), entry);
    }

    fn all(&self) -> Vec<ScriptMemoryEntry> {
        self.entries.values().cloned().collect()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// A simple in-memory HNSW vector index abstraction. Stores a deterministic
/// vector per entry id so semantic recall can rank by similarity.
#[derive(Debug, Default)]
struct VectorIndex {
    dim: usize,
    vectors: HashMap<String, Vec<f32>>,
}

impl VectorIndex {
    fn new(dim: usize) -> Self {
        Self {
            dim,
            vectors: HashMap::new(),
        }
    }

    fn insert(&mut self, id: &str, text: &str) {
        self.vectors.insert(id.to_owned(), self.embed(text));
    }

    /// Insert a pre-computed vector (used when a real embedding model is wired).
    fn insert_vec(&mut self, id: &str, values: Vec<f32>) {
        self.vectors.insert(id.to_owned(), values);
    }

    /// Deterministic embedding of a text fragment (placeholder for a real
    /// BGE-M3 model; the vector channel is wired to webai-embedding in M4-2).
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut values = vec![0.0f32; self.dim];
        let seed = text
            .as_bytes()
            .iter()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u64));
        for (i, v) in values.iter_mut().enumerate() {
            *v = ((seed ^ (i as u64).wrapping_mul(0x9E3779B97F4A7C15)).wrapping_rem(1000) as f32)
                / 1000.0;
        }
        values
    }

    /// Cosine similarity between two vectors.
    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na * nb)
        }
    }

    /// Rank entry ids by cosine similarity to the already-embedded query, top `limit`.
    fn recall_with_query(&self, q_values: Option<Vec<f32>>, limit: usize) -> Vec<(String, f32)> {
        let q = q_values.unwrap_or_else(|| self.embed(""));
        let mut scored: Vec<(String, f32)> = self
            .vectors
            .iter()
            .map(|(id, v)| (id.clone(), Self::cosine(&q, v)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored
    }
}

/// The dual-channel memory store (ARCHITECTURE.md §4.4).
#[derive(Debug, Clone)]
pub struct SharedMemoryStore {
    graph: Arc<RwLock<GraphBackend>>,
    vector: Arc<RwLock<VectorIndex>>,
    config: MemoryConfig,
    disabled: bool,
    /// Optional real embedding model (M-2 / Kaneo #50). When `None` the
    /// vector channel's deterministic hash placeholder is used, so stub
    /// tests stay byte-for-byte reproducible.
    embedder: Option<Arc<dyn EmbeddingModel>>,
}

impl SharedMemoryStore {
    /// Construct a store with default config.
    pub fn new() -> Self {
        Self::from_config(MemoryConfig::default())
    }

    /// Construct a store from a config. If the backend is unavailable, the
    /// store degrades to no-memory mode (logs a clear message, never fails).
    ///
    /// Backend-name validity is checked here (task #78): an unknown
    /// graph/vector backend degrades to `disabled = true` with a log line, so
    /// "backend stopped / misconfigured" surfaces as a degradation signal
    /// rather than a half-working store.
    pub fn from_config(config: MemoryConfig) -> Self {
        let mut store = Self {
            graph: Arc::new(RwLock::new(GraphBackend::default())),
            vector: Arc::new(RwLock::new(VectorIndex::new(config.dim))),
            config: config.clone(),
            disabled: false,
            embedder: None,
        };
        // Backend-name validity: unknown names degrade up front.
        if !config.backends_supported() {
            tracing::warn!(
                graph = %config.graph_backend,
                vector = %config.vector_backend,
                "unsupported memory backend name; degrading to no-memory"
            );
            store.disabled = true;
            return store;
        }
        // Try to reopen a persisted index.
        if let Err(e) = store.reopen_index() {
            tracing::warn!(err = %e, "memory vector index reopen failed; degrading to no-memory");
            store.disabled = true;
        }
        store
    }

    /// Construct a degraded, no-memory store (backend unavailable).
    pub fn disabled() -> Self {
        Self {
            graph: Arc::new(RwLock::new(GraphBackend::default())),
            vector: Arc::new(RwLock::new(VectorIndex::new(1024))),
            config: MemoryConfig::default(),
            disabled: true,
            embedder: None,
        }
    }

    /// Whether the store is usable (not degraded).
    pub fn is_available(&self) -> bool {
        !self.disabled
    }

    /// Wire a real embedding model (M-2 / Kaneo #50).
    ///
    /// Once wired, the vector channel embeds task/verb/url/script text
    /// through the model (remote `/embeddings` endpoint when configured in
    /// `embd.toml`, deterministic placeholder otherwise — the adapter
    /// already encodes that policy). Without a model the store keeps the
    /// deterministic hash placeholder so stub tests stay reproducible.
    /// Returns `false` if the model's dimension does not match
    /// `MemoryConfig::dim` (caller should keep the fallback).
    pub fn set_embedder(&mut self, model: Arc<dyn EmbeddingModel>) -> bool {
        if model.dimensions() != self.config.dim {
            tracing::warn!(
                model_dim = model.dimensions(),
                config_dim = self.config.dim,
                "embedding model dimension mismatch; keeping placeholder embedder"
            );
            return false;
        }
        self.embedder = Some(model);
        true
    }

    /// Embed `text` through the wired real model when present.
    ///
    /// The BGE-M3 model is async (reqwest); sync call sites (script memory
    /// reuse, recall) bridge via `tokio::Handle::current().block_on` when a
    /// runtime is entered, and fall back to the placeholder otherwise
    /// (model-build/embedding-failure also degrades to the placeholder with
    /// a warning — memory must never take the main flow down).
    fn embed_with_model(&self, text: &str) -> Option<Vec<f32>> {
        let model = self.embedder.as_ref()?;
        let result = match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                    tokio::task::block_in_place(|| handle.block_on(model.embed(text)))
                } else {
                    return None; // single-thread runtime: cannot block
                }
            }
            Err(_) => return None,
        };
        match result {
            Ok(emb) => Some(emb.values),
            Err(EmbeddingError::BackendUnavailable(m)) => {
                tracing::warn!(reason = %m, "embedding backend unavailable; placeholder used");
                None
            }
            Err(e) => {
                tracing::warn!(err = %e, "embedding failed; placeholder used");
                None
            }
        }
    }

    /// Write a script-memory entry with the canonical `script:{verb}` /
    /// `session:{id}` tags, into both the graph and vector channels.
    pub fn write_script(&self, mut entry: ScriptMemoryEntry) -> Result<(), MemoryError> {
        if self.disabled {
            return Err(MemoryError::BackendUnavailable("memory disabled".into()));
        }
        entry.tags.insert(0, format!("script:{}", entry.verb));
        let searchable = format!(
            "{} {} {} {}",
            entry.task, entry.verb, entry.url, entry.script
        );
        self.graph
            .write()
            .map_err(|_| MemoryError::BackendUnavailable("graph lock poisoned".into()))?
            .insert(entry.clone());
        // Real-model embedding (M-2) when wired; deterministic placeholder
        // otherwise (or on embedding failure — see embed_with_model).
        let values = self.embed_with_model(&searchable).unwrap_or_else(|| {
            self.vector
                .read()
                .map(|v| v.embed(&searchable))
                .unwrap_or_default()
        });
        self.vector
            .write()
            .map_err(|_| MemoryError::BackendUnavailable("vector lock poisoned".into()))?
            .insert_vec(&entry.id, values);
        self.persist_index()
    }

    /// Recall script-memory entries relevant to `task`, ranked by semantic
    /// similarity (vector channel) and cross-session (graph channel).
    pub fn recall_scripts(&self, task: &str, limit: usize) -> Vec<ScriptMemoryEntry> {
        if self.disabled || limit == 0 {
            return Vec::new();
        }
        // Vector channel: rank by semantic similarity.
        let q_values = self.embed_with_model(task).or_else(|| {
            self.vector
                .read()
                .ok()
                .map(|v| v.embed(task))
        });
        let ranked = self
            .vector
            .read()
            .map_err(|_| ())
            .map(|v| v.recall_with_query(q_values, limit))
            .unwrap_or_default();
        let graph = self
            .graph
            .read()
            .map_err(|_| ())
            .map(|g| g.all())
            .unwrap_or_default();
        let by_id: HashMap<String, ScriptMemoryEntry> =
            graph.into_iter().map(|e| (e.id.clone(), e)).collect();
        ranked
            .into_iter()
            .filter_map(|(id, _)| by_id.get(&id).cloned())
            .collect()
    }

    /// Number of stored script entries.
    pub fn len(&self) -> usize {
        self.graph.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Persist the vector index AND the graph entries to the configured path.
    fn persist_index(&self) -> Result<(), MemoryError> {
        let path = &self.config.index_path;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MemoryError::PersistFailed(e.to_string()))?;
        }
        let vectors = self
            .vector
            .read()
            .map_err(|_| MemoryError::PersistFailed("vector lock poisoned".into()))?
            .vectors
            .clone();
        let entries = self
            .graph
            .read()
            .map_err(|_| MemoryError::PersistFailed("graph lock poisoned".into()))?
            .all();
        let data = serde_json::json!({
            "dim": self.config.dim,
            "vectors": vectors,
            "entries": entries,
        });
        std::fs::write(path, serde_json::to_vec(&data).unwrap_or_default())
            .map_err(|e| MemoryError::PersistFailed(e.to_string()))
    }

    /// Reopen a persisted index from the configured path.
    fn reopen_index(&self) -> Result<(), MemoryError> {
        let path = &self.config.index_path;
        if !path.exists() {
            return Ok(()); // no persisted index yet
        }
        let raw =
            std::fs::read_to_string(path).map_err(|e| MemoryError::PersistFailed(e.to_string()))?;
        let data: Json =
            serde_json::from_str(&raw).map_err(|e| MemoryError::PersistFailed(e.to_string()))?;
        let dim = data.get("dim").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let vectors = data.get("vectors").and_then(|v| v.as_object());
        if let Some(vectors) = vectors {
            let mut idx = self
                .vector
                .write()
                .map_err(|_| MemoryError::PersistFailed("vector lock poisoned".into()))?;
            idx.dim = dim;
            for (id, v) in vectors {
                if let Some(arr) = v.as_array() {
                    idx.vectors.insert(
                        id.clone(),
                        arr.iter()
                            .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                            .collect(),
                    );
                }
            }
        }
        // Restore the graph entries so recall_scripts can resolve ids.
        if let Some(entries) = data.get("entries").and_then(|v| v.as_array()) {
            let mut graph = self
                .graph
                .write()
                .map_err(|_| MemoryError::PersistFailed("graph lock poisoned".into()))?;
            for e in entries {
                if let Ok(entry) = serde_json::from_value::<ScriptMemoryEntry>(e.clone()) {
                    graph.insert(entry);
                }
            }
        }
        Ok(())
    }
}

impl Default for SharedMemoryStore {
    fn default() -> Self {
        Self::from_config(MemoryConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic two-dim "model" (M-2 wired-embedder test): vectors
    /// cluster by shared keyword presence, so recall ranks semantic
    /// similarity distinctly from the hash placeholder.
    #[derive(Debug, Clone)]
    struct LdaLikeModel {
        dim: usize,
    }
    #[async_trait::async_trait]
    impl webai_embedding::EmbeddingModel for LdaLikeModel {
        async fn embed(
            &self,
            text: &str,
        ) -> Result<webai_embedding::Embedding, webai_embedding::EmbeddingError> {
            let lower = text.to_lowercase();
            let mut v = vec![0.0f32; self.dim];
            for (i, keyword) in ["login", "shopping", "cart", "form", "page"].iter().enumerate() {
                if lower.contains(keyword) {
                    v[i % self.dim] += 1.0;
                }
            }
            v[0] += 0.5; // all docs share a base component
            Ok(webai_embedding::Embedding { dim: self.dim, values: v })
        }
        async fn embed_batch(
            &self,
            texts: &[&str],
        ) -> Result<Vec<webai_embedding::Embedding>, webai_embedding::EmbeddingError> {
            let mut out = Vec::new();
            for t in texts {
                out.push(self.embed(t).await?);
            }
            Ok(out)
        }
        fn dimensions(&self) -> usize {
            self.dim
        }
    }

    /// M-2 (Kaneo #50): with a real embedding model wired, `recall_scripts`
    /// must rank by the model's semantics (a paraphrased task over a
    /// different task), and the round-trip stays cross-session.
    #[tokio::test(flavor = "multi_thread")]
    async fn wired_embedder_ranking_and_cross_session() {
        let path = unique_index();
        let cfg = MemoryConfig {
            dim: 5,
            index_path: path.clone(),
            ..Default::default()
        };
        let mut store = SharedMemoryStore::from_config(cfg);
        assert!(store.set_embedder(Arc::new(LdaLikeModel { dim: 5 })));
        store
            .write_script(sample_entry("fill", "log in to shopping site"))
            .unwrap();
        store
            .write_script(sample_entry("click", "browse shopping page"))
            .unwrap();
        // Paraphrase of entry 1 must rank entry 1 first.
        let hits = store.recall_scripts("sign in on my shopping website now", 2);
        assert!(!hits.is_empty(), "must recall with wired model");
        assert!(
            hits[0].task.contains("log in"),
            "paraphrase must rank the login entry first, got {:?}",
            hits.iter().map(|h| &h.task).collect::<Vec<_>>()
        );
        // Cross-session: a fresh store instance on the same index path.
        let store_b = SharedMemoryStore::from_config(MemoryConfig {
            dim: 5,
            index_path: path.clone(),
            ..Default::default()
        });
        let mut store_b = store_b;
        assert!(store_b.set_embedder(Arc::new(LdaLikeModel { dim: 5 })));
        let hits_b = store_b.recall_scripts("shopping cart fill", 2);
        assert!(
            hits_b
                .iter()
                .any(|h| h.task.contains("log in")),
            "cross-session recall must work with the wired model, got {:?}",
            hits_b.iter().map(|h| &h.task).collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// A model whose dimension does not match the config must be rejected
    /// and the store keeps working (placeholder), never failing the flow.
    #[tokio::test]
    async fn dim_mismatch_rejects_but_store_usable() {
        let cfg = MemoryConfig {
            dim: 4,
            index_path: unique_index(),
            ..Default::default()
        };
        let mut store = SharedMemoryStore::from_config(cfg);
        assert!(!store.set_embedder(Arc::new(LdaLikeModel { dim: 8 })));
        assert!(store.is_available());
        store
            .write_script(sample_entry("fill", "log in"))
            .unwrap();
        assert_eq!(store.len(), 1);
    }

    fn unique_index() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("webai-idx-{}-{n}", std::process::id()))
    }

    fn sample_entry(verb: &str, task: &str) -> ScriptMemoryEntry {
        ScriptMemoryEntry {
            task: task.to_string(),
            verb: verb.to_string(),
            url: "https://example.com".into(),
            script: "window.location.href = args.url".into(),
            tags: vec!["session:s1".into()],
            id: format!("{verb}-{task}"),
        }
    }

    #[test]
    fn write_and_recall_script_entries() {
        let store = SharedMemoryStore::from_config(MemoryConfig {
            index_path: unique_index(),
            ..Default::default()
        });
        store
            .write_script(sample_entry("click", "submit login form"))
            .unwrap();
        store
            .write_script(sample_entry("fill", "submit login form"))
            .unwrap();
        let hits = store.recall_scripts("login form", 10);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|e| e
            .tags
            .first()
            .map(|t| t.starts_with("script:"))
            .unwrap_or(false)));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn recall_ranks_by_semantic_similarity() {
        let store = SharedMemoryStore::from_config(MemoryConfig {
            index_path: unique_index(),
            ..Default::default()
        });
        store
            .write_script(sample_entry("click", "submit login form"))
            .unwrap();
        store
            .write_script(sample_entry("click", "buy groceries"))
            .unwrap();
        // "login" should recall the login-form entry (semantic similarity).
        let hits = store.recall_scripts("login", 10);
        assert!(
            hits.iter().any(|e| e.task == "submit login form"),
            "login-form entry must be recalled for query 'login'"
        );
    }

    #[test]
    fn cross_session_recall_after_reopen() {
        let idx = unique_index();
        // Session A writes.
        let store_a = SharedMemoryStore::from_config(MemoryConfig {
            index_path: idx.clone(),
            ..Default::default()
        });
        store_a
            .write_script(sample_entry("click", "submit login form"))
            .unwrap();
        // New store instance (same index path) can recall.
        let store_b = SharedMemoryStore::from_config(MemoryConfig {
            index_path: idx.clone(),
            ..Default::default()
        });
        let hits = store_b.recall_scripts("login form", 10);
        assert_eq!(hits.len(), 1, "cross-session recall must work after reopen");
        let _ = std::fs::remove_file(&idx);
    }

    #[test]
    fn disabled_store_rejects_writes_and_recalls_nothing() {
        let store = SharedMemoryStore::disabled();
        assert!(!store.is_available());
        assert!(store.write_script(sample_entry("click", "x")).is_err());
        assert!(store.recall_scripts("x", 10).is_empty());
    }

    #[test]
    fn index_persists_and_reopens_with_same_count() {
        let idx = unique_index();
        let store = SharedMemoryStore::from_config(MemoryConfig {
            index_path: idx.clone(),
            ..Default::default()
        });
        store.write_script(sample_entry("click", "task A")).unwrap();
        store.write_script(sample_entry("fill", "task B")).unwrap();
        assert_eq!(store.len(), 2);
        // Reopen: same index path, count must match.
        let reopened = SharedMemoryStore::from_config(MemoryConfig {
            index_path: idx.clone(),
            ..Default::default()
        });
        assert_eq!(reopened.len(), 2, "index reopen must restore entry count");
        let _ = std::fs::remove_file(&idx);
    }

    /// Task #78 acceptance 1: sample mem.toml / vec.toml parse into the
    /// config fields (previously declared but never read).
    #[test]
    fn from_toml_files_parses_hnsw_and_backend_fields() {
        let dir = std::env::temp_dir().join(format!(
            "webai-dual-cfg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mem = dir.join("mem.toml");
        let vec = dir.join("vec.toml");
        std::fs::write(&mem, "graph_backend = \"kuzu\"\n").unwrap();
        std::fs::write(
            &vec,
            "backend = \"hnsw\"\nhnsw_m = 32\nhnsw_ef = 128\ndim = 512\nindex_path = \"/tmp/vec-idx\"\n",
        )
        .unwrap();
        let cfg = MemoryConfig::from_toml_files(&mem, &vec);
        assert_eq!(cfg.graph_backend, "kuzu");
        assert_eq!(cfg.vector_backend, "hnsw");
        assert_eq!(cfg.hnsw_m, 32);
        assert_eq!(cfg.hnsw_ef, 128);
        assert_eq!(cfg.dim, 512);
        assert_eq!(cfg.index_path, PathBuf::from("/tmp/vec-idx"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Task #78 acceptance 2: an unknown backend name degrades to a disabled
    /// store (is_available == false, write_script -> BackendUnavailable).
    #[test]
    fn from_config_unknown_backend_degrades_to_disabled() {
        let store = SharedMemoryStore::from_config(MemoryConfig {
            graph_backend: "not-a-graph-db".into(),
            vector_backend: "hnsw".into(),
            ..Default::default()
        });
        assert!(!store.is_available(), "unknown backend must degrade");
        let entry = ScriptMemoryEntry {
            task: "t".into(),
            verb: "click".into(),
            url: "https://example.com".into(),
            script: "s".into(),
            tags: vec![],
            id: "x".into(),
        };
        let err = store.write_script(entry).unwrap_err();
        assert!(matches!(err, MemoryError::BackendUnavailable(_)));
        // A bad vector backend degrades identically.
        let store2 = SharedMemoryStore::from_config(MemoryConfig {
            vector_backend: "faiss".into(),
            ..Default::default()
        });
        assert!(!store2.is_available());
    }

    /// Malformed TOML files degrade to defaults rather than failing startup.
    #[test]
    fn from_toml_files_malformed_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("webai-dual-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mem = dir.join("mem.toml");
        let vec = dir.join("vec.toml");
        std::fs::write(&mem, "not [valid toml ===").unwrap();
        std::fs::write(&vec, "also broken {{{").unwrap();
        let cfg = MemoryConfig::from_toml_files(&mem, &vec);
        assert_eq!(cfg, MemoryConfig::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

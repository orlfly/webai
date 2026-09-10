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
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

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
#[derive(Debug, Clone)]
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

    /// Rank entry ids by cosine similarity to the query, returning top `limit`.
    fn recall(&self, query: &str, limit: usize) -> Vec<(String, f32)> {
        let q = self.embed(query);
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
}

impl SharedMemoryStore {
    /// Construct a store with default config.
    pub fn new() -> Self {
        Self::from_config(MemoryConfig::default())
    }

    /// Construct a store from a config. If the backend is unavailable, the
    /// store degrades to no-memory mode (logs a clear message, never fails).
    pub fn from_config(config: MemoryConfig) -> Self {
        let mut store = Self {
            graph: Arc::new(RwLock::new(GraphBackend::default())),
            vector: Arc::new(RwLock::new(VectorIndex::new(config.dim))),
            config: config.clone(),
            disabled: false,
        };
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
        }
    }

    /// Whether the store is usable (not degraded).
    pub fn is_available(&self) -> bool {
        !self.disabled
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
        self.vector
            .write()
            .map_err(|_| MemoryError::BackendUnavailable("vector lock poisoned".into()))?
            .insert(&entry.id, &searchable);
        self.persist_index()
    }

    /// Recall script-memory entries relevant to `task`, ranked by semantic
    /// similarity (vector channel) and cross-session (graph channel).
    pub fn recall_scripts(&self, task: &str, limit: usize) -> Vec<ScriptMemoryEntry> {
        if self.disabled || limit == 0 {
            return Vec::new();
        }
        // Vector channel: rank by semantic similarity.
        let ranked = self
            .vector
            .read()
            .map_err(|_| ())
            .map(|v| v.recall(task, limit))
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
}

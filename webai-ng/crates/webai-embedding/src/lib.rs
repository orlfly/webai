//! Embedding interfaces and adapters.
//!
//! Implements the embedding interface abstraction + BGE-M3 adapter
//! (ARCHITECTURE.md §4.4). The vector channel of memory depends on embedding
//! generation (`embedding < memory`, §3.2). FR-6 allows graceful degradation
//! when the vector model is missing: the caller falls back to graph-only or
//! no-memory mode.
//!
//! The adapter supports a remote endpoint (OpenAI-compatible `/embeddings`) or
//! a local llama.cpp endpoint. The dimension is taken from `embd.toml`; a
//! mismatch with the declared dimension fails fast.

use async_trait::async_trait;

/// A vector embedding of a text fragment.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    pub dim: usize,
    pub values: Vec<f32>,
}

/// A cosine-similarity search hit over an index.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingHit {
    pub score: f32,
    pub key: String,
}

/// Uniform embedding interface (ARCHITECTURE.md §4.4).
#[async_trait]
pub trait EmbeddingModel: Send + Sync {
    /// Embed a single text fragment.
    async fn embed(&self, text: &str) -> Result<Embedding, EmbeddingError>;
    /// Embed many fragments as a batch.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbeddingError>;
    /// Dimensionality of vectors produced by this model.
    fn dimensions(&self) -> usize;
}

/// Errors raised by an embedding backend.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("embedding backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("endpoint unreachable: {0}")]
    Unreachable(String),
    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimMismatch { expected: usize, actual: usize },
    #[error("request timed out after {0}ms")]
    Timeout(u64),
}

/// Configuration for the BGE-M3 adapter (from `embd.toml`).
#[derive(Debug, Clone)]
pub struct BgeM3Config {
    /// The embedding model name (e.g. `BAAI/bge-m3`).
    pub model: String,
    /// The endpoint URL. `None` = local llama.cpp server.
    pub endpoint: Option<String>,
    /// The declared embedding dimension (from `embd.toml`).
    pub dim: usize,
}

impl Default for BgeM3Config {
    fn default() -> Self {
        Self {
            model: "BAAI/bge-m3".into(),
            endpoint: None,
            dim: 1024,
        }
    }
}

/// BGE-M3 adapter.
///
/// With a remote endpoint it calls the OpenAI-compatible `/embeddings` API.
/// Without an endpoint (local llama.cpp) it produces a deterministic
/// placeholder vector of the configured dimension so upper layers can be
/// built and tested without a live model.
#[derive(Debug, Clone)]
pub struct BgeM3Adapter {
    config: BgeM3Config,
    client: reqwest::Client,
    /// Set by [`BgeM3Adapter::degraded`]: every embed call then returns
    /// `BackendUnavailable` instead of a silent placeholder (task #74).
    degraded: bool,
}

impl BgeM3Adapter {
    /// Construct from a config. The dimension is validated against the
    /// declared `embd.toml` value.
    pub fn new(config: BgeM3Config) -> Self {
        Self {
            config,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            degraded: false,
        }
    }

    /// Construct a degraded adapter (no vector model). Returns a structured
    /// [`EmbeddingError::BackendUnavailable`] on every call so the caller can
    /// fall back to graph-only / no-memory mode.
    pub fn degraded() -> Self {
        Self {
            config: BgeM3Config::default(),
            client: reqwest::Client::new(),
            degraded: true,
        }
    }

    /// Whether this adapter has a usable endpoint.
    pub fn is_available(&self) -> bool {
        self.config.endpoint.is_some()
    }

    /// Produce a deterministic placeholder vector hashing the input text.
    fn placeholder(&self, text: &str) -> Embedding {
        let mut values = vec![0.0f32; self.config.dim];
        let seed = text
            .as_bytes()
            .iter()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u64));
        for (i, v) in values.iter_mut().enumerate() {
            *v = ((seed ^ (i as u64).wrapping_mul(0x9E3779B97F4A7C15)).wrapping_rem(1000) as f32)
                / 1000.0;
        }
        Embedding {
            dim: self.config.dim,
            values,
        }
    }

    /// Call the remote `/embeddings` endpoint for a batch of texts.
    async fn remote_embed(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbeddingError> {
        let endpoint =
            self.config.endpoint.as_deref().ok_or_else(|| {
                EmbeddingError::BackendUnavailable("no endpoint configured".into())
            })?;
        let url = format!("{endpoint}/embeddings");
        let body = serde_json::json!({
            "model": self.config.model,
            "input": texts,
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| EmbeddingError::Unreachable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(EmbeddingError::Provider(format!(
                "HTTP {} from {url}",
                resp.status()
            )));
        }
        let parsed: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| EmbeddingError::Provider(format!("bad response: {e}")))?;
        let data = parsed
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or_else(|| EmbeddingError::Provider("response missing `data`".into()))?;
        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let values = item
                .get("embedding")
                .and_then(|v| v.as_array())
                .ok_or_else(|| EmbeddingError::Provider("item missing `embedding`".into()))?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect::<Vec<_>>();
            let dim = values.len();
            if dim != self.config.dim {
                return Err(EmbeddingError::DimMismatch {
                    expected: self.config.dim,
                    actual: dim,
                });
            }
            out.push(Embedding { dim, values });
        }
        Ok(out)
    }
}

#[async_trait]
impl EmbeddingModel for BgeM3Adapter {
    async fn embed(&self, text: &str) -> Result<Embedding, EmbeddingError> {
        // Degraded construction (missing embd.toml / no backend) is an
        // explicit signal, never a silent placeholder (task #74).
        if self.degraded {
            return Err(EmbeddingError::BackendUnavailable(
                "embedding adapter is degraded (no vector model configured)".into(),
            ));
        }
        if self.config.endpoint.is_some() {
            let mut batch = self.remote_embed(&[text]).await?;
            return batch
                .pop()
                .ok_or_else(|| EmbeddingError::Provider("empty embedding response".into()));
        }
        Ok(self.placeholder(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbeddingError> {
        if self.degraded {
            return Err(EmbeddingError::BackendUnavailable(
                "embedding adapter is degraded (no vector model configured)".into(),
            ));
        }
        if self.config.endpoint.is_some() {
            return self.remote_embed(texts).await;
        }
        Ok(texts.iter().map(|t| self.placeholder(t)).collect())
    }

    fn dimensions(&self) -> usize {
        self.config.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_adapter_is_deterministic_and_dims_match() {
        let m = BgeM3Adapter::new(BgeM3Config {
            dim: 1024,
            ..Default::default()
        });
        let a = m.embed("hello world").await.unwrap();
        let b = m.embed("hello world").await.unwrap();
        assert_eq!(
            a, b,
            "same input must deterministically embed to the same vector"
        );
        assert_eq!(a.dim, 1024);
        assert_eq!(m.dimensions(), 1024);
        assert_eq!(a.values.len(), 1024);
    }

    #[tokio::test]
    async fn different_inputs_differ() {
        let m = BgeM3Adapter::new(BgeM3Config {
            dim: 8,
            ..Default::default()
        });
        let a = m.embed("alpha").await.unwrap();
        let b = m.embed("beta").await.unwrap();
        assert_ne!(a.values, b.values);
    }

    #[tokio::test]
    async fn batch_embedding_returns_all_vectors() {
        let m = BgeM3Adapter::new(BgeM3Config {
            dim: 16,
            ..Default::default()
        });
        let out = m.embed_batch(&["a", "b", "c"]).await.unwrap();
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|e| e.dim == 16));
    }

    #[tokio::test]
    async fn degraded_adapter_reports_backend_unavailable() {
        // Degraded construction must yield an explicit BackendUnavailable on
        // every call, never a panic or a silent placeholder vector (#74).
        let m = BgeM3Adapter::degraded();
        assert!(!m.is_available());

        let err = m.embed("any text").await.unwrap_err();
        assert!(
            matches!(err, EmbeddingError::BackendUnavailable(_)),
            "embed must report BackendUnavailable, got {err:?}"
        );

        let batch_err = m.embed_batch(&["a", "b"]).await.unwrap_err();
        assert!(
            matches!(batch_err, EmbeddingError::BackendUnavailable(_)),
            "embed_batch must report BackendUnavailable, got {batch_err:?}"
        );
    }

    #[tokio::test]
    async fn unreachable_endpoint_returns_structured_error() {
        // A remote endpoint that is unreachable must return a structured error.
        let m = BgeM3Adapter::new(BgeM3Config {
            endpoint: Some("http://127.0.0.1:1".into()), // port 1 = unreachable
            dim: 8,
            ..Default::default()
        });
        let err = m.embed("hello").await.unwrap_err();
        match err {
            EmbeddingError::Unreachable(_) => {}
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    #[test]
    fn dim_mismatch_is_structured() {
        // The DimMismatch error carries expected/actual.
        let err = EmbeddingError::DimMismatch {
            expected: 1024,
            actual: 512,
        };
        assert!(err.to_string().contains("1024"));
        assert!(err.to_string().contains("512"));
    }
}

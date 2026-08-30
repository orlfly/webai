//! Embedding interfaces and adapters.
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). Concrete backend adapters
//! (BGE-M3, HNSW wiring) land in a later milestone; for now this crate defines
//! the public `EmbeddingModel` trait and a stub `BgeM3Adapter` that returns a
//! deterministic placeholder vector.

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
}

/// Stub BGE-M3 adapter. Returns a deterministic placeholder vector of the model
/// dimension so upper layers can be built and tested without a live model.
#[derive(Debug, Clone)]
pub struct BgeM3Adapter {
    dim: usize,
}

impl BgeM3Adapter {
    /// Deterministic placeholder dimension (BGE-M3 native is 1024).
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

#[async_trait]
impl EmbeddingModel for BgeM3Adapter {
    async fn embed(&self, text: &str) -> Result<Embedding, EmbeddingError> {
        Ok(self.placeholder(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbeddingError> {
        Ok(texts.iter().map(|t| self.placeholder(t)).collect())
    }

    fn dimensions(&self) -> usize {
        self.dim
    }
}

impl BgeM3Adapter {
    /// Produce a deterministic placeholder vector hashing the input text length.
    fn placeholder(&self, text: &str) -> Embedding {
        let mut values = vec![0.0f32; self.dim];
        let seed = text.as_bytes().iter().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u64));
        for (i, v) in values.iter_mut().enumerate() {
            *v = ((seed ^ (i as u64).wrapping_mul(0x9E3779B97F4A7C15)).wrapping_rem(1000) as f32) / 1000.0;
        }
        Embedding {
            dim: self.dim,
            values,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_adapter_is_deterministic_and_dims_match() {
        let m = BgeM3Adapter::new(1024);
        let a = m.embed("hello world").await.unwrap();
        let b = m.embed("hello world").await.unwrap();
        assert_eq!(a, b, "same input must deterministically embed to the same vector");
        assert_eq!(a.dim, 1024);
        assert_eq!(b.dim, 1024);
        assert_eq!(m.dimensions(), 1024);
        assert_eq!(a.values.len(), 1024);
    }

    #[tokio::test]
    async fn different_inputs_differ() {
        let m = BgeM3Adapter::new(8);
        let a = m.embed("alpha").await.unwrap();
        let b = m.embed("beta").await.unwrap();
        assert_ne!(a.values, b.values);
    }
}

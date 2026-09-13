//! WebkitViewPool: resource model for WebKit views.
//!
//! Implements ARCHITECTURE.md §6.1: each view is serialized by its own
//! `Mutex` (single-thread affinity), views across sessions run in parallel,
//! and the pool owns view **allocation / reuse / limit** plus **BrowserContext
//! isolation** (different sessions do not share cookies / localStorage).
//!
//! A new view is injected with the document-start bundle in `BUNDLE_SCRIPT_ORDER`
//! (ARCHITECTURE.md §4.6), so `bridge-client.js` installs `window.__webkitBridge`
//! first and the remaining building blocks follow in order.
//!
//! Pool errors are structured (code + human-readable reason); the pool never
//! silently degrades or silently reuses a dirty view.
//!
//! ## Session → view binding
//!
//! The pool maintains a `session_id → PooledView` map (ARCHITECTURE.md §6.1).
//! Each session is bound to exactly one view for its lifetime; `session/close`
//! releases the view back to the pool. This aligns with `AcpSessionRegistry`
//! (§4.10), which holds `Mutex<HashMap<session_id, Arc<AgentSession>>>` shared
//! with the TUI — so a local and a remote observer of the same session see the
//! same view. Isolation is per-session: different sessions never share cookies /
//! localStorage (each view carries its own BrowserContext).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::{bundle_scripts, WebkitBridge, WebkitError};

/// A structured pool error (ARCHITECTURE.md §7).
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("pool exhausted: {0}")]
    Exhausted(String),
    #[error("view creation failed: {0}")]
    CreateFailed(String),
    #[error("bundle injection failed: {0}")]
    InjectFailed(String),
    #[error("view not found for session: {0}")]
    NotFound(String),
}

impl PoolError {
    /// Stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            PoolError::Exhausted(_) => "pool_exhausted",
            PoolError::CreateFailed(_) => "view_create_failed",
            PoolError::InjectFailed(_) => "bundle_inject_failed",
            PoolError::NotFound(_) => "view_not_found",
        }
    }
}

/// A view slot in the pool. Each view is serialized by its own `Mutex`
/// (ARCHITECTURE.md §6.1); the pool hands out `Arc<WebkitBridge>` clones.
#[derive(Clone)]
pub struct PooledView {
    /// The underlying bridge (per-view Mutex serialization).
    pub bridge: Arc<WebkitBridge>,
    /// The session this view is bound to.
    pub session_id: String,
    /// Whether the document-start bundle has been injected.
    bundle_injected: Arc<Mutex<bool>>,
}

impl PooledView {
    /// Whether the bundle has been injected into this view.
    pub fn is_bundle_injected(&self) -> bool {
        *self.bundle_injected.lock().unwrap()
    }
}

impl std::fmt::Debug for PooledView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledView")
            .field("session_id", &self.session_id)
            .field("bundle_injected", &self.is_bundle_injected())
            .finish()
    }
}

/// Configuration for the view pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of live views across all sessions.
    pub max_views: usize,
    /// Whether to inject the document-start bundle on new views.
    pub inject_bundle: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_views: 8,
            inject_bundle: true,
        }
    }
}

/// The WebKit view pool (ARCHITECTURE.md §6.1).
///
/// Owns view allocation / reuse / limit and BrowserContext isolation. Each
/// session maps to a dedicated view (session → view binding), so different
/// sessions never share cookies / localStorage.
#[derive(Clone)]
pub struct WebkitViewPool {
    config: PoolConfig,
    /// session_id → view binding.
    views: Arc<Mutex<HashMap<String, PooledView>>>,
    /// A factory for creating new views (injectable for tests).
    factory: Arc<dyn Fn() -> WebkitBridge + Send + Sync>,
    /// Serialises the check-create-inject-insert critical section so two
    /// concurrent acquires of the same (or a boundary) session cannot race
    /// (task #77: TOCTOU leaked views / exceeded max_views).
    create_lock: Arc<tokio::sync::Mutex<()>>,
}

impl WebkitViewPool {
    /// Construct a pool with the default view factory (real FFI).
    pub fn new(config: PoolConfig) -> Self {
        Self {
            config,
            views: Arc::new(Mutex::new(HashMap::new())),
            factory: Arc::new(WebkitBridge::new),
            create_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Construct a pool with a custom view factory (for tests).
    pub fn with_factory<F>(config: PoolConfig, factory: F) -> Self
    where
        F: Fn() -> WebkitBridge + Send + Sync + 'static,
    {
        Self {
            config,
            views: Arc::new(Mutex::new(HashMap::new())),
            factory: Arc::new(factory),
            create_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Acquire (or create) the view bound to `session_id`.
    ///
    /// If the session already has a view, it is reused. Otherwise a new view is
    /// created (respecting the pool limit) and injected with the document-start
    /// bundle in `BUNDLE_SCRIPT_ORDER`.
    ///
    /// The create path runs inside an exclusive `create_lock` critical section
    /// (double-checked lookup → limit check → create → inject → insert), so
    /// concurrent acquires can neither duplicate a session's view nor exceed
    /// `max_views` (task #77 TOCTOU).
    pub async fn acquire(&self, session_id: &str) -> Result<PooledView, PoolError> {
        // Fast path: reuse an existing view (no lock held across await).
        if let Some(view) = self.views.lock().unwrap().get(session_id) {
            return Ok(view.clone());
        }
        // Exclusive creation critical section: hold across the await so the
        // double-check / limit / insert sequence is atomic w.r.t. other
        // creators. Holders of existing views are unaffected.
        let _guard = self.create_lock.lock().await;
        // Double-check: another task may have created this session's view
        // while we waited for the lock.
        if let Some(view) = self.views.lock().unwrap().get(session_id) {
            return Ok(view.clone());
        }
        if self.views.lock().unwrap().len() >= self.config.max_views {
            return Err(PoolError::Exhausted(format!(
                "max_views={} reached; release a view or raise the limit",
                self.config.max_views
            )));
        }
        let bridge = Arc::new((self.factory)());
        let view = PooledView {
            bridge: bridge.clone(),
            session_id: session_id.to_owned(),
            bundle_injected: Arc::new(Mutex::new(false)),
        };
        if self.config.inject_bundle {
            inject_bundle(&bridge)
                .await
                .map_err(|e| PoolError::InjectFailed(format!("session {session_id}: {e}")))?;
            *view.bundle_injected.lock().unwrap() = true;
        }
        self.views
            .lock()
            .unwrap()
            .insert(session_id.to_owned(), view.clone());
        Ok(view)
    }

    /// Release the view bound to `session_id`, removing it from the pool.
    pub fn release(&self, session_id: &str) -> Result<(), PoolError> {
        let mut views = self.views.lock().unwrap();
        views
            .remove(session_id)
            .map(|_| ())
            .ok_or_else(|| PoolError::NotFound(session_id.to_owned()))
    }

    /// Look up the view bound to `session_id` without creating one.
    pub fn get(&self, session_id: &str) -> Option<PooledView> {
        self.views.lock().unwrap().get(session_id).cloned()
    }

    /// Number of live views.
    pub fn len(&self) -> usize {
        self.views.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The configured max view count.
    pub fn max_views(&self) -> usize {
        self.config.max_views
    }
}

/// Inject the document-start bundle into a view in `BUNDLE_SCRIPT_ORDER`.
///
/// Each script is injected as a document-start user script. `bridge-client.js`
/// must be first (installs `window.__webkitBridge`); the rest follow in order.
async fn inject_bundle(bridge: &WebkitBridge) -> Result<(), WebkitError> {
    for (path, src) in bundle_scripts() {
        // Inject as a document-start user script. In no-FFI mode this returns
        // CogLaunch; we surface it so the pool does not silently proceed.
        bridge.inject_user_script(src).await?;
        tracing::debug!(path, "injected document-start bundle script");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CannedBackend;

    /// A canned bridge factory for tests (no FFI).
    fn canned_factory() -> WebkitBridge {
        WebkitBridge::with_canned(CannedBackend::default())
    }

    #[tokio::test]
    async fn acquire_creates_and_reuses_view_per_session() {
        let pool = WebkitViewPool::with_factory(PoolConfig::default(), canned_factory);
        let v1 = pool.acquire("s1").await.unwrap();
        let v2 = pool.acquire("s1").await.unwrap();
        // Same session → same view (reuse).
        assert!(Arc::ptr_eq(&v1.bridge, &v2.bridge));
        assert_eq!(pool.len(), 1);
        // Different session → different view.
        let v3 = pool.acquire("s2").await.unwrap();
        assert!(!Arc::ptr_eq(&v1.bridge, &v3.bridge));
        assert_eq!(pool.len(), 2);
    }

    #[tokio::test]
    async fn pool_limit_returns_structured_error() {
        let pool = WebkitViewPool::with_factory(
            PoolConfig {
                max_views: 2,
                ..Default::default()
            },
            canned_factory,
        );
        pool.acquire("s1").await.unwrap();
        pool.acquire("s2").await.unwrap();
        let err = pool.acquire("s3").await.unwrap_err();
        assert_eq!(err.code(), "pool_exhausted");
        match err {
            PoolError::Exhausted(msg) => {
                assert!(msg.contains("max_views=2"));
            }
            other => panic!("expected Exhausted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn release_removes_view_and_frees_slot() {
        let pool = WebkitViewPool::with_factory(
            PoolConfig {
                max_views: 1,
                ..Default::default()
            },
            canned_factory,
        );
        pool.acquire("s1").await.unwrap();
        assert_eq!(pool.len(), 1);
        pool.release("s1").unwrap();
        assert_eq!(pool.len(), 0);
        // Slot freed → can acquire again.
        pool.acquire("s1").await.unwrap();
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn release_unknown_session_returns_not_found() {
        let pool = WebkitViewPool::with_factory(PoolConfig::default(), canned_factory);
        let err = pool.release("nope").unwrap_err();
        assert!(matches!(err, PoolError::NotFound(_)));
        assert_eq!(err.code(), "view_not_found");
    }

    #[tokio::test]
    async fn bundle_is_injected_on_new_view() {
        let pool = WebkitViewPool::with_factory(PoolConfig::default(), canned_factory);
        let v = pool.acquire("s1").await.unwrap();
        assert!(
            v.is_bundle_injected(),
            "bundle must be injected on new view"
        );
    }

    #[test]
    fn get_returns_none_for_unknown_session() {
        let pool = WebkitViewPool::with_factory(PoolConfig::default(), canned_factory);
        assert!(pool.get("nope").is_none());
    }

    #[tokio::test]
    async fn same_view_serializes_concurrent_evaluates() {
        // Two concurrent evaluates on the same view must be serialized (no
        // interleaving). We use a canned bridge that returns a scripted result;
        // the pool hands out the same Arc for the same session.
        let pool = WebkitViewPool::with_factory(PoolConfig::default(), || {
            WebkitBridge::with_canned(CannedBackend {
                evaluate_result: Some(serde_json::json!(42)),
                ..Default::default()
            })
        });
        let v = pool.acquire("s1").await.unwrap();
        let v2 = v.clone();
        let h1 =
            tokio::spawn(
                async move { v.bridge.evaluate_javascript("1", 1000, None).await.unwrap().json },
            );
        let h2 =
            tokio::spawn(
                async move { v2.bridge.evaluate_javascript("2", 1000, None).await.unwrap().json },
            );
        let (r1, r2) = tokio::join!(h1, h2);
        // Both complete without panic; the per-view Mutex serializes access.
        assert_eq!(r1.unwrap(), serde_json::json!(42));
        assert_eq!(r2.unwrap(), serde_json::json!(42));
    }

    #[tokio::test]
    async fn cross_view_parallel_acquire() {
        // Different sessions get different views; acquiring them in parallel
        // must not collide.
        let pool = WebkitViewPool::with_factory(PoolConfig::default(), canned_factory);
        let p1 = pool.clone();
        let p2 = pool.clone();
        let h1 = tokio::spawn(async move { p1.acquire("s1").await.unwrap() });
        let h2 = tokio::spawn(async move { p2.acquire("s2").await.unwrap() });
        let (v1, v2) = tokio::join!(h1, h2);
        assert!(!Arc::ptr_eq(&v1.unwrap().bridge, &v2.unwrap().bridge));
        assert_eq!(pool.len(), 2);
    }

    /// Task #77 acceptance 1: N tasks racing to acquire the SAME new session
    /// must create exactly one view (no leak from an overwritten insert).
    #[tokio::test]
    async fn concurrent_acquire_same_session_creates_exactly_one_view() {
        let pool = Arc::new(WebkitViewPool::with_factory(
            PoolConfig::default(),
            canned_factory,
        ));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let p = Arc::clone(&pool);
            handles.push(tokio::spawn(
                async move { p.acquire("race").await.unwrap() },
            ));
        }
        let mut bridges = Vec::new();
        for h in handles {
            bridges.push(h.await.unwrap().bridge);
        }
        // Every caller observes the same underlying view.
        assert!(bridges.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])));
        // Exactly one view exists — no leaked duplicate.
        assert_eq!(pool.len(), 1);
    }

    /// Task #77 acceptance 2: distinct sessions racing at the max_views
    /// boundary must never exceed the hard cap (structured Exhausted instead).
    #[tokio::test]
    async fn concurrent_acquire_at_limit_never_exceeds_max_views() {
        const MAX: usize = 4;
        let pool = Arc::new(WebkitViewPool::with_factory(
            PoolConfig {
                max_views: MAX,
                ..Default::default()
            },
            canned_factory,
        ));
        // 12 racers over distinct session ids with only 4 slots: some must get
        // a view, the rest a structured Exhausted — never exceeding MAX.
        let mut handles = Vec::new();
        for i in 0..12 {
            let p = Arc::clone(&pool);
            handles.push(tokio::spawn(
                async move { p.acquire(&format!("s{i}")).await },
            ));
        }
        let mut ok = 0;
        let mut exhausted = 0;
        for h in handles {
            match h.await.unwrap() {
                Ok(_) => ok += 1,
                Err(PoolError::Exhausted(_)) => exhausted += 1,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        assert_eq!(ok + exhausted, 12);
        assert_eq!(ok, MAX, "exactly max_views slots are handed out");
        assert_eq!(exhausted, 12 - MAX);
        assert!(pool.len() <= MAX, "pool must never exceed max_views");
    }
}

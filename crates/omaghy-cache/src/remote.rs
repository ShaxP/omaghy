//! The network half of the `Store`, inverted.
//!
//! # Why this trait exists
//!
//! `spec/20-store.md` §8 places `SqliteStore` in this crate and says it is
//! "tested against recorded HTTP fixtures" — which would mean `omaghy-cache`
//! makes HTTP requests. It cannot: `CONTRIBUTING.md`'s crate graph has
//! `omaghy-api` depending on `omaghy-cache`, not the reverse, and dependencies
//! point strictly downward.
//!
//! So the dependency is inverted. `SqliteStore` owns the cache, the freshness
//! policy, the events and the optimistic half of every mutation; it delegates
//! anything that touches a socket to a [`Remote`] it is handed. `omaghy-sync`
//! implements that with `omaghy-api` behind it at M1 integration. The spec has
//! been corrected to say so.
//!
//! The immediate payoff is that **no test in this crate can open a socket**:
//! there is nothing here that knows how.

use async_trait::async_trait;
use omaghy_model::{NotificationId, Result};
use omaghy_store::RefreshTarget;
use std::sync::Mutex;

/// Everything the `Store` needs that is not the cache.
///
/// Scheduling is fire-and-forget: `schedule` returns immediately and the work
/// reports back by the store's `refresh_finished` / `refresh_failed`. The
/// mutation methods *are* awaited, because their result decides whether the
/// optimistic local write stands or is rolled back (§5).
#[async_trait]
pub trait Remote: std::fmt::Debug + Send + Sync + 'static {
    /// Begin fetching `target`. Coalescing is the implementation's business:
    /// the store has already checked whether it thinks one is in flight.
    fn schedule(&self, target: RefreshTarget);

    /// Abandon work for a surface the user has left.
    fn cancel(&self, target: &RefreshTarget);

    /// Tell GitHub about a local mark-read. Must be idempotent.
    async fn mark_read(&self, ids: &[NotificationId]) -> Result<()>;

    async fn mark_unread(&self, ids: &[NotificationId]) -> Result<()>;
}

/// The default [`Remote`]: records what was asked for and touches nothing.
///
/// This is what ships until M1 integration wires the real client. Reads,
/// freshness, viewer keying and optimistic mutation are all exercised against
/// it; only the fetch is missing. `scheduled()` is how a test — or `omaghy
/// doctor` — sees what a surface asked for.
#[derive(Debug, Default)]
pub struct RecordIntent {
    scheduled: Mutex<Vec<RefreshTarget>>,
}

impl RecordIntent {
    pub fn new() -> Self {
        Self::default()
    }

    /// Targets requested and not yet cancelled, in order.
    pub fn scheduled(&self) -> Vec<RefreshTarget> {
        self.scheduled.lock().expect("not poisoned").clone()
    }
}

#[async_trait]
impl Remote for RecordIntent {
    fn schedule(&self, target: RefreshTarget) {
        self.scheduled.lock().expect("not poisoned").push(target);
    }

    fn cancel(&self, target: &RefreshTarget) {
        self.scheduled
            .lock()
            .expect("not poisoned")
            .retain(|t| t != target);
    }

    async fn mark_read(&self, _ids: &[NotificationId]) -> Result<()> {
        // The local write already happened and is the user-visible effect.
        // Reporting success here means it stands, which is the correct
        // behaviour for a cache with no client behind it yet.
        Ok(())
    }

    async fn mark_unread(&self, _ids: &[NotificationId]) -> Result<()> {
        Ok(())
    }
}

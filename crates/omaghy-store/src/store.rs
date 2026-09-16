//! The trait. The only seam `omaghy-tui` sees.
//!
//! Two rules, from `spec/20-store.md` §1:
//!
//! **Reads never touch the network.** They answer from cache immediately —
//! possibly stale, possibly empty. A read that blocks on a socket is a bug.
//!
//! **Refresh is fire-and-forget.** There is deliberately no "await the fresh
//! data" call; offering one guarantees somebody awaits it in a draw path.

use crate::{
    event::{RefreshTarget, StoreEvent},
    fresh::Fresh,
    query::{DashboardConfig, NotificationQuery, Page, PrQuery},
};
use async_trait::async_trait;
use omaghy_model::{Notification, NotificationId, PrDetail, PullRequest, Result, SubjectRef};
use tokio::sync::broadcast;

/// Who we are acting as. Every cache row is keyed by this: `i_am_requested`,
/// `my_review` and `unread` all answer "does this need *me*".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub login: String,
}

impl Viewer {
    pub fn new(login: impl Into<String>) -> Self {
        Self {
            login: login.into(),
        }
    }
}

/// One dashboard section, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardSectionData {
    pub title: String,
    pub query: String,
    /// Rendered rows. Typed per-surface in M2; notifications-shaped for now.
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dashboard {
    pub sections: Vec<DashboardSectionData>,
}

/// The M1 surface of the store, plus M2's pull requests.
///
/// Issues, actions, repositories and search join this trait in M2–M4. They
/// are deliberately absent rather than stubbed: a method nothing implements
/// is a contract nobody has checked.
#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// Change notification. Events name what changed and carry no data.
    fn subscribe(&self) -> broadcast::Receiver<StoreEvent>;

    fn viewer(&self) -> &Viewer;

    // ---- reads: cache only, never block on the network ------------------

    async fn dashboard(&self, cfg: &DashboardConfig) -> Result<Fresh<Dashboard>>;

    async fn notifications(&self, q: &NotificationQuery) -> Result<Fresh<Page<Notification>>>;

    /// The rows for a query. Empty and never fetched on a cold cache, which
    /// `Fresh` says; refresh with `RefreshTarget::PullRequests(q)`.
    async fn pull_requests(&self, q: &PrQuery) -> Result<Fresh<Page<PullRequest>>>;

    /// One pull request, opened. `None` means the cache has never held it —
    /// **not** that it does not exist. A detail that has been fetched and
    /// found missing is a `RefreshFailed` with `StoreError::NotFound`, and
    /// the surface hears about it by subscribing, as it does for every other
    /// fetch outcome.
    async fn pull_request(&self, r: &SubjectRef) -> Result<Fresh<Option<PrDetail>>>;

    // ---- refresh: schedules and returns ---------------------------------

    /// Schedule work. Results arrive as [`StoreEvent::Updated`].
    fn refresh(&self, target: RefreshTarget);

    /// Cancel work for a surface the user has left.
    fn cancel(&self, target: &RefreshTarget);

    // ---- mutations: optimistic, with rollback ---------------------------

    /// Idempotent: marking an already-read thread must not error.
    async fn mark_read(&self, ids: &[NotificationId]) -> Result<()>;

    /// **Local only — GitHub has no counterpart.** Verified by W2.1 against
    /// the live API: `PATCH` with `{"unread": true}` answers 205 and leaves
    /// the thread read.
    ///
    /// The method stays because undoing a mis-pressed `Enter` is worth having,
    /// and unread is the viewer's own triage state. The consequence must be
    /// surfaced rather than hidden: a thread marked unread here **comes back
    /// read** on the next fetch that touches it. An implementation must not
    /// pretend otherwise by suppressing the incoming value.
    async fn mark_unread(&self, ids: &[NotificationId]) -> Result<()>;
}

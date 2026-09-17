//! `SqliteStore` — the real `Store`, reading from SQLite.
//!
//! Two rules from `spec/20-store.md` §1 shape every method below:
//!
//! **Reads never touch the network.** Nothing in this file can: the only path
//! to a socket is [`Remote`], and every read method ignores it. A read answers
//! from SQLite immediately — possibly stale, possibly empty — and says so in
//! the [`Fresh`] wrapper rather than lying.
//!
//! **Refresh is fire-and-forget.** [`SqliteStore::refresh`] records intent,
//! emits `RefreshStarted`, and returns. Data landing is reported back in by
//! [`SqliteStore::refresh_finished`], which emits `Updated`; the UI re-queries.
//! Nothing awaits a fetch, because offering that guarantees somebody awaits it
//! in a draw path.

use crate::{
    cache::{Cache, NOTIFICATIONS_LIST, dashboard_list_key, pr_detail_key},
    remote::{RecordIntent, Remote},
    ttl,
};
use async_trait::async_trait;
use omaghy_model::{
    Notification, NotificationId, PrDetail, PullRequest, Result, StoreError, SubjectRef,
};
use omaghy_store::{
    Dashboard, DashboardSectionData, Fresh, RefreshTarget, Store, StoreEvent, Viewer,
    query::{DashboardConfig, NotificationQuery, Page, PrQuery},
};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use time::{Duration, OffsetDateTime};
use tokio::sync::broadcast;

/// Where "now" comes from.
///
/// Freshness is a comparison against the clock, so a test that cannot fix the
/// clock cannot assert on staleness without sleeping. `Fixed` is not a
/// production mode — it is what makes §4's table testable.
#[derive(Debug, Clone, Copy)]
pub enum Clock {
    System,
    Fixed(OffsetDateTime),
}

impl Clock {
    pub fn now(self) -> OffsetDateTime {
        match self {
            Self::System => OffsetDateTime::now_utc(),
            Self::Fixed(t) => t,
        }
    }
}

/// Enough room that a surface which stops reading for a frame does not lose
/// events, and small enough that a stalled subscriber cannot grow unbounded.
const EVENT_CAPACITY: usize = 64;

pub struct SqliteStore {
    viewer: Viewer,
    /// `rusqlite::Connection` is `Send` but not `Sync`, and `Store` requires
    /// `Sync`. The lock is never held across an `.await`, so the futures stay
    /// `Send` and a slow mutation cannot block a read.
    cache: Mutex<Cache>,
    events: broadcast::Sender<StoreEvent>,
    remote: Arc<dyn Remote>,
    in_flight: Mutex<HashSet<RefreshTarget>>,
    clock: Clock,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore")
            .field("viewer", &self.viewer)
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl SqliteStore {
    /// Open the cache at `path` for `viewer`.
    ///
    /// A schema-version mismatch rebuilds silently — it is a cache. A file
    /// that is not a database rebuilds *and* returns
    /// `StoreError::Cache(CacheError::Corrupt)`, the one error for which
    /// `keeps_cached_content()` is false.
    pub fn open(path: impl AsRef<Path>, viewer: Viewer) -> Result<Self> {
        let cache = Cache::open(path, &viewer.login)?;
        Ok(Self::around(cache, viewer))
    }

    /// A store over an in-memory database. Nothing survives the process.
    pub fn in_memory(viewer: Viewer) -> Result<Self> {
        let cache = Cache::in_memory(&viewer.login)?;
        Ok(Self::around(cache, viewer))
    }

    fn around(cache: Cache, viewer: Viewer) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            viewer,
            cache: Mutex::new(cache),
            events,
            remote: Arc::new(RecordIntent::new()),
            in_flight: Mutex::new(HashSet::new()),
            clock: Clock::System,
        }
    }

    /// Install the network half. Until M1 integration this is
    /// [`RecordIntent`], which records what was asked for and fetches nothing.
    #[must_use]
    pub fn with_remote(mut self, remote: Arc<dyn Remote>) -> Self {
        self.remote = remote;
        self
    }

    /// Fix the clock, so freshness can be asserted without sleeping.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn now(&self) -> OffsetDateTime {
        self.clock.now()
    }

    /// Borrow the cache for a unit of work.
    ///
    /// This is the seam the fetcher writes through: `omaghy-api` translates a
    /// response into `omaghy-model` types and hands them here, and only then
    /// does anyone call [`Self::refresh_finished`]. Deliberately a closure
    /// rather than a `&Cache` getter, so the lock cannot be held across an
    /// await by accident.
    pub fn with_cache<R>(
        &self,
        f: impl FnOnce(&Cache) -> std::result::Result<R, omaghy_model::CacheError>,
    ) -> Result<R> {
        let cache = self.lock();
        Ok(f(&cache)?)
    }

    /// Report that a scheduled refresh landed. Emits `Updated`; the UI
    /// re-queries and the event carries no data, so there is one path into
    /// UI state rather than two.
    pub fn refresh_finished(&self, target: RefreshTarget) {
        self.in_flight.lock().expect("not poisoned").remove(&target);
        self.announce(StoreEvent::Updated(target));
    }

    /// Report that a scheduled refresh failed. The cache keeps whatever it
    /// held; `StoreError::keeps_cached_content()` decides whether the UI does.
    pub fn refresh_failed(&self, target: RefreshTarget, error: StoreError) {
        self.in_flight.lock().expect("not poisoned").remove(&target);
        self.announce(StoreEvent::RefreshFailed { target, error });
    }

    /// Targets currently believed to be fetching. Drives `Fresh::refreshing`,
    /// which is how a surface says "showing cached, refreshing" instead of
    /// replacing content with a spinner.
    pub fn in_flight(&self) -> Vec<RefreshTarget> {
        self.in_flight
            .lock()
            .expect("not poisoned")
            .iter()
            .cloned()
            .collect()
    }

    fn is_refreshing(&self, target: &RefreshTarget) -> bool {
        self.in_flight
            .lock()
            .expect("not poisoned")
            .contains(target)
    }

    fn announce(&self, ev: StoreEvent) {
        // No subscribers is the normal state before the TUI starts.
        let _ = self.events.send(ev);
    }

    fn lock(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().expect("not poisoned")
    }

    /// Wrap a value with its provenance, given when it was fetched.
    ///
    /// `None` is the cold case and becomes `Fresh::never` — never fetched is
    /// distinct from fetched-and-empty, and the two produce different screens.
    fn wrap<T>(
        &self,
        value: T,
        fetched_at: Option<OffsetDateTime>,
        ttl: Duration,
        target: &RefreshTarget,
    ) -> Fresh<T> {
        let refreshing = self.is_refreshing(target);
        match fetched_at {
            None => Fresh::never(value).refreshing(refreshing),
            Some(at) => Fresh::from_cache(value, at, ttl, self.now()).refreshing(refreshing),
        }
    }
}

#[async_trait]
impl Store for SqliteStore {
    fn subscribe(&self) -> broadcast::Receiver<StoreEvent> {
        self.events.subscribe()
    }

    fn viewer(&self) -> &Viewer {
        &self.viewer
    }

    async fn dashboard(&self, cfg: &DashboardConfig) -> Result<Fresh<Dashboard>> {
        let (sections, oldest, any_missing) = {
            let cache = self.lock();
            let mut sections = Vec::with_capacity(cfg.sections.len());
            let mut oldest: Option<OffsetDateTime> = None;
            let mut any_missing = false;

            for section in &cfg.sections {
                let key = dashboard_list_key(&section.query);
                let count = match cache.list_meta(&key)? {
                    Some(meta) => {
                        oldest = Some(match oldest {
                            Some(o) if o < meta.fetched_at => o,
                            _ => meta.fetched_at,
                        });
                        // How many *match*, not how many we hold. A section
                        // fetches at most `limit` ids and renders a count, so
                        // counting the stored ids reported the limit — a queue
                        // of forty read as "10". `total` is absent only for a
                        // list stored before it was fetched with one, where
                        // the ids are the whole answer.
                        meta.total.unwrap_or(cache.list(&key)?.len() as u32)
                    }
                    None => {
                        any_missing = true;
                        0
                    }
                };
                sections.push(DashboardSectionData {
                    title: section.title.clone(),
                    query: section.query.clone(),
                    count,
                });
            }
            (sections, oldest, any_missing)
        };

        let mut fresh = self.wrap(
            Dashboard { sections },
            oldest,
            ttl::DASHBOARD,
            &RefreshTarget::Dashboard,
        );
        // A dashboard is only as fresh as its stalest section. One section
        // never fetched makes the whole screen stale, because the count it
        // shows is zero and that is a claim, not an absence.
        if any_missing {
            fresh.stale = true;
        }
        Ok(fresh)
    }

    async fn notifications(&self, q: &NotificationQuery) -> Result<Fresh<Page<Notification>>> {
        let (mut items, meta, poll) = {
            let cache = self.lock();
            (
                cache.notifications(q)?,
                cache.list_meta(NOTIFICATIONS_LIST)?,
                cache.poll_interval()?,
            )
        };

        // `total` counts what matched the filter, before the view's limit —
        // "12 of 340 unread" is a different sentence from "12 unread".
        let total = items.len() as u32;
        if let Some(limit) = q.limit {
            items.truncate(limit);
        }

        let page = Page {
            items,
            cursor: meta.as_ref().and_then(|m| m.cursor.clone()),
            total: Some(total),
        };
        Ok(self.wrap(
            page,
            meta.map(|m| m.fetched_at),
            ttl::notifications(poll),
            &RefreshTarget::Notifications,
        ))
    }

    async fn pull_requests(&self, q: &PrQuery) -> Result<Fresh<Page<PullRequest>>> {
        let key = q.cache_key();
        let (items, meta) = {
            let cache = self.lock();
            let meta = cache.list_meta(&key)?;
            let mut items = Vec::new();
            for id in cache.list(&key)? {
                // Membership and bodies are stored apart (§3), so a list can
                // name a row whose body is gone — a rebuilt cache, or a sweep
                // by kind. Skipping it is honest: the list is what was
                // fetched, and a row that cannot be shown is not shown.
                if let Some(stored) = cache.entity::<PullRequest>(&id)? {
                    items.push(stored.value);
                }
            }
            (items, meta)
        };

        let page = Page {
            items,
            cursor: meta.as_ref().and_then(|m| m.cursor.clone()),
            total: meta.as_ref().and_then(|m| m.total),
        };
        Ok(self.wrap(
            page,
            meta.map(|m| m.fetched_at),
            ttl::LIST,
            &RefreshTarget::PullRequests(q.clone()),
        ))
    }

    async fn pull_request(&self, r: &SubjectRef) -> Result<Fresh<Option<PrDetail>>> {
        let stored = {
            let cache = self.lock();
            cache.entity::<PrDetail>(&pr_detail_key(r))?
        };
        let (value, fetched_at) = match stored {
            Some(s) => (Some(s.value), Some(s.fetched_at)),
            None => (None, None),
        };
        Ok(self.wrap(
            value,
            fetched_at,
            ttl::DETAIL,
            &RefreshTarget::PullRequest(r.clone()),
        ))
    }

    fn refresh(&self, target: RefreshTarget) {
        // Coalesce: the same target requested twice while in flight is one
        // request with two waiters (§6). The second call is a no-op rather
        // than a second `RefreshStarted`, so the UI's spinner does not
        // flicker.
        let fresh = self
            .in_flight
            .lock()
            .expect("not poisoned")
            .insert(target.clone());
        if !fresh {
            return;
        }
        self.announce(StoreEvent::RefreshStarted(target.clone()));
        self.remote.schedule(target);
    }

    fn cancel(&self, target: &RefreshTarget) {
        self.in_flight.lock().expect("not poisoned").remove(target);
        self.remote.cancel(target);
    }

    async fn mark_read(&self, ids: &[NotificationId]) -> Result<()> {
        self.set_read_state(ids, false).await
    }

    async fn mark_unread(&self, ids: &[NotificationId]) -> Result<()> {
        self.set_read_state(ids, true).await
    }
}

impl SqliteStore {
    /// The optimistic mutation of `spec/20-store.md` §5, both halves.
    ///
    /// 1. Capture the prior value — **before** writing, or there is nothing to
    ///    roll back to.
    /// 2. Write locally and emit `Updated`, so the keypress lands in one frame
    ///    instead of after a 700ms round trip.
    /// 3. Fire the request.
    /// 4. On failure, restore exactly what was there and emit `Updated` again.
    ///
    /// Rollback restores per-id state rather than setting them all back to one
    /// value: a bulk mark-read over a mixed selection would otherwise come
    /// back with every row unread, including the ones that already were read.
    async fn set_read_state(&self, ids: &[NotificationId], unread: bool) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let prior = {
            let cache = self.lock();
            let prior = cache.unread_flags(ids)?;
            cache.set_unread(ids, unread)?;
            prior
        };
        self.announce(StoreEvent::Updated(RefreshTarget::Notifications));

        let sent = if unread {
            self.remote.mark_unread(ids).await
        } else {
            self.remote.mark_read(ids).await
        };

        if let Err(e) = sent {
            {
                let cache = self.lock();
                cache.restore_unread(&prior)?;
            }
            self.announce(StoreEvent::Updated(RefreshTarget::Notifications));
            return Err(e);
        }
        Ok(())
    }
}

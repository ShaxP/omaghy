//! Where the cache meets the network.
//!
//! `omaghy-cache` deliberately cannot reach a socket — `SqliteStore` owns the
//! cache, freshness, events and the optimistic half of every mutation, and
//! delegates anything touching GitHub to the [`Remote`] trait it is handed
//! (`spec/20-store.md` §8). This crate is that implementation, and the only
//! place a fetched response becomes a cache write.
//!
//! The store holds the remote and the remote must write back into the store,
//! which is a cycle. It is broken with a [`Weak`], set by [`Syncer::attach`]
//! once both exist.

pub mod poll;

pub use poll::PollConfig;

use async_trait::async_trait;
use omaghy_api::{Conditional, GitHubClient, NotificationFilter, Notifications};
use omaghy_cache::{Cache, ListMeta, Remote, SqliteStore, dashboard_list_key};
use omaghy_model::{NotificationId, Result, Validators};
use omaghy_store::{DashboardConfig, NotificationQuery, RefreshTarget, Store};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::task::JoinHandle;

/// Notifications have their own table, but their freshness lives with every
/// other list's, under this key (`spec/20-store.md` §3).
const INBOX: &str = "notifications";

/// The pieces a background fetch needs. Cloned into the spawned task, so
/// `schedule` never needs an owned `Arc<Syncer>`.
#[derive(Clone)]
struct Job {
    client: Arc<GitHubClient>,
    store: Arc<SqliteStore>,
    running: Arc<Mutex<HashMap<RefreshTarget, JoinHandle<()>>>>,
    /// The sections to count. `RefreshTarget::Dashboard` names no queries —
    /// it cannot, since it is a key that has to hash and compare — so the
    /// syncer is told once at startup, which is also when config is read
    /// (`spec/40-config.md` §1).
    dashboard: Arc<DashboardConfig>,
    failures: Arc<Mutex<HashMap<RefreshTarget, u32>>>,
}

pub struct Syncer {
    client: Arc<GitHubClient>,
    store: Mutex<Weak<SqliteStore>>,
    running: Arc<Mutex<HashMap<RefreshTarget, JoinHandle<()>>>>,
    dashboard: Arc<DashboardConfig>,
    /// Consecutive failures per target, for the poll loop's back-off. Reset by
    /// the first success, so a network that comes back is noticed at the
    /// configured interval rather than at the backed-off one.
    failures: Arc<Mutex<HashMap<RefreshTarget, u32>>>,
}

impl std::fmt::Debug for Syncer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.running.lock().map(|r| r.len()).unwrap_or(0);
        f.debug_struct("Syncer")
            .field("in_flight", &n)
            .finish_non_exhaustive()
    }
}

impl Syncer {
    pub fn new(client: Arc<GitHubClient>) -> Arc<Self> {
        Self::with_dashboard(client, DashboardConfig::default())
    }

    /// The syncer, told which dashboard sections to count.
    pub fn with_dashboard(client: Arc<GitHubClient>, dashboard: DashboardConfig) -> Arc<Self> {
        Arc::new(Self {
            client,
            store: Mutex::new(Weak::new()),
            running: Arc::new(Mutex::new(HashMap::new())),
            dashboard: Arc::new(dashboard),
            failures: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Give the syncer its way back to the store.
    ///
    /// Separate from [`Syncer::new`] because the store needs the syncer to
    /// exist and the syncer needs the store. Weak, so the store stays free to
    /// drop.
    pub fn attach(&self, store: &Arc<SqliteStore>) {
        *self.store.lock().expect("not poisoned") = Arc::downgrade(store);
    }

    /// The store, if it is still alive. `None` once the TUI has dropped it,
    /// which is how a poll task learns to stop.
    pub(crate) fn store(&self) -> Option<Arc<SqliteStore>> {
        self.store.lock().expect("not poisoned").upgrade()
    }

    /// Consecutive failures for a target. Zero when it last succeeded.
    pub(crate) fn failures(&self, target: &RefreshTarget) -> u32 {
        self.failures
            .lock()
            .expect("not poisoned")
            .get(target)
            .copied()
            .unwrap_or(0)
    }

    fn job(&self) -> Option<Job> {
        Some(Job {
            client: self.client.clone(),
            store: self.store.lock().expect("not poisoned").upgrade()?,
            running: self.running.clone(),
            dashboard: self.dashboard.clone(),
            failures: self.failures.clone(),
        })
    }
}

impl Job {
    /// Fetch the inbox and write it through.
    ///
    /// A 304 is **not** an empty inbox: it means the cached copy is still
    /// correct and the request cost no rate limit, so nothing is written and
    /// only the freshness stamp moves. Confusing the two writes an empty list
    /// over good data.
    async fn sync_notifications(&self) -> Result<()> {
        let previous = self.store.with_cache(|c| c.list_meta(INBOX))?;
        let validators = previous
            .as_ref()
            .map(|m| m.validators.clone())
            .unwrap_or_default();

        let api = Notifications::new(&self.client);
        let now = self.store.now();

        match api.list(&NotificationFilter::default(), validators).await? {
            Conditional::NotModified { validators } => {
                tracing::info!("inbox unchanged (304), nothing rewritten");
                self.store.with_cache(|c| {
                    Self::stamp(c, validators, previous.and_then(|m| m.cursor), now)
                })?;
            }
            Conditional::Modified(page) => {
                let complete = page.next_page.is_none();
                tracing::info!(rows = page.items.len(), complete, "inbox updated");
                self.store.with_cache(|c| {
                    c.put_notifications(&page.items)?;
                    Self::stamp_complete(c, page.validators, complete, now)
                })?;
            }
        }
        Ok(())
    }

    /// Record how fresh the inbox is, and what would revalidate it.
    ///
    /// `put_list` with no ids: the notifications table holds the rows, and
    /// `list_meta` holds only their freshness.
    fn stamp(
        c: &Cache,
        validators: Validators,
        cursor: Option<String>,
        now: time::OffsetDateTime,
    ) -> std::result::Result<(), omaghy_model::CacheError> {
        c.put_list(
            INBOX,
            &[],
            &ListMeta {
                validators,
                cursor,
                // The inbox's rows live in the notifications table, so its
                // count is a query against that and never a stored total.
                total: None,
                complete: true,
                fetched_at: now,
            },
        )
    }

    fn stamp_complete(
        c: &Cache,
        validators: Validators,
        complete: bool,
        now: time::OffsetDateTime,
    ) -> std::result::Result<(), omaghy_model::CacheError> {
        c.put_list(
            INBOX,
            &[],
            &ListMeta {
                validators,
                cursor: None,
                total: None,
                complete,
                fetched_at: now,
            },
        )
    }

    /// Count every dashboard section in one request.
    ///
    /// Sections are counts, not rows, until M2 — so this stores a `total` and
    /// no ids. The count is what the surface renders, and counting stored ids
    /// would report the fetch limit instead of the queue.
    ///
    /// A section GitHub could not answer keeps whatever count it had rather
    /// than dropping to zero. Zero is a claim — "nothing needs your review" —
    /// and it is the wrong one to make out of a failure.
    async fn sync_dashboard(&self) -> Result<()> {
        let queries: Vec<String> = self
            .dashboard
            .sections
            .iter()
            .map(|s| s.query.clone())
            .collect();
        if queries.is_empty() {
            return Ok(());
        }

        let counts = self.client.search().counts(&queries).await?;
        let now = self.store.now();

        self.store.with_cache(|c| {
            for (query, count) in queries.iter().zip(&counts) {
                match count {
                    Ok(total) => {
                        c.put_list(
                            &dashboard_list_key(query),
                            &[],
                            &ListMeta {
                                validators: Validators::default(),
                                cursor: None,
                                total: Some(*total),
                                complete: true,
                                fetched_at: now,
                            },
                        )?;
                    }
                    Err(message) => {
                        tracing::warn!(%query, %message, "a dashboard section could not be counted");
                    }
                }
            }
            Ok(())
        })
    }

    /// Fill in what the REST payload left out.
    ///
    /// Separate from the list fetch: a different budget — one GraphQL point
    /// for a whole page — and a failed enrichment must not discard a list that
    /// arrived fine. `enrich` itself selects only rows whose detail is
    /// `Absent`, so handing it everything is correct and cheap.
    async fn enrich(&self) -> Result<()> {
        let wanted = self
            .store
            .with_cache(|c| c.notifications_wanting_enrichment(omaghy_api::ENRICHMENT_BATCH))?;
        if wanted.is_empty() {
            return Ok(());
        }
        let mut rows = self
            .store
            .with_cache(|c| c.notifications(&NotificationQuery::default()))?;
        rows.retain(|n| wanted.contains(&n.id));

        Notifications::new(&self.client).enrich(&mut rows).await?;
        self.store.with_cache(|c| c.put_notifications(&rows))?;
        Ok(())
    }

    /// Run one refresh and report what happened.
    ///
    /// **Every completed refresh logs one `info` line.** A TUI owns the
    /// screen, so the log file is the only window into work that happens
    /// without a keypress — and until this existed, background fetching was
    /// entirely invisible at the default level: the poll tick was `debug`, and
    /// the refresh itself logged nothing at any level. "Is it still polling?"
    /// had no answer short of a packet capture.
    async fn run(self, target: RefreshTarget) {
        let started = std::time::Instant::now();
        let result = match &target {
            RefreshTarget::Notifications => self.sync_notifications().await,
            RefreshTarget::NotificationDetails => self.enrich().await,
            RefreshTarget::Dashboard => self.sync_dashboard().await,
            // Pull request and issue lists arrive with the surfaces that
            // render rows for them, in M2. Reporting success for a target
            // nothing fetches is what made the dashboard claim a refresh it
            // never performed, so this arm is now only the unbuilt surfaces.
            _ => Ok(()),
        };
        self.running.lock().expect("not poisoned").remove(&target);
        let listed_ok = result.is_ok() && target == RefreshTarget::Notifications;
        {
            let mut failures = self.failures.lock().expect("not poisoned");
            match &result {
                Ok(()) => {
                    failures.remove(&target);
                }
                // Saturating rather than wrapping: a counter that rolled over
                // would reset the back-off to nothing after four billion
                // failures, which is a silly way to start hammering.
                Err(_) => *failures.entry(target.clone()).or_insert(0) += 1,
            }
        }
        let ms = started.elapsed().as_millis();
        match result {
            Ok(()) => {
                tracing::info!(?target, ms, "refresh finished");
                self.store.refresh_finished(target)
            }
            Err(e) => {
                // `warn`, not `error`: being offline is an ordinary state for
                // a program someone opens to check whether CI passed, and the
                // cached rows are still on screen.
                tracing::warn!(?target, ms, error = %e, "refresh failed");
                self.store.refresh_failed(target, e)
            }
        }
        // The list arrives without numbers, states or actors, so a row paints
        // as "awaiting details" until a second pass fills them in. Chained
        // here rather than asked for by the surface: the two-phase fetch is
        // the store's business, and a surface that had to know about it would
        // be a surface doing the store's job.
        if listed_ok {
            self.store.refresh(RefreshTarget::NotificationDetails);
        }
    }
}

#[async_trait]
impl Remote for Syncer {
    fn schedule(&self, target: RefreshTarget) {
        let Some(job) = self.job() else { return };
        let mut running = self.running.lock().expect("not poisoned");
        if running.contains_key(&target) {
            return; // already in flight: one request, two waiters
        }
        let handle = tokio::spawn(job.run(target.clone()));
        running.insert(target, handle);
    }

    fn cancel(&self, target: &RefreshTarget) {
        if let Some(handle) = self.running.lock().expect("not poisoned").remove(target) {
            handle.abort();
        }
    }

    async fn mark_read(&self, ids: &[NotificationId]) -> Result<()> {
        Notifications::new(&self.client).mark_read(ids).await
    }

    /// Sends nothing. GitHub has no counterpart — verified by W2.1 against the
    /// live API — and the store's own documentation says the row comes back
    /// read on the next fetch.
    async fn mark_unread(&self, _ids: &[NotificationId]) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omaghy_api::cassette::StubTransport;
    use omaghy_api::{GitHubClient, HttpResponse, Token, TransportError};
    use omaghy_store::Viewer;

    fn syncer_over(transport: Arc<dyn omaghy_api::Transport>) -> (Arc<Syncer>, Arc<SqliteStore>) {
        let client = Arc::new(GitHubClient::new(
            Token::new("gho_notarealtoken").expect("non-empty"),
            transport,
        ));
        let syncer = Syncer::new(client);
        let store = Arc::new(
            SqliteStore::in_memory(Viewer::new("ShaxP"))
                .expect("in-memory cache")
                .with_remote(syncer.clone()),
        );
        syncer.attach(&store);
        (syncer, store)
    }

    /// Wait for the spawned refresh to finish, without sleeping a fixed amount
    /// and hoping. Fails loudly rather than hanging.
    async fn settle(syncer: &Arc<Syncer>) {
        for _ in 0..200 {
            if syncer.running.lock().expect("not poisoned").is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("a refresh never finished");
    }

    /// The back-off in `poll` is only real if this counter moves. Asserting
    /// the arithmetic alone would pass with the wiring removed.
    #[tokio::test]
    async fn a_failed_refresh_is_counted_and_a_good_one_clears_it() {
        let offline = Arc::new(StubTransport::failing(TransportError::Unreachable(
            "no route to host".into(),
        )));
        let (syncer, store) = syncer_over(offline);

        assert_eq!(syncer.failures(&RefreshTarget::Notifications), 0);

        store.refresh(RefreshTarget::Notifications);
        settle(&syncer).await;
        assert_eq!(
            syncer.failures(&RefreshTarget::Notifications),
            1,
            "an offline fetch must raise the back-off"
        );

        store.refresh(RefreshTarget::Notifications);
        settle(&syncer).await;
        assert_eq!(syncer.failures(&RefreshTarget::Notifications), 2);

        // Now let one succeed. A 304 is the cheapest real success the inbox
        // has — nothing is written and only the freshness stamp moves — and it
        // needs no recorded request path to match against.
        let (syncer, store) = syncer_over(Arc::new(StubTransport::always(HttpResponse::new(304))));
        store.refresh(RefreshTarget::Notifications);
        settle(&syncer).await;
        assert_eq!(
            syncer.failures(&RefreshTarget::Notifications),
            0,
            "a network that comes back polls at the configured rate, not the backed-off one"
        );
    }
}

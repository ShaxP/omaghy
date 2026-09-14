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

use async_trait::async_trait;
use omaghy_api::{Conditional, GitHubClient, NotificationFilter, Notifications};
use omaghy_cache::{Cache, ListMeta, Remote, SqliteStore};
use omaghy_model::{NotificationId, Result};
use omaghy_store::{NotificationQuery, RefreshTarget, Store};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::task::JoinHandle;

/// Notifications have their own table, but their freshness lives with every
/// other list's, under this key (`spec/20-store.md` §3).
const INBOX: &str = "notifications";

/// `omaghy-api` and `omaghy-cache` each define their own `Validators`, with
/// identical fields, because neither may depend on the other and
/// `omaghy-model` does not carry the type. Converting here is the smallest fix
/// that does not block integration; the right one is to move `Validators` into
/// the model, which is a contract change and merges alone.
fn to_api(v: omaghy_cache::Validators) -> omaghy_api::Validators {
    omaghy_api::Validators {
        etag: v.etag,
        last_modified: v.last_modified,
    }
}

fn to_cache(v: omaghy_api::Validators) -> omaghy_cache::Validators {
    omaghy_cache::Validators {
        etag: v.etag,
        last_modified: v.last_modified,
    }
}

/// The pieces a background fetch needs. Cloned into the spawned task, so
/// `schedule` never needs an owned `Arc<Syncer>`.
#[derive(Clone)]
struct Job {
    client: Arc<GitHubClient>,
    store: Arc<SqliteStore>,
    running: Arc<Mutex<HashMap<RefreshTarget, JoinHandle<()>>>>,
}

pub struct Syncer {
    client: Arc<GitHubClient>,
    store: Mutex<Weak<SqliteStore>>,
    running: Arc<Mutex<HashMap<RefreshTarget, JoinHandle<()>>>>,
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
        Arc::new(Self {
            client,
            store: Mutex::new(Weak::new()),
            running: Arc::new(Mutex::new(HashMap::new())),
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

    fn job(&self) -> Option<Job> {
        Some(Job {
            client: self.client.clone(),
            store: self.store.lock().expect("not poisoned").upgrade()?,
            running: self.running.clone(),
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
        let validators = to_api(
            previous
                .as_ref()
                .map(|m| m.validators.clone())
                .unwrap_or_default(),
        );

        let api = Notifications::new(&self.client);
        let now = self.store.now();

        match api.list(&NotificationFilter::default(), validators).await? {
            Conditional::NotModified { validators } => {
                self.store.with_cache(|c| {
                    Self::stamp(
                        c,
                        to_cache(validators),
                        previous.and_then(|m| m.cursor),
                        now,
                    )
                })?;
            }
            Conditional::Modified(page) => {
                let complete = page.next_page.is_none();
                self.store.with_cache(|c| {
                    c.put_notifications(&page.items)?;
                    Self::stamp_complete(c, to_cache(page.validators), complete, now)
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
        validators: omaghy_cache::Validators,
        cursor: Option<String>,
        now: time::OffsetDateTime,
    ) -> std::result::Result<(), omaghy_model::CacheError> {
        c.put_list(
            INBOX,
            &[],
            &ListMeta {
                validators,
                cursor,
                complete: true,
                fetched_at: now,
            },
        )
    }

    fn stamp_complete(
        c: &Cache,
        validators: omaghy_cache::Validators,
        complete: bool,
        now: time::OffsetDateTime,
    ) -> std::result::Result<(), omaghy_model::CacheError> {
        c.put_list(
            INBOX,
            &[],
            &ListMeta {
                validators,
                cursor: None,
                complete,
                fetched_at: now,
            },
        )
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

    async fn run(self, target: RefreshTarget) {
        let result = match &target {
            RefreshTarget::Notifications => self.sync_notifications().await,
            RefreshTarget::NotificationDetails => self.enrich().await,
            // Dashboard sections and the rest arrive with the surfaces that
            // render rows for them, in M2.
            _ => Ok(()),
        };
        self.running.lock().expect("not poisoned").remove(&target);
        let listed_ok = result.is_ok() && target == RefreshTarget::Notifications;
        match result {
            Ok(()) => self.store.refresh_finished(target),
            Err(e) => self.store.refresh_failed(target, e),
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

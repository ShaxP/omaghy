//! A `Store` backed by a fixed corpus, with knobs for everything that goes
//! wrong.
//!
//! This is what surface agents build against while the real client is being
//! written, and what snapshot tests run on so screens are deterministic.
//! It must be able to produce **every** arm of the state matrix
//! (`spec/30-ui.md` §8) on demand: the error states are the ones that rot
//! unnoticed, and they are most of what a user sees on a bad day.

use crate::{
    event::{RefreshTarget, StoreEvent},
    fresh::Fresh,
    query::{DashboardConfig, NotificationQuery, Page, ReadFilter},
    store::{Dashboard, DashboardSectionData, Store, Viewer},
};
use async_trait::async_trait;
use omaghy_model::{
    Enrichment, Notification, NotificationId, NotificationReason, RepoRef, Result, StoreError,
    SubjectKind, SubjectRef,
};
use std::sync::{Arc, Mutex};
use time::{Duration, OffsetDateTime};
use tokio::sync::broadcast;

/// The fixed clock the corpus is built against, so relative ages are stable
/// in snapshots.
pub const FIXTURE_NOW: OffsetDateTime = time::macros::datetime!(2026-09-10 11:00 UTC);

/// How the fake should misbehave.
#[derive(Debug, Clone, Default)]
pub struct Behaviour {
    /// Fail every read with this.
    pub read_error: Option<StoreError>,
    /// Fail every mutation with this.
    pub write_error: Option<StoreError>,
    /// Report reads as stale.
    pub stale: bool,
    /// Report a refresh as in flight.
    pub refreshing: bool,
    /// Answer reads with no rows at all, as on a cold start.
    pub empty: bool,
    /// Report data as never fetched — the cold-cache case, distinct from
    /// fetched-and-empty.
    pub never_fetched: bool,
}

impl Behaviour {
    pub fn offline_with_cache() -> Self {
        Self {
            stale: true,
            read_error: None,
            ..Default::default()
        }
    }

    pub fn offline_without_cache() -> Self {
        Self {
            empty: true,
            never_fetched: true,
            ..Default::default()
        }
    }

    pub fn failing(err: StoreError) -> Self {
        Self {
            read_error: Some(err),
            ..Default::default()
        }
    }
}

pub struct FakeStore {
    viewer: Viewer,
    rows: Mutex<Vec<Notification>>,
    behaviour: Mutex<Behaviour>,
    events: broadcast::Sender<StoreEvent>,
    /// Targets passed to `refresh()`, so tests can assert scheduling.
    scheduled: Arc<Mutex<Vec<RefreshTarget>>>,
}

impl std::fmt::Debug for FakeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeStore")
            .field("viewer", &self.viewer)
            .finish_non_exhaustive()
    }
}

impl FakeStore {
    /// The full 29-row corpus.
    pub fn with_corpus() -> Self {
        Self::with_rows(corpus())
    }

    pub fn with_rows(rows: Vec<Notification>) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            viewer: Viewer::new("ShaxP"),
            rows: Mutex::new(rows),
            behaviour: Mutex::new(Behaviour::default()),
            events,
            scheduled: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// An empty store — the "all caught up" and cold-start cases.
    pub fn empty() -> Self {
        Self::with_rows(Vec::new())
    }

    pub fn set_behaviour(&self, b: Behaviour) {
        *self.behaviour.lock().unwrap() = b;
    }

    /// Targets `refresh()` was called with, in order.
    pub fn scheduled(&self) -> Vec<RefreshTarget> {
        self.scheduled.lock().unwrap().clone()
    }

    /// Emit an event as the real store would, so UI reactions can be tested.
    pub fn emit(&self, ev: StoreEvent) {
        let _ = self.events.send(ev);
    }

    fn wrap<T>(&self, value: T) -> Fresh<T> {
        let b = self.behaviour.lock().unwrap();
        if b.never_fetched {
            return Fresh::never(value).refreshing(b.refreshing);
        }
        let age = if b.stale {
            Duration::hours(2)
        } else {
            Duration::seconds(5)
        };
        Fresh::from_cache(value, FIXTURE_NOW - age, Duration::minutes(5), FIXTURE_NOW)
            .refreshing(b.refreshing)
    }

    fn matches(n: &Notification, q: &NotificationQuery) -> bool {
        if q.read == ReadFilter::UnreadOnly && !n.unread {
            return false;
        }
        if let Some(repo) = &q.repo
            && &n.repo != repo
        {
            return false;
        }
        if !q.reasons.is_empty() && !q.reasons.contains(&n.reason) {
            return false;
        }
        if !q.kinds.is_empty() && !q.kinds.contains(&n.kind) {
            return false;
        }
        if let Some(s) = &q.search {
            let s = s.to_lowercase();
            let hay = format!(
                "{} {}",
                n.title.to_lowercase(),
                n.repo.to_string().to_lowercase()
            );
            if !hay.contains(&s) {
                return false;
            }
        }
        true
    }
}

#[async_trait]
impl Store for FakeStore {
    fn subscribe(&self) -> broadcast::Receiver<StoreEvent> {
        self.events.subscribe()
    }

    fn viewer(&self) -> &Viewer {
        &self.viewer
    }

    async fn dashboard(&self, cfg: &DashboardConfig) -> Result<Fresh<Dashboard>> {
        if let Some(e) = self.behaviour.lock().unwrap().read_error.clone() {
            return Err(e);
        }
        let empty = self.behaviour.lock().unwrap().empty;
        let rows = self.rows.lock().unwrap();
        let sections = cfg
            .sections
            .iter()
            .map(|s| DashboardSectionData {
                title: s.title.clone(),
                query: s.query.clone(),
                count: if empty {
                    0
                } else {
                    // Enough signal to render distinct sections without
                    // pretending to execute GitHub search syntax.
                    rows.iter().filter(|n| n.reason.is_directed_at_me()).count() as u32
                },
            })
            .collect();
        Ok(self.wrap(Dashboard { sections }))
    }

    async fn notifications(&self, q: &NotificationQuery) -> Result<Fresh<Page<Notification>>> {
        if let Some(e) = self.behaviour.lock().unwrap().read_error.clone() {
            return Err(e);
        }
        if self.behaviour.lock().unwrap().empty {
            return Ok(self.wrap(Page::empty()));
        }
        let rows = self.rows.lock().unwrap();
        let mut items: Vec<_> = rows
            .iter()
            .filter(|n| Self::matches(n, q))
            .cloned()
            .collect();
        items.sort_by_key(|n| std::cmp::Reverse(n.updated_at));
        if let Some(limit) = q.limit {
            items.truncate(limit);
        }
        Ok(self.wrap(Page::complete(items)))
    }

    fn refresh(&self, target: RefreshTarget) {
        self.scheduled.lock().unwrap().push(target.clone());
        let _ = self.events.send(StoreEvent::RefreshStarted(target.clone()));
        let _ = self.events.send(StoreEvent::Updated(target));
    }

    fn cancel(&self, target: &RefreshTarget) {
        self.scheduled.lock().unwrap().retain(|t| t != target);
    }

    async fn mark_read(&self, ids: &[NotificationId]) -> Result<()> {
        if let Some(e) = self.behaviour.lock().unwrap().write_error.clone() {
            return Err(e);
        }
        let mut rows = self.rows.lock().unwrap();
        for n in rows.iter_mut() {
            if ids.contains(&n.id) {
                n.unread = false;
            }
        }
        drop(rows);
        let _ = self
            .events
            .send(StoreEvent::Updated(RefreshTarget::Notifications));
        Ok(())
    }

    async fn mark_unread(&self, ids: &[NotificationId]) -> Result<()> {
        if let Some(e) = self.behaviour.lock().unwrap().write_error.clone() {
            return Err(e);
        }
        let mut rows = self.rows.lock().unwrap();
        for n in rows.iter_mut() {
            if ids.contains(&n.id) {
                n.unread = true;
            }
        }
        drop(rows);
        let _ = self
            .events
            .send(StoreEvent::Updated(RefreshTarget::Notifications));
        Ok(())
    }
}

// ---------------------------------------------------------------- the corpus

/// 29 curated rows. Real titles come from the public `ShaxP/shax`; the rest is
/// synthesized, because a real inbox is too homogeneous to design against —
/// the one this was drawn from is 14 notifications, all `author`, all
/// `PullRequest`, all one repo, all read.
///
/// Covers every reason, seven subject kinds, ages from four minutes to four
/// hundred days, rows still awaiting enrichment, and a row with no avatar.
pub fn corpus() -> Vec<Notification> {
    use NotificationReason as R;
    use SubjectKind as K;

    /// id, unread, reason, repo, kind, minutes ago, title, enriched
    type Row = (u32, bool, R, &'static str, K, i64, &'static str, bool);

    let rows: &[Row] = &[
        (
            1,
            true,
            R::ReviewRequested,
            "quickshell/quickshell",
            K::PullRequest,
            4,
            "Add SocketServer reconnect backoff and idle timeout",
            true,
        ),
        (
            2,
            true,
            R::Mention,
            "basecamp/omarchy",
            K::Issue,
            22,
            "Bar widget plugins should be able to declare a preferred section",
            true,
        ),
        (
            3,
            true,
            R::CiActivity,
            "ShaxP/shax",
            K::CheckSuite,
            41,
            "CI failed on main",
            true,
        ),
        (
            4,
            true,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            120,
            "fix: syntax highlighting follows the Dark/Light/System toggle",
            true,
        ),
        (
            5,
            true,
            R::Assign,
            "ShaxP/omaghy",
            K::Issue,
            180,
            "Notifications inbox: decide enrichment strategy",
            true,
        ),
        (
            6,
            true,
            R::Comment,
            "rust-lang/rust",
            K::PullRequest,
            300,
            "Stabilize `let_chains` in the 2024 edition",
            false,
        ),
        (
            7,
            true,
            R::TeamMention,
            "some-very-long-organization-name/an-equally-long-repository-name",
            K::Discussion,
            360,
            "RFC: unifying the plugin manifest across shell surfaces",
            false,
        ),
        (
            8,
            true,
            R::Subscribed,
            "quickshell/quickshell",
            K::PullRequest,
            480,
            "Refactor the Wayland layer-shell surface lifecycle so that anchors, exclusive zones, and keyboard focus are reconciled in a single pass",
            true,
        ),
        (
            9,
            true,
            R::SecurityAlert,
            "ShaxP/omaghy",
            K::VulnerabilityAlert,
            660,
            "Moderate severity vulnerability in openssl 0.10.66",
            true,
        ),
        (
            10,
            false,
            R::Subscribed,
            "rust-lang/rust",
            K::Release,
            1_440,
            "Rust 1.94.0",
            true,
        ),
        (
            11,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            1_680,
            "M7 slice 1: light theme + Dark/Light/System toggle",
            true,
        ),
        (
            12,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            2_880,
            "fix: tighter markdown spacing in chat bubbles",
            true,
        ),
        (
            13,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            3_240,
            "Ollama per-model tool + vision probing (closes M6)",
            true,
        ),
        (
            14,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            4_320,
            "fix: block-focus works while the assistant panel is open",
            true,
        ),
        (
            15,
            false,
            R::StateChange,
            "basecamp/omarchy",
            K::Issue,
            4_860,
            "Theme switcher should preview before applying",
            true,
        ),
        (
            16,
            false,
            R::Author,
            "ShaxP/omaghy",
            K::PullRequest,
            5_760,
            "spec: notifications inbox domain model",
            true,
        ),
        (
            17,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            7_200,
            "Tool integration: run_command via safety gate (M6 loop close)",
            false,
        ),
        (
            18,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            8_640,
            "M6 slice 4: assistant chat overlay + explain-on-error + capability gating",
            true,
        ),
        (
            19,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            11_520,
            "M6 slice 3: Ollama provider (local, capability-probed)",
            true,
        ),
        (
            20,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            12_960,
            "M6 slice 2b: Claude subscription lane (local CLI subprocess)",
            true,
        ),
        (
            21,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            15_840,
            "M6 slice 2a: Claude provider — API key lane (Rust proxy)",
            true,
        ),
        (
            22,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            18_720,
            "M6 slice 1: safety gate + AssistantProvider interface",
            true,
        ),
        (
            23,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            23_040,
            "docs: pluggable AssistantProvider model for M6",
            true,
        ),
        (
            24,
            false,
            R::Comment,
            "ShaxP/clipboard-sharing-mac-omarchy",
            K::Commit,
            30_240,
            "Handle wl-paste MIME negotiation on macOS bridge",
            true,
        ),
        (
            25,
            false,
            R::Manual,
            "some-very-long-organization-name/an-equally-long-repository-name",
            K::Issue,
            43_200,
            "Tracking: plugin manifest schemaVersion 2",
            true,
        ),
        (
            26,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            89_280,
            "M5 slice 3: ls widget (closes M5)",
            true,
        ),
        (
            27,
            false,
            R::Author,
            "ShaxP/shax",
            K::PullRequest,
            169_920,
            "M5 slice 2 follow-up: silent-reads model + sticky-bottom live widget",
            true,
        ),
        (
            28,
            false,
            R::Invitation,
            "basecamp/omarchy",
            K::Issue,
            576_000,
            "You were invited to collaborate",
            true,
        ),
        (
            29,
            true,
            R::Comment,
            "some-very-long-organization-name/an-equally-long-repository-name",
            K::Issue,
            9,
            "Short one",
            true,
        ),
    ];

    rows.iter()
        .map(|(id, unread, reason, repo, kind, mins, title, enriched)| {
            let repo_ref = RepoRef::parse(repo).expect("fixture repo names are well-formed");
            let segment = kind.api_segment().unwrap_or("issues");
            let subject = SubjectRef::from_api_url(&format!(
                "https://api.github.com/repos/{repo}/{segment}/{id}"
            ));
            Notification {
                id: NotificationId(format!("{}", 24_555_446_000u64 + u64::from(*id))),
                unread: *unread,
                reason: reason.clone(),
                updated_at: FIXTURE_NOW - Duration::minutes(*mins),
                title: (*title).to_owned(),
                kind: kind.clone(),
                repo: repo_ref,
                subject,
                detail: if *enriched {
                    Enrichment::Pending
                } else {
                    Enrichment::Absent
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corpus_covers_the_design_space() {
        let c = corpus();
        assert_eq!(c.len(), 29);
        assert_eq!(c.iter().filter(|n| n.unread).count(), 10);

        let reasons: std::collections::HashSet<_> = c.iter().map(|n| n.reason.label()).collect();
        assert!(
            reasons.len() >= 10,
            "expected most reasons represented, got {}",
            reasons.len()
        );

        let kinds: std::collections::HashSet<_> =
            c.iter().map(|n| format!("{:?}", n.kind)).collect();
        assert!(
            kinds.len() >= 6,
            "expected several subject kinds, got {}",
            kinds.len()
        );

        // The edge cases the design has to survive.
        assert!(c.iter().any(|n| n.title.len() > 130), "a very long title");
        assert!(
            c.iter().any(|n| n.repo.to_string().len() > 50),
            "a very long repo name"
        );
        assert!(
            c.iter().any(|n| matches!(n.detail, Enrichment::Absent)),
            "rows still awaiting enrichment"
        );
    }

    #[tokio::test]
    async fn reads_never_fail_by_default_and_are_newest_first() {
        let s = FakeStore::with_corpus();
        let page = s
            .notifications(&NotificationQuery::default())
            .await
            .unwrap();
        assert_eq!(page.value.len(), 29);
        let times: Vec<_> = page.value.items.iter().map(|n| n.updated_at).collect();
        assert!(times.windows(2).all(|w| w[0] >= w[1]), "newest first");
    }

    #[tokio::test]
    async fn filters_narrow_the_page() {
        let s = FakeStore::with_corpus();
        let unread = s.notifications(&NotificationQuery::unread()).await.unwrap();
        assert_eq!(unread.value.len(), 10);

        let q = NotificationQuery {
            repo: RepoRef::parse("ShaxP/shax"),
            ..Default::default()
        };
        let shax = s.notifications(&q).await.unwrap();
        assert!(shax.value.items.iter().all(|n| n.repo.name == "shax"));

        let q = NotificationQuery {
            search: Some("ollama".into()),
            ..Default::default()
        };
        let hits = s.notifications(&q).await.unwrap();
        assert_eq!(
            hits.value.len(),
            2,
            "search is case-insensitive over titles"
        );
    }

    #[tokio::test]
    async fn marking_read_is_idempotent_and_announces_itself() {
        let s = FakeStore::with_corpus();
        let mut rx = s.subscribe();
        let id = NotificationId("24555446001".into());

        s.mark_read(std::slice::from_ref(&id)).await.unwrap();
        assert!(matches!(rx.recv().await.unwrap(), StoreEvent::Updated(_)));

        let before = s
            .notifications(&NotificationQuery::unread())
            .await
            .unwrap()
            .value
            .len();
        // Marking an already-read thread must not error, per the trait contract.
        s.mark_read(std::slice::from_ref(&id)).await.unwrap();
        let after = s
            .notifications(&NotificationQuery::unread())
            .await
            .unwrap()
            .value
            .len();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn every_state_matrix_arm_is_reachable() {
        // These are the states that rot unnoticed, so the fake must be able to
        // produce all of them on demand (spec/30-ui.md §8).
        let s = FakeStore::with_corpus();

        s.set_behaviour(Behaviour::offline_with_cache());
        let stale = s
            .notifications(&NotificationQuery::default())
            .await
            .unwrap();
        assert!(
            stale.stale && !stale.value.is_empty(),
            "stale data is never hidden"
        );

        s.set_behaviour(Behaviour::offline_without_cache());
        let cold = s
            .notifications(&NotificationQuery::default())
            .await
            .unwrap();
        assert!(cold.value.is_empty() && cold.fetched_at.is_none());

        s.set_behaviour(Behaviour::failing(StoreError::Forbidden));
        assert!(
            s.notifications(&NotificationQuery::default())
                .await
                .is_err()
        );

        s.set_behaviour(Behaviour {
            refreshing: true,
            ..Default::default()
        });
        let r = s
            .notifications(&NotificationQuery::default())
            .await
            .unwrap();
        assert!(
            r.refreshing && !r.value.is_empty(),
            "content is not replaced by a spinner"
        );

        // "All caught up" is a real state, distinct from a cold cache.
        let empty = FakeStore::empty();
        let none = empty
            .notifications(&NotificationQuery::default())
            .await
            .unwrap();
        assert!(none.value.is_empty() && none.fetched_at.is_some());
    }

    #[tokio::test]
    async fn write_failures_can_be_injected() {
        let s = FakeStore::with_corpus();
        s.set_behaviour(Behaviour {
            write_error: Some(StoreError::Forbidden),
            ..Default::default()
        });
        assert!(
            s.mark_read(&[NotificationId("24555446001".into())])
                .await
                .is_err()
        );
    }

    #[test]
    fn refresh_records_what_was_scheduled() {
        let s = FakeStore::with_corpus();
        s.refresh(RefreshTarget::Notifications);
        s.refresh(RefreshTarget::Dashboard);
        assert_eq!(s.scheduled().len(), 2);
        s.cancel(&RefreshTarget::Dashboard);
        assert_eq!(s.scheduled(), vec![RefreshTarget::Notifications]);
    }
}

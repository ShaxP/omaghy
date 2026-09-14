//! Tests for the things that actually bite.
//!
//! **No test opens a socket.** There is no code in this crate that can: the
//! only route to one is the [`Remote`] trait, and the doubles below either
//! record or fail.
//!
//! **No test touches a shared path.** `tempfile` is not in the M1 dependency
//! set (`spec/90-plan.md` §2.1), so file-backed tests use [`TempDir`] — a
//! directory under `std::env::temp_dir()` named from the process id and an
//! atomic counter, removed on drop. Everything that does not need a file on
//! disk uses an in-memory database instead.

use crate::{
    Cache, Clock, EntityKind, ListMeta, NOTIFICATIONS_LIST, RecordIntent, Remote, SCHEMA_VERSION,
    SqliteStore, Stored, Validators, cache::NOTIFICATIONS_LIST as INBOX, dashboard_list_key,
    schema::Opened, ttl,
};
use async_trait::async_trait;
use omaghy_model::{
    Actor, AuthError, Block, CacheError, CheckConclusion, CheckRollup, CheckRun, CheckStatus,
    CommitStatus, Enrichment, Issue, IssueState, IssueStateReason, Label, Markdown, Mergeable,
    NodeId, Notification, NotificationId, NotificationReason, PrDisplayStatus, PrState,
    PullRequest, Reactions, Repo, RepoRef, Result, ReviewDecision, ReviewState, ReviewSummary, Rgb,
    RollupState, SpanStyle, StatusState, StoreError, StyledSpan, SubjectDetail, SubjectId,
    SubjectKind, SubjectRef, SubjectStatus, TimelineEvent, TimelineKind,
};
use omaghy_store::{
    Fresh, RefreshTarget, Source, Store, StoreEvent, Viewer,
    query::{DashboardConfig, DashboardSection, NotificationQuery, Page, ReadFilter},
};
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use time::{Duration, OffsetDateTime, macros::datetime};

const NOW: OffsetDateTime = datetime!(2026-09-11 12:00 UTC);

// --------------------------------------------------------------- scaffolding

/// A private directory under the system temp dir, removed on drop.
///
/// Two tests running concurrently must not share a database file — SQLite
/// would happily let them, and the resulting failure looks like a cache bug
/// rather than a test bug.
#[derive(Debug)]
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "omaghy-cache-test-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn db(&self) -> PathBuf {
        self.0.join("cache.db")
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A [`Remote`] whose mutations always fail. This is the only way to exercise
/// rollback, and rollback is the half of an optimistic update that nobody
/// sees until the day it matters.
#[derive(Debug)]
struct FailingRemote(StoreError);

#[async_trait]
impl Remote for FailingRemote {
    fn schedule(&self, _target: RefreshTarget) {}
    fn cancel(&self, _target: &RefreshTarget) {}
    async fn mark_read(&self, _ids: &[NotificationId]) -> Result<()> {
        Err(self.0.clone())
    }
    async fn mark_unread(&self, _ids: &[NotificationId]) -> Result<()> {
        Err(self.0.clone())
    }
}

fn notification(id: &str, unread: bool, mins: i64) -> Notification {
    Notification {
        id: NotificationId(id.into()),
        unread,
        reason: NotificationReason::ReviewRequested,
        updated_at: NOW - Duration::minutes(mins),
        title: format!("notification {id}"),
        kind: SubjectKind::PullRequest,
        repo: RepoRef::new("ShaxP", "omaghy"),
        subject: SubjectRef::from_api_url("https://api.github.com/repos/ShaxP/omaghy/pulls/1"),
        detail: Enrichment::Absent,
    }
}

fn store(viewer: &str) -> SqliteStore {
    SqliteStore::in_memory(Viewer::new(viewer))
        .expect("in-memory store")
        .with_clock(Clock::Fixed(NOW))
}

// ------------------------------------------------------- viewer keying (§3.1)

#[test]
fn two_viewers_never_see_each_others_rows() {
    // The bug this prevents is invisible until someone switches accounts, at
    // which point every "does this need me" answer is quietly wrong.
    let tmp = TempDir::new("viewers");
    let mine = Cache::open(tmp.db(), "ShaxP").unwrap();
    let theirs = Cache::open(tmp.db(), "octocat").unwrap();

    mine.put_notifications(&[notification("1", true, 5)])
        .unwrap();
    mine.put_entity(
        EntityKind::Repo,
        &NodeId("R_1".into()),
        &"mine",
        &Validators::etag("W/\"abc\""),
        NOW,
    )
    .unwrap();
    mine.put_list("prs", &[NodeId("PR_1".into())], &ListMeta::complete_at(NOW))
        .unwrap();
    mine.kv_put("budget", b"4999").unwrap();

    // Same file, same tables, different viewer: nothing.
    assert!(
        theirs
            .notifications(&NotificationQuery::default())
            .unwrap()
            .is_empty()
    );
    assert!(
        theirs
            .entity::<String>(&NodeId("R_1".into()))
            .unwrap()
            .is_none()
    );
    assert!(theirs.list("prs").unwrap().is_empty());
    assert!(theirs.list_meta("prs").unwrap().is_none());
    assert!(theirs.kv_get("budget").unwrap().is_none());

    // And the first viewer still has all of it — isolation, not deletion.
    assert_eq!(
        mine.notifications(&NotificationQuery::default())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        mine.entity::<String>(&NodeId("R_1".into()))
            .unwrap()
            .unwrap()
            .value,
        "mine"
    );
    assert_eq!(mine.list("prs").unwrap().len(), 1);
    assert_eq!(
        mine.kv_get("budget").unwrap().as_deref(),
        Some(&b"4999"[..])
    );
}

#[test]
fn the_same_id_may_exist_for_both_viewers_with_different_content() {
    // Notification 42 is unread for me and read for you. Both are true.
    let tmp = TempDir::new("same-id");
    let mine = Cache::open(tmp.db(), "ShaxP").unwrap();
    let theirs = Cache::open(tmp.db(), "octocat").unwrap();

    mine.put_notifications(&[notification("42", true, 1)])
        .unwrap();
    theirs
        .put_notifications(&[notification("42", false, 1)])
        .unwrap();

    assert!(
        mine.notifications(&NotificationQuery::unread())
            .unwrap()
            .len()
            == 1
    );
    assert!(
        theirs
            .notifications(&NotificationQuery::unread())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_mutation_by_one_viewer_does_not_touch_the_other() {
    let tmp = TempDir::new("mutation-viewers");
    let mine = SqliteStore::open(tmp.db(), Viewer::new("ShaxP")).unwrap();
    let theirs = SqliteStore::open(tmp.db(), Viewer::new("octocat")).unwrap();

    for s in [&mine, &theirs] {
        s.with_cache(|c| c.put_notifications(&[notification("42", true, 1)]))
            .unwrap();
    }

    mine.mark_read(&[NotificationId("42".into())])
        .await
        .unwrap();

    assert!(
        mine.notifications(&NotificationQuery::unread())
            .await
            .unwrap()
            .value
            .is_empty()
    );
    assert_eq!(
        theirs
            .notifications(&NotificationQuery::unread())
            .await
            .unwrap()
            .value
            .len(),
        1,
        "the other account's inbox must not move"
    );
}

// ------------------------------------------------------- schema (§3, §3.2)

#[test]
fn a_schema_version_mismatch_rebuilds_rather_than_erroring() {
    let tmp = TempDir::new("version");
    let db = tmp.db();

    {
        let cache = Cache::open(&db, "ShaxP").unwrap();
        cache
            .put_notifications(&[notification("1", true, 1)])
            .unwrap();
    }

    // Pretend the last release wrote a different schema.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 7)
            .unwrap();
    }

    // It is a cache: the rows are re-fetchable, so they go, and the caller
    // gets a working database rather than an error it cannot act on.
    let cache = Cache::open(&db, "ShaxP").expect("a version mismatch is not a failure");
    assert!(
        cache
            .notifications(&NotificationQuery::default())
            .unwrap()
            .is_empty()
    );

    let conn = rusqlite::Connection::open(&db).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
}

#[test]
fn a_corrupt_database_is_reported_as_corrupt_and_then_rebuilt() {
    let tmp = TempDir::new("corrupt");
    let db = tmp.db();
    std::fs::write(&db, b"this is not a database, it is a sentence").unwrap();

    let err = Cache::open(&db, "ShaxP").expect_err("garbage is not a cache");
    assert!(
        matches!(err, CacheError::Corrupt(_)),
        "expected Corrupt, got {err:?}"
    );

    // The one error that invalidates what the UI is holding.
    let as_store: StoreError = err.into();
    assert!(!as_store.keeps_cached_content());

    // …and the file was rebuilt on the way out, so this is a single bad read
    // rather than a permanently broken install.
    let cache = Cache::open(&db, "ShaxP").expect("the second open succeeds");
    cache
        .put_notifications(&[notification("1", true, 1)])
        .unwrap();
    assert_eq!(
        cache
            .notifications(&NotificationQuery::default())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_truncated_database_is_corrupt_not_merely_empty() {
    let tmp = TempDir::new("truncated");
    let db = tmp.db();
    {
        let cache = Cache::open(&db, "ShaxP").unwrap();
        cache
            .put_notifications(&[notification("1", true, 1)])
            .unwrap();
    }
    // Scribble over the header of a real database.
    let mut bytes = std::fs::read(&db).unwrap();
    bytes[0..16].copy_from_slice(b"not-a-header----");
    std::fs::write(&db, &bytes).unwrap();
    // Remove the WAL so the header we broke is the one that gets read.
    let _ = std::fs::remove_file(db.with_extension("db-wal"));

    let err = Cache::open(&db, "ShaxP").expect_err("a broken header is corruption");
    assert!(matches!(err, CacheError::Corrupt(_)), "got {err:?}");
}

#[test]
fn the_database_runs_in_wal_so_watch_can_share_it() {
    // `omaghy watch` is a second process against the same file and there is no
    // IPC between them (`spec/00-overview.md` §6). WAL is the whole protocol.
    let tmp = TempDir::new("wal");
    let cache = Cache::open(tmp.db(), "ShaxP").unwrap();
    let conn = rusqlite::Connection::open(tmp.db()).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    drop(cache);
}

#[test]
fn a_second_connection_reads_what_the_first_wrote() {
    let tmp = TempDir::new("two-readers");
    let writer = Cache::open(tmp.db(), "ShaxP").unwrap();
    let reader = Cache::open(tmp.db(), "ShaxP").unwrap();

    writer
        .put_notifications(&[notification("1", true, 1)])
        .unwrap();
    assert_eq!(
        reader
            .notifications(&NotificationQuery::default())
            .unwrap()
            .len(),
        1,
        "the watch process must see the TUI's writes"
    );
}

#[test]
fn opening_reports_whether_it_had_to_build() {
    let tmp = TempDir::new("opened");
    let (_, first) = crate::schema::open(&tmp.db()).unwrap();
    assert_eq!(first, Opened::Rebuilt);
    let (_, second) = crate::schema::open(&tmp.db()).unwrap();
    assert_eq!(second, Opened::Existing);
}

#[test]
fn a_rebuild_takes_the_wal_sidecar_with_it() {
    // Leaving `-wal` beside a fresh file is how a "rebuilt" cache comes back
    // with the contents it was supposed to have dropped.
    let tmp = TempDir::new("sidecar");
    let db = tmp.db();
    {
        let cache = Cache::open(&db, "ShaxP").unwrap();
        cache
            .put_notifications(&[notification("1", true, 1)])
            .unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
    }
    let cache = Cache::open(&db, "ShaxP").unwrap();
    assert!(
        cache
            .notifications(&NotificationQuery::default())
            .unwrap()
            .is_empty()
    );
    assert!(tmp.path().exists());
}

// ------------------------------------------ entities, lists, kv (§3)

#[test]
fn list_membership_is_stored_apart_from_entities() {
    // One PR updating must not invalidate every list it appears in, and
    // re-fetching a list must not rewrite bodies that did not change.
    let cache = Cache::in_memory("ShaxP").unwrap();
    let id = NodeId("PR_1".into());

    cache
        .put_entity(
            EntityKind::PullRequest,
            &id,
            &"first body",
            &Validators::etag("v1"),
            NOW - Duration::minutes(10),
        )
        .unwrap();
    cache
        .put_list(
            "needs-review",
            std::slice::from_ref(&id),
            &ListMeta::complete_at(NOW),
        )
        .unwrap();
    cache
        .put_list(
            "mine",
            std::slice::from_ref(&id),
            &ListMeta::complete_at(NOW),
        )
        .unwrap();

    // The PR changes.
    cache
        .put_entity(
            EntityKind::PullRequest,
            &id,
            &"second body",
            &Validators::etag("v2"),
            NOW,
        )
        .unwrap();

    // Both lists still hold it, with their own freshness untouched.
    assert_eq!(cache.list("needs-review").unwrap(), vec![id.clone()]);
    assert_eq!(cache.list("mine").unwrap(), vec![id.clone()]);
    let stored: Stored<String> = cache.entity(&id).unwrap().unwrap();
    assert_eq!(stored.value, "second body");
    assert_eq!(stored.validators.etag.as_deref(), Some("v2"));

    // And re-fetching one list leaves the other alone.
    cache
        .put_list("mine", &[], &ListMeta::complete_at(NOW))
        .unwrap();
    assert_eq!(cache.list("needs-review").unwrap(), vec![id]);
    assert!(cache.list("mine").unwrap().is_empty());
}

#[test]
fn a_list_keeps_the_order_it_was_given() {
    let cache = Cache::in_memory("ShaxP").unwrap();
    let ids: Vec<NodeId> = (0..5).map(|i| NodeId(format!("PR_{i}"))).collect();
    cache
        .put_list("k", &ids, &ListMeta::complete_at(NOW))
        .unwrap();
    assert_eq!(cache.list("k").unwrap(), ids);
}

#[test]
fn both_validators_are_stored_beside_the_data() {
    // §4: ETags on REST, Last-Modified on notifications. A 304 costs no rate
    // limit, so the validator is what makes aggressive polling cheap — and it
    // is useless if we cannot read it back to send it.
    let cache = Cache::in_memory("ShaxP").unwrap();
    let v = Validators {
        etag: Some("W/\"deadbeef\"".into()),
        last_modified: Some("Thu, 11 Sep 2026 12:00:00 GMT".into()),
    };
    let id = NodeId("PR_1".into());
    cache
        .put_entity(EntityKind::PullRequest, &id, &1u8, &v, NOW)
        .unwrap();
    let back: Stored<u8> = cache.entity(&id).unwrap().unwrap();
    assert_eq!(back.validators, v);
    assert_eq!(back.fetched_at, NOW);

    let meta = ListMeta {
        validators: v.clone(),
        cursor: Some("Y3Vyc29yOjE=".into()),
        complete: false,
        fetched_at: NOW,
    };
    cache.put_list("k", &[], &meta).unwrap();
    assert_eq!(cache.list_meta("k").unwrap(), Some(meta));
}

#[test]
fn a_304_refreshes_the_clock_without_rewriting_the_list() {
    let cache = Cache::in_memory("ShaxP").unwrap();
    let ids = vec![NodeId("PR_1".into())];
    cache
        .put_list(
            "k",
            &ids,
            &ListMeta {
                validators: Validators::etag("v1"),
                cursor: None,
                complete: true,
                fetched_at: NOW - Duration::hours(1),
            },
        )
        .unwrap();

    cache.touch_list("k", NOW).unwrap();

    let meta = cache.list_meta("k").unwrap().unwrap();
    assert_eq!(meta.fetched_at, NOW);
    assert_eq!(meta.validators.etag.as_deref(), Some("v1"));
    assert_eq!(cache.list("k").unwrap(), ids, "membership is untouched");
}

#[test]
fn never_stored_is_distinguishable_from_stored_and_empty() {
    let cache = Cache::in_memory("ShaxP").unwrap();
    assert!(cache.list_meta("k").unwrap().is_none());
    cache
        .put_list("k", &[], &ListMeta::complete_at(NOW))
        .unwrap();
    assert!(cache.list_meta("k").unwrap().is_some());
    assert!(cache.list("k").unwrap().is_empty());
}

#[test]
fn forgetting_an_entity_leaves_nothing_behind() {
    let cache = Cache::in_memory("ShaxP").unwrap();
    let id = NodeId("PR_1".into());
    cache
        .put_entity(EntityKind::Issue, &id, &"x", &Validators::default(), NOW)
        .unwrap();
    assert!(cache.forget_entity(&id).unwrap());
    assert!(cache.entity::<String>(&id).unwrap().is_none());
    assert!(!cache.forget_entity(&id).unwrap(), "and it is idempotent");
}

#[test]
fn the_poll_interval_survives_a_restart() {
    let tmp = TempDir::new("poll");
    {
        let cache = Cache::open(tmp.db(), "ShaxP").unwrap();
        assert_eq!(cache.poll_interval().unwrap(), None);
        cache.set_poll_interval(Duration::seconds(120)).unwrap();
    }
    let cache = Cache::open(tmp.db(), "ShaxP").unwrap();
    assert_eq!(cache.poll_interval().unwrap(), Some(Duration::seconds(120)));
}

// ------------------------------------------------------ notifications (§3)

#[test]
fn the_inbox_comes_back_newest_first() {
    let cache = Cache::in_memory("ShaxP").unwrap();
    cache
        .put_notifications(&[
            notification("old", true, 500),
            notification("new", true, 1),
            notification("mid", true, 60),
        ])
        .unwrap();
    let ids: Vec<String> = cache
        .notifications(&NotificationQuery::default())
        .unwrap()
        .into_iter()
        .map(|n| n.id.0)
        .collect();
    assert_eq!(ids, ["new", "mid", "old"]);
}

#[test]
fn filters_narrow_the_inbox() {
    let cache = Cache::in_memory("ShaxP").unwrap();
    let mut mention = notification("m", true, 2);
    mention.reason = NotificationReason::Mention;
    mention.kind = SubjectKind::Issue;
    mention.repo = RepoRef::new("basecamp", "omarchy");
    mention.title = "Bar widget plugins".into();

    cache
        .put_notifications(&[
            notification("a", true, 1),
            notification("b", false, 3),
            mention,
        ])
        .unwrap();

    let unread = cache.notifications(&NotificationQuery::unread()).unwrap();
    assert_eq!(unread.len(), 2);

    let by_repo = cache
        .notifications(&NotificationQuery {
            repo: RepoRef::parse("basecamp/omarchy"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_repo.len(), 1);

    let by_reason = cache
        .notifications(&NotificationQuery {
            reasons: vec![NotificationReason::Mention],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_reason.len(), 1);

    let by_kind = cache
        .notifications(&NotificationQuery {
            kinds: vec![SubjectKind::Issue],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_kind.len(), 1);

    let by_text = cache
        .notifications(&NotificationQuery {
            search: Some("BAR WIDGET".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_text.len(), 1, "search is case-insensitive");
}

#[test]
fn re_fetching_updates_a_row_rather_than_duplicating_it() {
    let cache = Cache::in_memory("ShaxP").unwrap();
    cache
        .put_notifications(&[notification("1", true, 10)])
        .unwrap();
    let mut updated = notification("1", true, 1);
    updated.title = "retitled".into();
    cache.put_notifications(&[updated]).unwrap();

    let rows = cache.notifications(&NotificationQuery::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "retitled");
}

#[test]
fn the_read_column_wins_over_a_stale_body() {
    // The body was serialized before the mark-read; if the body won, the row
    // would come back unread and the keypress would appear to have been
    // ignored.
    let cache = Cache::in_memory("ShaxP").unwrap();
    cache
        .put_notifications(&[notification("1", true, 1)])
        .unwrap();
    cache
        .set_unread(&[NotificationId("1".into())], false)
        .unwrap();
    let rows = cache.notifications(&NotificationQuery::default()).unwrap();
    assert!(!rows[0].unread);
}

#[test]
fn only_unrequested_enrichment_is_offered_for_fetching() {
    // `Failed` is terminal and `Pending` is in flight. Retrying either on
    // every open is how a repo you lost access to burns the rate limit.
    let cache = Cache::in_memory("ShaxP").unwrap();
    let mut rows = vec![
        notification("absent", true, 1),
        notification("pending", true, 2),
        notification("failed", true, 3),
        notification("ready", true, 4),
    ];
    rows[1].detail = Enrichment::Pending;
    rows[2].detail = Enrichment::Failed {
        reason: "403".into(),
    };
    rows[3].detail = Enrichment::Ready(subject_detail());
    cache.put_notifications(&rows).unwrap();

    let want = cache.notifications_wanting_enrichment(50).unwrap();
    assert_eq!(want, vec![NotificationId("absent".into())]);
}

// ------------------------------------------------- freshness (§1, §4)

#[tokio::test]
async fn a_cold_cache_is_never_fetched_not_empty() {
    let s = store("ShaxP");
    let got = s
        .notifications(&NotificationQuery::default())
        .await
        .unwrap();
    assert!(got.value.is_empty());
    assert_eq!(got.fetched_at, None, "never fetched");
    assert!(got.stale);
    assert_eq!(got.source, Source::Cache);
}

#[tokio::test]
async fn fetched_and_empty_is_not_a_cold_cache() {
    // "All caught up" and "we have never looked" are different screens.
    let s = store("ShaxP");
    s.with_cache(|c| c.put_list(INBOX, &[], &ListMeta::complete_at(NOW)))
        .unwrap();
    let got = s
        .notifications(&NotificationQuery::default())
        .await
        .unwrap();
    assert!(got.value.is_empty());
    assert_eq!(got.fetched_at, Some(NOW));
    assert!(!got.stale);
}

#[tokio::test]
async fn notification_staleness_follows_the_poll_interval_floored_at_a_minute() {
    let s = store("ShaxP");
    s.with_cache(|c| {
        c.put_notifications(&[notification("1", true, 1)])?;
        c.put_list(
            INBOX,
            &[],
            &ListMeta::complete_at(NOW - Duration::seconds(90)),
        )
    })
    .unwrap();

    // No header seen yet: the 60s floor applies, so 90s old is stale.
    let got = s
        .notifications(&NotificationQuery::default())
        .await
        .unwrap();
    assert!(got.stale);
    assert!(!got.value.is_empty(), "stale data is never hidden");

    // GitHub says poll every five minutes: obey it, and 90s is fresh.
    s.with_cache(|c| c.set_poll_interval(Duration::seconds(300)))
        .unwrap();
    let got = s
        .notifications(&NotificationQuery::default())
        .await
        .unwrap();
    assert!(!got.stale);
    assert_eq!(
        ttl::notifications(Some(Duration::seconds(300))),
        Duration::seconds(300)
    );
}

#[tokio::test]
async fn the_dashboard_is_only_as_fresh_as_its_stalest_section() {
    let cfg = DashboardConfig {
        sections: vec![
            DashboardSection {
                title: "Needs my review".into(),
                query: "is:open is:pr review-requested:@me".into(),
                limit: 10,
            },
            DashboardSection {
                title: "My pull requests".into(),
                query: "is:open is:pr author:@me".into(),
                limit: 10,
            },
        ],
    };
    let s = store("ShaxP");

    // Nothing fetched at all: never, not empty.
    let got = s.dashboard(&cfg).await.unwrap();
    assert_eq!(got.fetched_at, None);
    assert!(got.value.sections.iter().all(|s| s.count == 0));
    assert_eq!(got.value.sections[0].title, "Needs my review");

    // One section fresh, the other never fetched — the screen is stale, since
    // the zero it is showing for the second section is a claim, not a gap.
    s.with_cache(|c| {
        c.put_list(
            &dashboard_list_key(&cfg.sections[0].query),
            &[NodeId("PR_1".into()), NodeId("PR_2".into())],
            &ListMeta::complete_at(NOW),
        )
    })
    .unwrap();
    let got = s.dashboard(&cfg).await.unwrap();
    assert_eq!(got.value.sections[0].count, 2);
    assert!(got.stale);

    // Both fetched, one an hour ago: still stale, at the older time.
    s.with_cache(|c| {
        c.put_list(
            &dashboard_list_key(&cfg.sections[1].query),
            &[NodeId("PR_3".into())],
            &ListMeta::complete_at(NOW - Duration::hours(1)),
        )
    })
    .unwrap();
    let got = s.dashboard(&cfg).await.unwrap();
    assert_eq!(got.fetched_at, Some(NOW - Duration::hours(1)));
    assert!(got.stale);

    // Both recent: fresh.
    s.with_cache(|c| {
        c.put_list(
            &dashboard_list_key(&cfg.sections[1].query),
            &[NodeId("PR_3".into())],
            &ListMeta::complete_at(NOW - Duration::minutes(1)),
        )
    })
    .unwrap();
    let got = s.dashboard(&cfg).await.unwrap();
    assert!(!got.stale);
    assert_eq!(got.value.sections[1].count, 1);
}

#[tokio::test]
async fn a_read_reports_a_refresh_in_flight_without_hiding_content() {
    let s = store("ShaxP");
    s.with_cache(|c| {
        c.put_notifications(&[notification("1", true, 1)])?;
        c.put_list(INBOX, &[], &ListMeta::complete_at(NOW))
    })
    .unwrap();

    s.refresh(RefreshTarget::Notifications);
    let got = s
        .notifications(&NotificationQuery::default())
        .await
        .unwrap();
    assert!(got.refreshing);
    assert_eq!(got.value.len(), 1, "content is not replaced by a spinner");
    assert!(got.needs_provenance_note());

    s.refresh_finished(RefreshTarget::Notifications);
    let got = s
        .notifications(&NotificationQuery::default())
        .await
        .unwrap();
    assert!(!got.refreshing);
}

#[tokio::test]
async fn the_limit_narrows_the_page_but_not_the_total() {
    // "12 of 340 unread" is a different sentence from "12 unread".
    let s = store("ShaxP");
    let rows: Vec<Notification> = (0..10)
        .map(|i| notification(&format!("{i}"), true, i))
        .collect();
    s.with_cache(|c| c.put_notifications(&rows)).unwrap();

    let got = s
        .notifications(&NotificationQuery {
            limit: Some(3),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(got.value.len(), 3);
    assert_eq!(got.value.total, Some(10));
}

// --------------------------------------------------------- refresh (§1, §6)

#[tokio::test]
async fn refresh_schedules_and_returns_without_fetching() {
    let remote = Arc::new(RecordIntent::new());
    let s = store("ShaxP").with_remote(remote.clone());
    let mut rx = s.subscribe();

    s.refresh(RefreshTarget::Notifications);
    s.refresh(RefreshTarget::Dashboard);

    assert_eq!(
        rx.recv().await.unwrap(),
        StoreEvent::RefreshStarted(RefreshTarget::Notifications)
    );
    assert_eq!(
        rx.recv().await.unwrap(),
        StoreEvent::RefreshStarted(RefreshTarget::Dashboard)
    );
    assert_eq!(remote.scheduled().len(), 2);

    s.cancel(&RefreshTarget::Dashboard);
    assert_eq!(remote.scheduled(), vec![RefreshTarget::Notifications]);
}

#[tokio::test]
async fn the_same_target_twice_is_one_request() {
    // §6: the same target requested twice while in flight is one request with
    // two waiters — and, on screen, one spinner rather than a flicker.
    let remote = Arc::new(RecordIntent::new());
    let s = store("ShaxP").with_remote(remote.clone());
    s.refresh(RefreshTarget::Notifications);
    s.refresh(RefreshTarget::Notifications);
    assert_eq!(remote.scheduled().len(), 1);

    s.refresh_finished(RefreshTarget::Notifications);
    s.refresh(RefreshTarget::Notifications);
    assert_eq!(remote.scheduled().len(), 2, "and it can be asked again");
}

#[tokio::test]
async fn a_failed_refresh_clears_the_flight_and_names_the_target() {
    let s = store("ShaxP");
    let mut rx = s.subscribe();
    s.refresh(RefreshTarget::Dashboard);
    let _ = rx.recv().await.unwrap();

    s.refresh_failed(RefreshTarget::Dashboard, StoreError::Offline("dns".into()));
    match rx.recv().await.unwrap() {
        StoreEvent::RefreshFailed { target, error } => {
            assert_eq!(target, RefreshTarget::Dashboard);
            assert!(error.keeps_cached_content(), "offline keeps the cache");
        }
        other => panic!("expected RefreshFailed, got {other:?}"),
    }
    assert!(s.in_flight().is_empty());
}

#[tokio::test]
async fn events_name_what_changed_and_carry_none_of_it() {
    // Carrying payloads means two paths into UI state and they diverge.
    let s = store("ShaxP");
    s.with_cache(|c| c.put_notifications(&[notification("1", true, 1)]))
        .unwrap();
    let mut rx = s.subscribe();
    s.mark_read(&[NotificationId("1".into())]).await.unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        StoreEvent::Updated(RefreshTarget::Notifications)
    );
}

// ------------------------------------------------------- mutations (§5)

#[tokio::test]
async fn a_mark_read_lands_locally_before_anything_is_sent() {
    let s = store("ShaxP");
    s.with_cache(|c| c.put_notifications(&[notification("1", true, 1)]))
        .unwrap();

    s.mark_read(&[NotificationId("1".into())]).await.unwrap();
    assert!(
        s.notifications(&NotificationQuery::unread())
            .await
            .unwrap()
            .value
            .is_empty()
    );

    s.mark_unread(&[NotificationId("1".into())]).await.unwrap();
    assert_eq!(
        s.notifications(&NotificationQuery::unread())
            .await
            .unwrap()
            .value
            .len(),
        1
    );
}

#[tokio::test]
async fn marking_an_already_read_thread_is_not_an_error() {
    let s = store("ShaxP");
    s.with_cache(|c| c.put_notifications(&[notification("1", false, 1)]))
        .unwrap();
    s.mark_read(&[NotificationId("1".into())]).await.unwrap();
    s.mark_read(&[NotificationId("1".into())]).await.unwrap();
    // An id we have never seen is also not an error.
    s.mark_read(&[NotificationId("nope".into())]).await.unwrap();
}

#[tokio::test]
async fn a_failed_mutation_rolls_back_to_the_value_captured_first() {
    let s = store("ShaxP").with_remote(Arc::new(FailingRemote(StoreError::Forbidden)));
    s.with_cache(|c| {
        c.put_notifications(&[
            notification("unread-1", true, 1),
            notification("already-read", false, 2),
            notification("unread-2", true, 3),
        ])
    })
    .unwrap();

    let mut rx = s.subscribe();
    let ids = [
        NotificationId("unread-1".into()),
        NotificationId("already-read".into()),
        NotificationId("unread-2".into()),
    ];
    let err = s.mark_read(&ids).await.expect_err("the remote refused");
    assert_eq!(err, StoreError::Forbidden);

    // Two Updated events: the optimistic write, then the rollback. Both are
    // needed — the UI painted the optimistic state and must be told to repaint.
    assert_eq!(
        rx.recv().await.unwrap(),
        StoreEvent::Updated(RefreshTarget::Notifications)
    );
    assert_eq!(
        rx.recv().await.unwrap(),
        StoreEvent::Updated(RefreshTarget::Notifications)
    );

    // Rollback restores per-id state. Setting them all back to `unread` would
    // resurrect `already-read`, which the user did not touch.
    let rows = s
        .notifications(&NotificationQuery::default())
        .await
        .unwrap()
        .value;
    let state: Vec<(String, bool)> = rows.items.into_iter().map(|n| (n.id.0, n.unread)).collect();
    assert!(state.contains(&("unread-1".into(), true)));
    assert!(state.contains(&("unread-2".into(), true)));
    assert!(
        state.contains(&("already-read".into(), false)),
        "a row that was already read must stay read: {state:?}"
    );
}

#[tokio::test]
async fn a_mutation_over_no_ids_does_nothing_at_all() {
    let s = store("ShaxP").with_remote(Arc::new(FailingRemote(StoreError::Forbidden)));
    let mut rx = s.subscribe();
    s.mark_read(&[]).await.unwrap();
    assert!(
        rx.try_recv().is_err(),
        "an empty selection is not a change to announce"
    );
}

#[tokio::test]
async fn a_rejected_token_still_leaves_the_cache_readable() {
    // §7: Auth loss is a banner over cached content, not an empty screen.
    let s = store("ShaxP").with_remote(Arc::new(FailingRemote(StoreError::Auth(
        AuthError::Rejected,
    ))));
    s.with_cache(|c| c.put_notifications(&[notification("1", true, 1)]))
        .unwrap();
    let err = s
        .mark_read(&[NotificationId("1".into())])
        .await
        .expect_err("the token was rejected");
    assert!(err.keeps_cached_content());
    assert_eq!(
        s.notifications(&NotificationQuery::default())
            .await
            .unwrap()
            .value
            .len(),
        1
    );
}

// ------------------------------------------------ model round-trips (§3)

/// Store a value as an entity body and read it back. Bodies are serialized
/// `omaghy-model` types, so this is also the first real exercise of the
/// model's serde attributes — nothing before this ever wrote one to bytes.
fn roundtrip<T>(cache: &Cache, id: &str, value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let node = NodeId(id.into());
    cache
        .put_entity(
            EntityKind::Other("roundtrip"),
            &node,
            value,
            &Validators::default(),
            NOW,
        )
        .unwrap();
    let back: Stored<T> = cache
        .entity(&node)
        .unwrap()
        .unwrap_or_else(|| panic!("{id} was not stored"));
    assert_eq!(&back.value, value, "{id} did not survive storage");
}

fn actor() -> Actor {
    Actor {
        login: "ShaxP".into(),
        node_id: Some(NodeId("U_1".into())),
        avatar_url: Some("https://avatars.githubusercontent.com/u/1".into()),
        is_bot: false,
    }
}

fn label() -> Label {
    Label {
        name: "bug".into(),
        color: Rgb::parse_hex("#d73a4a").unwrap(),
        description: Some("Something is not working".into()),
    }
}

fn rollup() -> CheckRollup {
    CheckRollup::merge(
        vec![CheckRun {
            name: "build".into(),
            status: CheckStatus::Completed,
            conclusion: Some(CheckConclusion::Success),
            url: Some("https://github.com/ShaxP/omaghy/runs/1".into()),
        }],
        &[CommitStatus {
            context: "ci/legacy".into(),
            state: StatusState::Pending,
            url: None,
        }],
    )
}

fn subject_detail() -> SubjectDetail {
    SubjectDetail {
        number: Some(61),
        status: SubjectStatus::PullRequest(PrDisplayStatus::Merged),
        checks: rollup(),
        last_actor: Some(actor()),
        html_url: "https://github.com/ShaxP/shax/pull/61".into(),
    }
}

fn markdown() -> Markdown {
    Markdown {
        source: "# Title\n\nBody with `code`.".into(),
        blocks: vec![
            Block::Heading {
                level: 1,
                spans: vec![StyledSpan {
                    text: "Title".into(),
                    style: SpanStyle::Plain,
                }],
            },
            Block::Paragraph(vec![
                StyledSpan {
                    text: "Body with ".into(),
                    style: SpanStyle::Emphasis,
                },
                StyledSpan {
                    text: "code".into(),
                    style: SpanStyle::Code,
                },
                StyledSpan {
                    text: "@ShaxP".into(),
                    style: SpanStyle::Mention,
                },
                StyledSpan {
                    text: "#61".into(),
                    style: SpanStyle::Reference,
                },
                StyledSpan {
                    text: "here".into(),
                    style: SpanStyle::Link,
                },
                StyledSpan {
                    text: "bold".into(),
                    style: SpanStyle::Strong,
                },
            ]),
            Block::Code {
                language: Some("rust".into()),
                lines: vec![vec![
                    StyledSpan {
                        text: "fn".into(),
                        style: SpanStyle::Token(omaghy_model::content::SyntaxToken::Keyword),
                    },
                    StyledSpan {
                        text: "main".into(),
                        style: SpanStyle::Token(omaghy_model::content::SyntaxToken::Function),
                    },
                    StyledSpan {
                        text: "()".into(),
                        style: SpanStyle::Token(omaghy_model::content::SyntaxToken::Punctuation),
                    },
                    StyledSpan {
                        text: "\"s\"".into(),
                        style: SpanStyle::Token(omaghy_model::content::SyntaxToken::String),
                    },
                    StyledSpan {
                        text: "1".into(),
                        style: SpanStyle::Token(omaghy_model::content::SyntaxToken::Number),
                    },
                    StyledSpan {
                        text: "// c".into(),
                        style: SpanStyle::Token(omaghy_model::content::SyntaxToken::Comment),
                    },
                    StyledSpan {
                        text: "u8".into(),
                        style: SpanStyle::Token(omaghy_model::content::SyntaxToken::Type),
                    },
                ]],
            },
            Block::Quote(vec![Block::Rule]),
            Block::List {
                ordered: true,
                items: vec![vec![Block::Rule]],
            },
            Block::Task {
                checked: true,
                spans: vec![],
            },
            Block::Table {
                header: vec![vec![]],
                rows: vec![vec![vec![]]],
            },
            Block::Image {
                alt: "a screenshot".into(),
                url: "https://example.invalid/s.png".into(),
            },
        ],
    }
}

fn timeline_kinds() -> Vec<TimelineKind> {
    use omaghy_model::timeline::{ReviewThread, ThreadComment};
    let thread = ReviewThread {
        path: "src/lib.rs".into(),
        line: Some(12),
        is_resolved: false,
        is_outdated: true,
        comments: vec![ThreadComment {
            author: Some(actor()),
            body: markdown(),
            at: NOW,
        }],
    };
    vec![
        TimelineKind::Comment {
            body: markdown(),
            reactions: Reactions {
                thumbs_up: 3,
                ..Default::default()
            },
            edited: true,
        },
        TimelineKind::Review {
            state: ReviewState::ChangesRequested,
            body: Some(markdown()),
            threads: vec![thread.clone()],
        },
        TimelineKind::ReviewThread(thread),
        TimelineKind::Commit {
            oid: "9e1f2a3".into(),
            message_headline: "fix: the thing".into(),
            authored_by: None,
        },
        TimelineKind::Merged {
            commit: Some("abc".into()),
            base: "main".into(),
        },
        TimelineKind::Closed {
            by_commit: Some("def".into()),
        },
        TimelineKind::Reopened,
        TimelineKind::ReadyForReview,
        TimelineKind::ConvertedToDraft,
        TimelineKind::Renamed {
            from: "a".into(),
            to: "b".into(),
        },
        TimelineKind::Labeled { label: label() },
        TimelineKind::Unlabeled { label: label() },
        TimelineKind::Assigned { who: actor() },
        TimelineKind::Unassigned { who: actor() },
        TimelineKind::ReviewRequested { who: actor() },
        TimelineKind::ReviewRequestRemoved { who: actor() },
        TimelineKind::CrossReferenced {
            source: SubjectRef::from_api_url("https://api.github.com/repos/o/r/issues/7").unwrap(),
            will_close: true,
        },
        TimelineKind::HeadRefForcePushed {
            before: "aaa".into(),
            after: "bbb".into(),
        },
        TimelineKind::Other {
            kind: "PinnedEvent".into(),
        },
    ]
}

/// Adding a `TimelineKind` variant must break this, so the round-trip above
/// cannot quietly stop covering everything.
#[allow(dead_code)]
fn every_timeline_kind_is_listed(k: &TimelineKind) {
    match k {
        TimelineKind::Comment { .. }
        | TimelineKind::Review { .. }
        | TimelineKind::ReviewThread(_)
        | TimelineKind::Commit { .. }
        | TimelineKind::Merged { .. }
        | TimelineKind::Closed { .. }
        | TimelineKind::Reopened
        | TimelineKind::ReadyForReview
        | TimelineKind::ConvertedToDraft
        | TimelineKind::Renamed { .. }
        | TimelineKind::Labeled { .. }
        | TimelineKind::Unlabeled { .. }
        | TimelineKind::Assigned { .. }
        | TimelineKind::Unassigned { .. }
        | TimelineKind::ReviewRequested { .. }
        | TimelineKind::ReviewRequestRemoved { .. }
        | TimelineKind::CrossReferenced { .. }
        | TimelineKind::HeadRefForcePushed { .. }
        | TimelineKind::Other { .. } => {}
    }
}

#[test]
fn every_model_type_survives_a_round_trip_through_storage() {
    let cache = Cache::in_memory("ShaxP").unwrap();

    // Identity.
    roundtrip(&cache, "node-id", &NodeId("PR_kwDOA".into()));
    roundtrip(
        &cache,
        "notification-id",
        &NotificationId("24555446001".into()),
    );
    roundtrip(
        &cache,
        "subject-kinds",
        &vec![
            SubjectKind::PullRequest,
            SubjectKind::Issue,
            SubjectKind::Discussion,
            SubjectKind::Release,
            SubjectKind::CheckSuite,
            SubjectKind::Commit,
            SubjectKind::VulnerabilityAlert,
            SubjectKind::Other("Sponsorship".into()),
        ],
    );
    roundtrip(
        &cache,
        "subject-ids",
        &vec![SubjectId::Number(61), SubjectId::Sha("9e1f2a3".into())],
    );
    roundtrip(
        &cache,
        "subject-ref",
        &SubjectRef::from_api_url("https://api.github.com/repos/ShaxP/shax/pulls/61").unwrap(),
    );

    // People, repos, labels.
    roundtrip(&cache, "actor", &actor());
    roundtrip(
        &cache,
        "ghost-actor",
        &Actor {
            login: "ghost".into(),
            node_id: None,
            avatar_url: None,
            is_bot: true,
        },
    );
    roundtrip(&cache, "repo-ref", &RepoRef::new("ShaxP", "omaghy"));
    roundtrip(
        &cache,
        "repo",
        &Repo {
            node_id: NodeId("R_1".into()),
            r#ref: RepoRef::new("ShaxP", "omaghy"),
            is_private: true,
            description: None,
            default_branch: Some("main".into()),
        },
    );
    roundtrip(&cache, "label", &label());
    roundtrip(&cache, "rgb", &Rgb::parse_hex("ffffff").unwrap());

    // CI.
    roundtrip(&cache, "rollup", &rollup());
    roundtrip(&cache, "empty-rollup", &CheckRollup::empty());
    roundtrip(
        &cache,
        "check-statuses",
        &vec![
            CheckStatus::Requested,
            CheckStatus::Queued,
            CheckStatus::InProgress,
            CheckStatus::Completed,
            CheckStatus::Waiting,
            CheckStatus::Pending,
        ],
    );
    roundtrip(
        &cache,
        "check-conclusions",
        &vec![
            CheckConclusion::ActionRequired,
            CheckConclusion::TimedOut,
            CheckConclusion::Cancelled,
            CheckConclusion::Failure,
            CheckConclusion::Success,
            CheckConclusion::Neutral,
            CheckConclusion::Skipped,
            CheckConclusion::StartupFailure,
            CheckConclusion::Stale,
        ],
    );
    roundtrip(
        &cache,
        "status-states",
        &vec![
            StatusState::Expected,
            StatusState::Error,
            StatusState::Failure,
            StatusState::Pending,
            StatusState::Success,
        ],
    );
    roundtrip(
        &cache,
        "rollup-states",
        &vec![
            RollupState::Success,
            RollupState::Failure,
            RollupState::Pending,
            RollupState::Neutral,
            RollupState::None,
        ],
    );

    // Review.
    roundtrip(
        &cache,
        "review-summary",
        &ReviewSummary {
            decision: Some(ReviewDecision::ChangesRequested),
            reviewers: vec![(actor(), ReviewState::Approved)],
            i_am_requested: true,
            my_review: Some(ReviewState::Dismissed),
        },
    );
    roundtrip(
        &cache,
        "review-states",
        &vec![
            ReviewState::Pending,
            ReviewState::Commented,
            ReviewState::Approved,
            ReviewState::ChangesRequested,
            ReviewState::Dismissed,
        ],
    );
    roundtrip(
        &cache,
        "review-decisions",
        &vec![
            ReviewDecision::Approved,
            ReviewDecision::ChangesRequested,
            ReviewDecision::ReviewRequired,
        ],
    );

    // Pull requests and issues.
    roundtrip(
        &cache,
        "pull-request",
        &PullRequest {
            node_id: NodeId("PR_1".into()),
            number: 61,
            repo: RepoRef::new("ShaxP", "shax"),
            title: "fix: the thing".into(),
            author: Some(actor()),
            state: PrState::Open,
            is_draft: true,
            mergeable: Mergeable::Conflicting,
            labels: vec![label()],
            review: ReviewSummary::default(),
            checks: rollup(),
            comment_count: 4,
            additions: 120,
            deletions: 8,
            changed_files: 3,
            created_at: NOW - Duration::days(2),
            updated_at: NOW,
        },
    );
    roundtrip(
        &cache,
        "pr-states",
        &vec![PrState::Open, PrState::Closed, PrState::Merged],
    );
    roundtrip(
        &cache,
        "mergeable",
        &vec![
            Mergeable::Mergeable,
            Mergeable::Conflicting,
            Mergeable::Unknown,
        ],
    );
    roundtrip(
        &cache,
        "pr-display-statuses",
        &vec![
            PrDisplayStatus::Draft,
            PrDisplayStatus::Open,
            PrDisplayStatus::Merged,
            PrDisplayStatus::Closed,
        ],
    );
    roundtrip(
        &cache,
        "issue",
        &Issue {
            node_id: NodeId("I_1".into()),
            number: 7,
            repo: RepoRef::new("basecamp", "omarchy"),
            title: "Theme switcher should preview".into(),
            author: None,
            state: IssueState::Closed,
            state_reason: Some(IssueStateReason::NotPlanned),
            labels: vec![label()],
            assignees: vec![actor()],
            comment_count: 0,
            created_at: NOW - Duration::days(30),
            updated_at: NOW,
        },
    );
    roundtrip(
        &cache,
        "issue-state-reasons",
        &vec![
            IssueStateReason::Completed,
            IssueStateReason::NotPlanned,
            IssueStateReason::Reopened,
            IssueStateReason::Duplicate,
        ],
    );

    // Content and timeline.
    roundtrip(&cache, "markdown", &markdown());
    roundtrip(&cache, "empty-markdown", &Markdown::from_source(""));
    roundtrip(
        &cache,
        "timeline",
        &timeline_kinds()
            .into_iter()
            .map(|kind| TimelineEvent {
                node_id: Some(NodeId("E_1".into())),
                actor: Some(actor()),
                at: NOW,
                kind,
            })
            .collect::<Vec<_>>(),
    );

    // Notifications, and every enrichment state.
    roundtrip(
        &cache,
        "notification-reasons",
        &vec![
            NotificationReason::ReviewRequested,
            NotificationReason::Mention,
            NotificationReason::TeamMention,
            NotificationReason::Assign,
            NotificationReason::Author,
            NotificationReason::Comment,
            NotificationReason::StateChange,
            NotificationReason::CiActivity,
            NotificationReason::Subscribed,
            NotificationReason::Manual,
            NotificationReason::Invitation,
            NotificationReason::SecurityAlert,
            NotificationReason::Other("new_thing".into()),
        ],
    );
    roundtrip(&cache, "subject-detail", &subject_detail());
    roundtrip(
        &cache,
        "enrichments",
        &vec![
            Enrichment::<SubjectDetail>::Absent,
            Enrichment::Pending,
            Enrichment::Failed {
                reason: "403".into(),
            },
            Enrichment::Ready(subject_detail()),
        ],
    );
    let mut enriched = notification("1", true, 3);
    enriched.detail = Enrichment::Ready(subject_detail());
    roundtrip(&cache, "notification", &enriched);
    let mut unaddressable = notification("2", false, 3);
    unaddressable.subject = None;
    roundtrip(&cache, "unaddressable-notification", &unaddressable);

    // The store's own wire types, which travel through `kv` and list cursors.
    roundtrip(
        &cache,
        "page",
        &Page {
            items: vec![enriched],
            cursor: Some("Y3Vyc29yOjE=".into()),
            total: Some(340),
        },
    );
    roundtrip(&cache, "dashboard-config", &DashboardConfig::default());
    roundtrip(
        &cache,
        "notification-query",
        &NotificationQuery {
            read: ReadFilter::UnreadOnly,
            repo: RepoRef::parse("ShaxP/omaghy"),
            reasons: vec![NotificationReason::Mention],
            kinds: vec![SubjectKind::Issue],
            search: Some("theme".into()),
            limit: Some(50),
        },
    );
    roundtrip(&cache, "source", &vec![Source::Cache, Source::Network]);
}

#[test]
fn a_notification_keeps_its_timestamp_to_the_second() {
    // The inbox is sorted on a column derived from this, so a round-trip that
    // rounded would reorder the screen.
    let cache = Cache::in_memory("ShaxP").unwrap();
    let n = notification("1", true, 0);
    cache.put_notifications(std::slice::from_ref(&n)).unwrap();
    let back = cache
        .notifications(&NotificationQuery::default())
        .unwrap()
        .remove(0);
    assert_eq!(back.updated_at, n.updated_at);
    assert_eq!(back, n);
}

#[test]
fn a_body_that_will_not_deserialize_is_corruption_not_a_missing_row() {
    // Skipping the row would render a shorter list that looks correct. The
    // schema version exists so this cannot happen in practice; if it does, the
    // honest answer is that the cache is not readable.
    let cache = Cache::in_memory("ShaxP").unwrap();
    let id = NodeId("PR_1".into());
    cache
        .put_entity(
            EntityKind::PullRequest,
            &id,
            &"a string",
            &Validators::default(),
            NOW,
        )
        .unwrap();
    let err = cache
        .entity::<PullRequest>(&id)
        .expect_err("a string is not a PullRequest");
    assert!(matches!(err, CacheError::Corrupt(_)), "got {err:?}");
}

// ------------------------------------------------------------- plumbing

#[tokio::test]
async fn the_store_is_usable_through_the_trait_object_the_tui_sees() {
    // The TUI holds `Arc<dyn Store>` and must not be able to tell this from
    // `FakeStore`.
    let s: Arc<dyn Store> = Arc::new(store("ShaxP"));
    assert_eq!(s.viewer(), &Viewer::new("ShaxP"));
    let _: Fresh<Page<Notification>> = s
        .notifications(&NotificationQuery::default())
        .await
        .unwrap();
    let _ = s.subscribe();
    s.refresh(RefreshTarget::Notifications);
    s.cancel(&RefreshTarget::Notifications);
}

#[test]
fn the_notification_list_key_is_stable() {
    // `omaghy-api` writes the inbox's freshness under this key and the store
    // reads it; a typo on either side is a permanently cold cache.
    assert_eq!(NOTIFICATIONS_LIST, "notifications");
    assert_eq!(INBOX, NOTIFICATIONS_LIST);
}

#[test]
fn dashboard_sections_are_keyed_by_query_not_title() {
    // Renaming a section in config must not throw away its rows.
    let a = dashboard_list_key("is:open is:pr review-requested:@me");
    let b = dashboard_list_key("is:open is:pr author:@me");
    assert_ne!(a, b);
    assert_eq!(a, dashboard_list_key("is:open is:pr review-requested:@me"));
}

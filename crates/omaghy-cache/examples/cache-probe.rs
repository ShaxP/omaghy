//! Open a cache as a given viewer, seed it, and print what that viewer sees.
//!
//! The crate is not wired into the binary until M1 integration, so this is how
//! the cache's behaviour can be observed by hand rather than only asserted in
//! tests: run it twice against one file as two different viewers and the two
//! inboxes are disjoint; corrupt or re-version the file and watch what it does
//! about it.
//!
//!     cargo run -p omaghy-cache --example cache-probe -- <db-path> <viewer>
//!
//! It touches no network — there is nothing in this crate that could.

use omaghy_cache::{Cache, Clock, ListMeta, NOTIFICATIONS_LIST, SqliteStore};
use omaghy_model::{
    Enrichment, Notification, NotificationId, NotificationReason, RepoRef, SubjectKind, SubjectRef,
};
use omaghy_store::{
    RefreshTarget, Store, Viewer,
    query::{NotificationQuery, ReadFilter},
};
use time::{Duration, OffsetDateTime};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(login)) = (args.next(), args.next()) else {
        eprintln!("usage: cache-probe <db-path> <viewer-login>");
        std::process::exit(2);
    };

    let store = match SqliteStore::open(&path, Viewer::new(&login)) {
        Ok(s) => s.with_clock(Clock::Fixed(now())),
        Err(e) => {
            // The corrupt case lands here. The file has already been rebuilt,
            // so running this again succeeds — that is the point of reporting
            // it rather than swallowing it.
            eprintln!("could not open {path} as {login}: {e}");
            eprintln!("(if that says 'corrupt', the file was rebuilt; run this again)");
            std::process::exit(1);
        }
    };

    // Before touching anything: what was already in the file for this viewer?
    // This is the line that makes a rebuild visible — after a version bump it
    // reads 0, and so does a fresh file, and so does another account's.
    let on_open = store
        .with_cache(|cache: &Cache| Ok(cache.notifications(&NotificationQuery::default())?.len()))
        .expect("a cache read");

    if let Err(e) = seed(&store, &login) {
        eprintln!("seeding failed: {e}");
        std::process::exit(1);
    }

    // Reads never touch the network, so they need no runtime of their own
    // beyond somewhere to poll the future to completion.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");

    rt.block_on(async {
        // Mark the oldest row read, as the TUI would on `x`. Optimistic: it
        // lands locally and emits Updated before anything is sent.
        store
            .mark_read(&[NotificationId(format!("{login}-3"))])
            .await
            .expect("mark_read is idempotent and local");

        // Schedule a refresh. Nothing fetches yet; the intent is recorded and
        // the read below reports itself as refreshing.
        store.refresh(RefreshTarget::Notifications);

        let all = store
            .notifications(&NotificationQuery::default())
            .await
            .expect("a cache read never fails on a healthy cache");
        let unread = store
            .notifications(&NotificationQuery {
                read: ReadFilter::UnreadOnly,
                ..Default::default()
            })
            .await
            .expect("a cache read");

        println!("viewer:     {login}");
        println!("on open:    {on_open} rows already stored for this viewer");
        println!("database:   {path}");
        println!(
            "schema:     user_version {} ({})",
            omaghy_cache::SCHEMA_VERSION,
            journal_mode(&path)
        );
        println!(
            "fetched_at: {:?}",
            all.fetched_at.map(|t| t.to_string()).as_deref()
        );
        println!(
            "state:      stale={} refreshing={} source={:?}",
            all.stale, all.refreshing, all.source
        );
        println!(
            "inbox:      {} rows, {} unread",
            all.value.len(),
            unread.value.len()
        );
        for n in &all.value.items {
            println!(
                "  [{}] {:<16} {:<22} {}",
                if n.unread { "u" } else { " " },
                n.reason.label(),
                n.repo.to_string(),
                n.title
            );
        }
    });
}

/// Fixed, so two runs produce the same ages and the output is comparable.
fn now() -> OffsetDateTime {
    time::macros::datetime!(2026-09-11 12:00 UTC)
}

/// Three rows whose ids carry the viewer's login, so a row leaking between
/// accounts is visible in the output rather than merely miscounted.
fn seed(store: &SqliteStore, login: &str) -> omaghy_model::Result<()> {
    let rows: Vec<Notification> = [
        (
            1,
            NotificationReason::ReviewRequested,
            SubjectKind::PullRequest,
            4,
        ),
        (2, NotificationReason::Mention, SubjectKind::Issue, 22),
        (
            3,
            NotificationReason::CiActivity,
            SubjectKind::CheckSuite,
            180,
        ),
    ]
    .into_iter()
    .map(|(i, reason, kind, mins)| Notification {
        id: NotificationId(format!("{login}-{i}")),
        unread: true,
        reason,
        updated_at: now() - Duration::minutes(mins),
        title: format!("{login}'s notification {i}"),
        kind,
        repo: RepoRef::new("ShaxP", "omaghy"),
        subject: SubjectRef::from_api_url("https://api.github.com/repos/ShaxP/omaghy/pulls/1"),
        detail: Enrichment::Absent,
    })
    .collect();

    store.with_cache(|cache: &Cache| {
        cache.put_notifications(&rows)?;
        // The inbox's freshness lives in list_meta, which is what makes the
        // read above say "fetched" rather than "never fetched".
        cache.put_list(NOTIFICATIONS_LIST, &[], &ListMeta::complete_at(now()))
    })
}

fn journal_mode(path: &str) -> String {
    rusqlite::Connection::open(path)
        .and_then(|c| c.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0)))
        .unwrap_or_else(|e| format!("unreadable: {e}"))
}

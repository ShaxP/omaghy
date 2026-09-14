//! Screen snapshots, driven by `FakeStore`.
//!
//! Every applicable arm of the state matrix (`spec/30-ui.md` §8) is covered:
//! the error and empty states are the ones that rot unnoticed, and they are
//! most of what a user sees on a bad day.

use omaghy_store::{FakeStore, Store, fake::Behaviour};
use omaghy_tui::{App, Route, SurfaceId};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::Arc;

async fn screen(store: Arc<dyn Store>, route: Route, w: u16, h: u16) -> String {
    let mut app = App::new(store, omaghy_store::fake::FIXTURE_NOW);
    app.start(route).await.expect("surface loads");
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.render(f)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn the_cursor_starts_on_the_first_row() {
    let store = Arc::new(FakeStore::with_corpus());
    let out = screen(store, Route::surface(SurfaceId::Notifications), 100, 12).await;
    // `group = by-repo` is the default (40-config.md §2), so line 1 is a
    // section header and the newest notification is the first row beneath it.
    let mut lines = out.lines().skip(1);
    let header = lines.next().unwrap_or_default();
    let first_row = lines.next().unwrap_or_default();
    assert!(
        header.contains("quickshell"),
        "the newest row's repository heads the list, got: {header}"
    );
    assert!(
        first_row.contains("Add SocketServer"),
        "newest notification should be the first row, got: {first_row}"
    );
}

#[tokio::test]
async fn populated_shows_counts_and_rows() {
    let store = Arc::new(FakeStore::with_corpus());
    let out = screen(store, Route::surface(SurfaceId::Notifications), 100, 12).await;
    assert!(out.contains("10 unread"), "header states the unread count");
    assert!(out.contains("29 total"));
    assert!(out.contains("ShaxP"), "header names the viewer");
    assert!(out.contains("quit"), "footer offers a way out");
}

#[tokio::test]
async fn all_caught_up_is_distinct_from_no_data_yet() {
    // An empty inbox and a cold cache are different screens.
    let empty = Arc::new(FakeStore::empty());
    let out = screen(empty, Route::surface(SurfaceId::Notifications), 80, 12).await;
    assert!(out.contains("All caught up"), "got: {out}");

    let cold = Arc::new(FakeStore::with_corpus());
    cold.set_behaviour(Behaviour::offline_without_cache());
    let out = screen(cold, Route::surface(SurfaceId::Notifications), 80, 12).await;
    assert!(out.contains("No data yet"), "got: {out}");
}

#[tokio::test]
async fn stale_data_is_shown_not_hidden() {
    let store = Arc::new(FakeStore::with_corpus());
    store.set_behaviour(Behaviour::offline_with_cache());
    let out = screen(store, Route::surface(SurfaceId::Notifications), 100, 12).await;
    assert!(
        out.contains("Add SocketServer"),
        "content still renders when stale"
    );
}

#[tokio::test]
async fn a_too_small_terminal_says_so_rather_than_degrading() {
    let store = Arc::new(FakeStore::with_corpus());
    let out = screen(store, Route::surface(SurfaceId::Notifications), 30, 6).await;
    assert!(out.contains("too small"), "got: {out}");
    // The message must fit the terminal it is describing, not be truncated
    // into nonsense by it.
    assert!(out.contains("40x10"), "names the minimum; got: {out}");
    assert!(
        out.contains("30x6"),
        "names what it actually has; got: {out}"
    );
}

#[tokio::test]
async fn unimplemented_surfaces_say_which_milestone() {
    let store = Arc::new(FakeStore::with_corpus());
    let out = screen(store.clone(), Route::surface(SurfaceId::Actions), 80, 12).await;
    assert!(out.contains("Not implemented"), "got: {out}");
    assert!(out.contains("M4"));

    let out = screen(store, Route::surface(SurfaceId::PullRequests), 80, 12).await;
    assert!(out.contains("M2"), "got: {out}");
}

/// The landing surface, reached through the router the way `omaghy dashboard`
/// reaches it. Replaces the half of the test above that asserted the
/// dashboard was still a stub — W2.2 landed it.
#[tokio::test]
async fn the_dashboard_is_the_landing_surface() {
    let store = Arc::new(FakeStore::with_corpus());
    let out = screen(store, Route::surface(SurfaceId::Dashboard), 80, 12).await;
    assert!(!out.contains("Not implemented"), "got: {out}");
    assert!(out.contains("Needs my review"), "got: {out}");
    assert!(out.contains("next section"), "the footer is its own: {out}");
}

/// Where the cursor actually is, by style rather than by text — the only way
/// to catch a highlight rendering on the wrong row.
#[tokio::test]
async fn the_highlight_lands_on_the_cursor_row() {
    use ratatui::style::Modifier;

    let store = Arc::new(FakeStore::with_corpus());
    let mut app = App::new(store, omaghy_store::fake::FIXTURE_NOW);
    app.start(Route::surface(SurfaceId::Notifications))
        .await
        .unwrap();
    let mut t = Terminal::new(TestBackend::new(100, 12)).unwrap();
    t.draw(|f| app.render(f)).unwrap();

    let buf = t.backend().buffer().clone();
    let reversed: Vec<u16> = (0..buf.area.height)
        .filter(|&y| {
            buf[(2, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        })
        .collect();

    // Two lines, because `rows = two-line` is the default and a row's cursor
    // covers both of its lines; y=1 is the section header, so the first row
    // starts at y=2.
    assert_eq!(
        reversed,
        vec![2, 3],
        "exactly the first list row — both its lines — should be highlighted"
    );
}

//! Screen snapshots for the whole widget set — `spec/30-ui.md` §10.
//!
//! Covered here, because these are the screens that rot unnoticed:
//!
//! - **every arm of the §8 state matrix**, in both icon modes;
//! - **every width breakpoint of §4.1**, in both icon modes, for both an
//!   unenriched page (what the first frame actually shows) and a fully
//!   populated one (what every column looks like);
//! - the palette, the help overlay, the header and the footer.
//!
//! Everything is driven by `FakeStore` and its fixed `FIXTURE_NOW`, so no
//! snapshot contains a wall-clock time, a duration computed against one, or
//! an order that depends on a hash map. A snapshot that fails on a Tuesday
//! for reasons nobody can reproduce is worse than no snapshot.

use crate::{
    keys::{Binding, GLOBAL_BINDINGS},
    theme::{Icon, Icons, Role},
    widgets::{
        Cell, Conditions, EmptyCopy, Footer, Freshness, Header, Help, HelpSection, Palette, Row,
        RowList, StateView, SurfaceState, Toast, classify,
        test_support::{buffer, render, text},
    },
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use omaghy_model::{LimitKind, Notification, StoreError, SubjectId, SubjectKind, age};
use omaghy_store::{
    FakeStore, NotificationQuery, Page, Store,
    fake::{Behaviour, FIXTURE_NOW},
};
use ratatui::{Frame, layout::Rect, style::Modifier, widgets::ListState};
use time::Duration;

// ------------------------------------------------------------ the mapping
//
// Turning a domain type into a [`Row`] belongs to the surface that owns the
// type — W2.3 for notifications. This is the mapping those snapshots need,
// and it doubles as the worked example of the widget API.

fn icon_for(kind: &SubjectKind) -> Icon {
    match kind {
        SubjectKind::PullRequest => Icon::PrOpen,
        SubjectKind::Issue => Icon::IssueOpen,
        SubjectKind::Discussion => Icon::Discussion,
        SubjectKind::Release => Icon::Release,
        SubjectKind::CheckSuite => Icon::Workflow,
        SubjectKind::Commit => Icon::Commit,
        SubjectKind::VulnerabilityAlert => Icon::Security,
        SubjectKind::Other(_) => Icon::Comment,
    }
}

fn row_of(n: &Notification) -> Row {
    let mut row = Row::new(n.title.clone(), age::relative(n.updated_at, FIXTURE_NOW))
        .icon(icon_for(&n.kind))
        .unread(n.unread)
        .repo(n.repo.to_string());
    if let Some(SubjectId::Number(num)) = n.subject.as_ref().map(|s| &s.id) {
        row = row.number(*num);
    }
    row
}

fn rows_of(page: &Page<Notification>) -> Vec<Row> {
    page.items.iter().map(row_of).collect()
}

/// A page with every column filled, so the snapshots show what each column
/// looks like rather than what an unenriched first frame looks like.
fn enriched_rows() -> Vec<Row> {
    vec![
        Row::new("Add SocketServer reconnect backoff and idle timeout", "4m")
            .icon(Icon::PrOpen)
            .unread(true)
            .repo("quickshell/quickshell")
            .number(412)
            .state(Cell::new(Icon::PrOpen, "open", Role::Success))
            .checks(Cell::new(Icon::CheckFail, "2/14", Role::Danger))
            .actor("outfoxxed"),
        Row::new("Bar widget plugins declare a preferred section", "22m")
            .icon(Icon::IssueOpen)
            .unread(true)
            .repo("basecamp/omarchy")
            .number(1904)
            .state(Cell::new(Icon::IssueOpen, "open", Role::Success))
            .checks(Cell::new(Icon::CheckPending, "3", Role::Warning))
            .actor("dhh"),
        Row::new(
            "fix: syntax highlighting follows the Dark/Light toggle",
            "2h",
        )
        .icon(Icon::PrMerged)
        .repo("ShaxP/shax")
        .number(61)
        .state(Cell::new(Icon::PrMerged, "merged", Role::Accent))
        .checks(Cell::new(Icon::CheckPass, "14", Role::Success))
        .actor("ShaxP"),
        Row::new(
            "Refactor the Wayland layer-shell surface lifecycle so that anchors, \
             exclusive zones, and keyboard focus are reconciled in a single pass",
            "8h",
        )
        .icon(Icon::PrDraft)
        .repo("some-very-long-organization-name/an-equally-long-repository-name")
        .number(12345)
        .state(Cell::new(Icon::PrDraft, "draft", Role::Muted))
        .actor("a-very-long-login-name"),
    ]
}

// ------------------------------------------------------------- the scenes

struct Scene {
    name: &'static str,
    store: FakeStore,
    error: Option<StoreError>,
    filter: Option<&'static str>,
}

/// Every arm of §8, each produced by `FakeStore` rather than by hand.
fn state_matrix() -> Vec<Scene> {
    let scene = |name, store, error, filter| Scene {
        name,
        store,
        error,
        filter,
    };
    let cold = FakeStore::with_corpus();
    cold.set_behaviour(Behaviour::offline_without_cache());

    let stale = FakeStore::with_corpus();
    stale.set_behaviour(Behaviour::offline_with_cache());

    let refreshing = FakeStore::with_corpus();
    refreshing.set_behaviour(Behaviour {
        refreshing: true,
        ..Default::default()
    });

    let no_cache = FakeStore::with_corpus();
    no_cache.set_behaviour(Behaviour::offline_without_cache());

    vec![
        scene("cold", cold, None, None),
        scene("populated", FakeStore::with_corpus(), None, None),
        scene("stale", stale, None, None),
        scene("refreshing", refreshing, None, None),
        scene("empty", FakeStore::empty(), None, None),
        scene(
            "filtered-empty",
            FakeStore::empty(),
            None,
            Some("unread only"),
        ),
        scene(
            "offline-with-cache",
            FakeStore::with_corpus(),
            Some(StoreError::Offline("dns lookup failed".into())),
            None,
        ),
        scene(
            "offline-without-cache",
            no_cache,
            Some(StoreError::Offline("dns lookup failed".into())),
            None,
        ),
        scene(
            "rate-limited",
            FakeStore::with_corpus(),
            Some(StoreError::RateLimited {
                kind: LimitKind::Primary,
                at: FIXTURE_NOW + Duration::hours(1),
            }),
            None,
        ),
        scene(
            "forbidden",
            FakeStore::with_corpus(),
            Some(StoreError::Forbidden),
            None,
        ),
        scene(
            "error",
            FakeStore::with_corpus(),
            Some(StoreError::Upstream {
                status: 502,
                message: "bad gateway".into(),
            }),
            None,
        ),
    ]
}

/// Header, body, footer — the three zones of §4, as a surface composes them.
#[allow(clippy::too_many_arguments)]
fn screen(
    f: &mut Frame,
    area: Rect,
    page: &omaghy_store::Fresh<Page<Notification>>,
    state: &SurfaceState,
    icons: Icons,
    selected: Option<usize>,
) {
    let [head, body, foot] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(1),
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(1),
    ])
    .areas(area);

    let rows = rows_of(&page.value);
    let list = RowList::new(&rows).icons(icons).viewer("ShaxP");
    let counts = format!(
        "{} unread · {} total",
        page.value.items.iter().filter(|n| n.unread).count(),
        page.value.len()
    );
    let mut header = Header::new("Notifications", "ShaxP")
        .counts(&counts)
        .freshness(Freshness::of(page))
        .icons(icons);
    if let Some(repo) = list.elided_repo() {
        header = header.scope(repo);
    }
    header.render(f, head);

    let view = StateView::new(state).icons(icons).empty(EmptyCopy {
        headline: "All caught up",
        detail: "No notification needs your attention.",
        action: Some(("r", "check again")),
    });
    if let Some(rows_area) = view.render(f, body).rows() {
        let mut ls = ListState::default();
        ls.select(selected);
        list.render(f, rows_area, &mut ls);
    }

    let hints: &[Binding] = &[
        Binding::new("notification.next", "j / k", "move"),
        Binding::new("notification.mark-read", "Enter", "toggle read"),
        Binding::new("app.help", "?", "help"),
    ];
    Footer::new(hints).icons(icons).render(f, foot);
}

async fn fresh(store: &FakeStore) -> omaghy_store::Fresh<Page<Notification>> {
    store
        .notifications(&NotificationQuery {
            limit: Some(8),
            ..Default::default()
        })
        .await
        .expect("FakeStore reads do not fail unless asked to")
}

async fn state_matrix_screens(icons: Icons) -> String {
    let mut out = String::new();
    for scene in state_matrix() {
        let page = fresh(&scene.store).await;
        let state = classify(
            &Conditions::from_fresh(&page, page.value.len())
                .filter(scene.filter)
                .error(scene.error.as_ref()),
        );
        assert_eq!(
            state.name(),
            scene.name,
            "the scene must actually produce the state it claims"
        );
        out.push_str(&format!("── {} ──\n", scene.name));
        out.push_str(&render(88, 12, |f, a| {
            screen(f, a, &page, &state, icons, Some(0))
        }));
        out.push_str("\n\n");
    }
    out
}

#[tokio::test]
async fn the_state_matrix_in_unicode() {
    insta::assert_snapshot!(
        "state_matrix_unicode",
        state_matrix_screens(Icons::UNICODE).await
    );
}

#[tokio::test]
async fn the_state_matrix_in_ascii() {
    insta::assert_snapshot!(
        "state_matrix_ascii",
        state_matrix_screens(Icons::ASCII).await
    );
}

// ------------------------------------------------------- width breakpoints

/// The breakpoints of §4.1, plus one either side of each boundary.
const WIDTHS: [u16; 8] = [40, 59, 60, 79, 80, 99, 100, 120];

fn breakpoints(rows: &[Row], icons: Icons) -> String {
    let mut out = String::new();
    for w in WIDTHS {
        let cols = RowList::new(rows).icons(icons).columns(w);
        out.push_str(&format!("── {w} cols: {:?} ──\n", cols.as_slice()));
        out.push_str(&render(w, rows.len() as u16, |f, a| {
            let mut ls = ListState::default();
            ls.select(Some(0));
            RowList::new(rows).icons(icons).render(f, a, &mut ls);
        }));
        out.push_str("\n\n");
    }
    out
}

#[tokio::test]
async fn every_breakpoint_with_an_unenriched_page() {
    // What the first frame actually shows: the second pass has not landed,
    // so state and checks are blank. The columns must still line up.
    let store = FakeStore::with_corpus();
    let page = fresh(&store).await;
    let rows = rows_of(&page.value);
    insta::assert_snapshot!(
        "breakpoints_unenriched_unicode",
        breakpoints(&rows, Icons::UNICODE)
    );
    insta::assert_snapshot!(
        "breakpoints_unenriched_ascii",
        breakpoints(&rows, Icons::ASCII)
    );
}

#[test]
fn every_breakpoint_with_every_column_filled() {
    let rows = enriched_rows();
    insta::assert_snapshot!(
        "breakpoints_enriched_unicode",
        breakpoints(&rows, Icons::UNICODE)
    );
    insta::assert_snapshot!(
        "breakpoints_enriched_ascii",
        breakpoints(&rows, Icons::ASCII)
    );
}

#[test]
fn no_row_ever_overflows_its_terminal() {
    // A row one cell too wide wraps, and a wrapped row silently halves the
    // list without failing anything.
    for icons in [Icons::UNICODE, Icons::ASCII] {
        for w in WIDTHS {
            let rows = enriched_rows();
            let out = render(w, 4, |f, a| {
                let mut ls = ListState::default();
                ls.select(Some(1));
                RowList::new(&rows).icons(icons).render(f, a, &mut ls);
            });
            for line in out.lines() {
                assert!(line.chars().count() <= w as usize, "{line:?} at width {w}");
            }
        }
    }
}

// ------------------------------------------------------------- the overlays

#[test]
fn the_command_palette() {
    let actions: Vec<Binding> = GLOBAL_BINDINGS
        .iter()
        .cloned()
        .chain([
            Binding::new("notification.mark-read", "Enter", "toggle read"),
            Binding::new("notification.unread-only", "u", "unread only"),
            Binding::new("pr.approve", "a", "approve"),
            Binding::new("repo.open-in-browser", "o", "open on github.com"),
        ])
        .collect();

    let empty = Palette::new(actions.clone());
    let mut typed = Palette::new(actions.clone());
    for c in "mark".chars() {
        typed.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let mut nothing = Palette::new(actions);
    for c in "zzz".chars() {
        nothing.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    let mut out = String::new();
    for (label, p) in [
        ("open", &empty),
        ("query", &typed),
        ("no matches", &nothing),
    ] {
        out.push_str(&format!("── {label} ──\n"));
        out.push_str(&render(80, 14, |f, a| p.render(f, a)));
        out.push_str("\n\n");
    }
    insta::assert_snapshot!("palette", out);
}

#[test]
fn the_help_overlay_in_both_icon_modes() {
    const SURFACE: &[Binding] = &[
        Binding::new("notification.next", "j / k", "move"),
        Binding::new("notification.mark-read", "Enter", "toggle read"),
        Binding::new("notification.unread-only", "u", "unread only"),
    ];
    let sections = [
        HelpSection::new("This surface", SURFACE),
        HelpSection::new("Everywhere", GLOBAL_BINDINGS),
    ];
    let mut out = String::new();
    for (label, icons) in [("unicode", Icons::UNICODE), ("ascii", Icons::ASCII)] {
        out.push_str(&format!("── {label} ──\n"));
        out.push_str(&render(80, 24, |f, a| {
            Help::new(&sections).icons(icons).render(f, a)
        }));
        out.push_str("\n\n");
    }
    insta::assert_snapshot!("help", out);
}

#[test]
fn the_header_and_footer_in_both_icon_modes() {
    let hints: &[Binding] = &[
        Binding::new("notification.next", "j / k", "move"),
        Binding::new("notification.mark-read", "Enter", "toggle read"),
        Binding::new("app.quit", "q", "quit"),
    ];
    let stale = omaghy_store::Fresh::from_cache(
        1,
        FIXTURE_NOW - Duration::hours(2),
        Duration::minutes(5),
        FIXTURE_NOW,
    )
    .refreshing(true);

    let mut out = String::new();
    for (label, icons) in [("unicode", Icons::UNICODE), ("ascii", Icons::ASCII)] {
        for (what, toast) in [
            ("info", Some(Toast::progress("Refreshing…"))),
            ("success", Some(Toast::success("Marked 3 read"))),
            ("error", Some(Toast::from_error(&StoreError::Forbidden))),
            ("none", None),
        ] {
            out.push_str(&format!("── {label} · {what} ──\n"));
            out.push_str(&render(80, 2, |f, a| {
                Header::new("Notifications", "ShaxP")
                    .scope("ShaxP/shax")
                    .counts("10 unread · 29 total")
                    .freshness(Freshness::of(&stale))
                    .icons(icons)
                    .render(f, Rect { height: 1, ..a });
                Footer::new(hints)
                    .icons(icons)
                    .toast(toast.as_ref())
                    .render(
                        f,
                        Rect {
                            y: a.y + 1,
                            height: 1,
                            ..a
                        },
                    );
            }));
            out.push_str("\n\n");
        }
    }
    insta::assert_snapshot!("chrome", out);
}

// ------------------------------------------------ style, not merely text

#[tokio::test]
async fn the_highlight_lands_on_the_row_the_cursor_is_on() {
    // A highlight drawn on the wrong row is invisible to any assertion about
    // text, which is how one nearly shipped.
    let store = FakeStore::with_corpus();
    let page = fresh(&store).await;
    let rows = rows_of(&page.value);
    let buf = buffer(100, 6, |f, a| {
        let mut ls = ListState::default();
        ls.select(Some(2));
        RowList::new(&rows).render(f, a, &mut ls);
    });

    let reversed: Vec<u16> = (0..buf.area.height)
        .filter(|&y| {
            buf[(4, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        })
        .collect();
    assert_eq!(reversed, vec![2], "exactly the third row is highlighted");

    // And the cursor is also a glyph, so it survives a terminal that does
    // not do reverse video and a screenshot that loses it.
    assert_eq!(buf[(0, 2)].symbol(), Icons::UNICODE.cursor());
    assert_eq!(buf[(0, 1)].symbol(), " ");
}

#[tokio::test]
async fn unread_rows_are_bold_and_read_rows_are_not() {
    let store = FakeStore::with_corpus();
    let page = fresh(&store).await;
    let rows = rows_of(&page.value);
    let unread_at: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.unread)
        .map(|(i, _)| i)
        .collect();
    assert!(!unread_at.is_empty(), "the corpus has unread rows");

    let buf = buffer(100, rows.len() as u16, |f, a| {
        let mut ls = ListState::default();
        ls.select(None);
        RowList::new(&rows).render(f, a, &mut ls);
    });
    for (y, row) in rows.iter().enumerate() {
        // Column 5 is inside the title, past the gutter and the icon.
        let bold = buf[(5, y as u16)]
            .style()
            .add_modifier
            .contains(Modifier::BOLD);
        assert_eq!(bold, row.unread, "row {y} weight disagrees with unread");
        let marker = buf[(1, y as u16)].symbol();
        assert_eq!(
            marker == Icons::UNICODE.get(Icon::Unread),
            row.unread,
            "row {y} marker disagrees with unread"
        );
    }
}

#[test]
fn every_non_content_state_draws_a_glyph_and_not_only_a_colour() {
    // The §7.1 rule, measured: Osaka Jade resolves accent and foreground to
    // the same value, so a state distinguished only by colour is not
    // distinguished at all.
    let states = [
        SurfaceState::Empty,
        SurfaceState::FilteredEmpty {
            filter: "unread only".into(),
        },
        SurfaceState::OfflineWithCache,
        SurfaceState::OfflineWithoutCache,
        SurfaceState::RateLimited {
            kind: LimitKind::Primary,
            at: FIXTURE_NOW + Duration::hours(1),
        },
        SurfaceState::Forbidden,
        SurfaceState::Error {
            message: "GitHub returned 502: bad gateway".into(),
        },
        SurfaceState::Cold,
    ];
    for state in &states {
        let buf = buffer(80, 12, |f, a| {
            StateView::new(state).render(f, a);
        });
        let screen = text(&buf);
        // An Octicon, a skeleton bar, or a spinner frame: anything that is
        // not a letter doing the work a colour would otherwise do alone.
        let has_glyph = screen.chars().any(|c| {
            (0xF400..=0xF533).contains(&(c as u32)) || matches!(c, '░' | '▘' | '▝' | '▗' | '▖')
        });
        assert!(has_glyph, "{} renders no glyph: {screen}", state.name());
    }
}

#[test]
fn the_offline_banner_keeps_the_rows_underneath_it() {
    // Stale data is never hidden, and content is never replaced by a banner.
    let rows = enriched_rows();
    let out = render(100, 8, |f, a| {
        let view = StateView::new(&SurfaceState::OfflineWithCache);
        if let Some(rows_area) = view.render(f, a).rows() {
            let mut ls = ListState::default();
            ls.select(Some(0));
            RowList::new(&rows).render(f, rows_area, &mut ls);
        }
    });
    assert!(out.lines().next().unwrap().contains("Offline"), "{out}");
    assert!(out.contains("SocketServer"), "the rows survive: {out}");
}

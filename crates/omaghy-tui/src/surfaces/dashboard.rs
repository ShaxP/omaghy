//! The dashboard — the landing surface.
//!
//! Configurable sections (`omaghy_store::DashboardConfig`), each a saved
//! GitHub search: *Needs my review*, *My pull requests*, *Assigned to me*,
//! *Recently mentioned*. The cursor moves between sections and `Enter`
//! navigates to the search the section names, so the dashboard answers
//! "where should I look" and then takes you there.
//!
//! **Sections carry a count, not rows.** `DashboardSectionData` has a title,
//! a query and a count, because nothing fetches pull-request or issue *lists*
//! until M2. This surface therefore renders a count per section and navigates
//! on `Enter`; rows arrive with the surfaces that can fetch them.
//!
//! Three encodings, never one (`spec/30-ui.md` §7.1): a section that needs
//! you carries a filled marker, a bold title *and* a number; a section that
//! is clear carries a tick, muted weight *and* the word "nothing". Osaka Jade
//! resolves `accent` and `foreground` to the same value, so the colour is the
//! encoding that is allowed to be missing.

use crate::{
    keys::Binding,
    route::{Route, SurfaceId},
    surface::{Ctx, Outcome, Surface},
    theme::{Icon, Icons, Role},
    widgets::chrome::Freshness,
    widgets::{Conditions, EmptyCopy, StateView, SurfaceState, classify, elide, list::cells},
};
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent};
use omaghy_model::{Result, StoreError};
use omaghy_store::{
    Dashboard as DashboardData, DashboardConfig, DashboardSectionData, Fresh, RefreshTarget,
    StoreEvent,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};

const BINDINGS: &[Binding] = &[
    Binding::new("dashboard.next", "j / ↓", "next section"),
    Binding::new("dashboard.prev", "k / ↑", "previous section"),
    Binding::new("dashboard.open", "Enter", "open this section"),
    Binding::new("dashboard.first", "g", "first"),
    Binding::new("dashboard.last", "G", "last"),
];

/// Cells before the title: cursor, space, section glyph, space. The cursor
/// column never drops — a selection that is only reverse video is invisible
/// on the terminals that do not do reverse video and in every screenshot
/// (`spec/30-ui.md` §4.1).
const GUTTER: usize = 4;

/// What a section says on its right-hand side when it has nothing.
///
/// A word rather than `0`: "nothing needs your review" is good news, and a
/// bare zero reads like a failed fetch.
const CLEAR: &str = "nothing";

#[derive(Debug, Default)]
pub struct Dashboard {
    cfg: DashboardConfig,
    /// The last successful read. Kept across a failed one, so an offline
    /// refresh shows a banner over content rather than replacing it.
    data: Option<Fresh<DashboardData>>,
    /// What the last read failed with. Cleared by a successful one.
    error: Option<StoreError>,
    cursor: usize,
    list: ListState,
    icons: Icons,
}

impl Dashboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// The sections to show (`40-config.md` §2 `[dashboard]`).
    ///
    /// Taken at construction rather than read from [`Ctx`] per frame: the
    /// sections decide what this surface *is*, and a screen offering sections
    /// the syncer does not count would be the dashboard lying again.
    #[must_use]
    pub fn with_config(mut self, cfg: &DashboardConfig) -> Self {
        self.cfg = cfg.clone();
        self
    }

    /// The icon mode.
    ///
    /// Defaulted rather than taken from [`Ctx`], which carries no icon mode —
    /// so `icons = "ascii"` from `config.toml` cannot currently reach a
    /// surface. Recorded as a contract-change request; the mode is plumbed
    /// here so both modes are snapshot-testable meanwhile (§10).
    pub fn icons(mut self, icons: Icons) -> Self {
        self.icons = icons;
        self
    }

    /// Sections as configured, resolved against the last read.
    fn sections(&self) -> &[DashboardSectionData] {
        self.data
            .as_ref()
            .map(|d| d.value.sections.as_slice())
            .unwrap_or_default()
    }

    fn focused(&self) -> Option<&DashboardSectionData> {
        self.sections().get(self.cursor)
    }

    /// How many sections have something in them.
    fn live(&self) -> usize {
        self.sections().iter().filter(|s| s.count > 0).count()
    }

    /// Which arm of `spec/30-ui.md` §8 applies.
    ///
    /// `items` is the number of sections we have *fetched counts for*, not
    /// the number configured: the section list comes from config and exists
    /// before any fetch, so its length is no evidence that anything was read.
    /// Without the distinction a cold cache would render four sections all
    /// reading "nothing", which is a lie rather than a skeleton.
    fn state(&self) -> SurfaceState {
        let Some(data) = self.data.as_ref() else {
            // Nothing read yet, successfully or otherwise.
            return match self.error.as_ref() {
                Some(err) => classify(&Conditions::default().error(Some(err))),
                None => SurfaceState::Cold,
            };
        };
        let items = if data.fetched_at.is_some() {
            data.value.sections.len()
        } else {
            0
        };
        classify(&Conditions::from_fresh(data, items).error(self.error.as_ref()))
    }

    fn move_cursor(&mut self, delta: isize) {
        let n = self.sections().len();
        if n == 0 {
            return;
        }
        let next = (self.cursor as isize + delta).clamp(0, n as isize - 1);
        self.cursor = next as usize;
    }

    fn jump(&mut self, to: usize) {
        let n = self.sections().len();
        if n == 0 {
            return;
        }
        self.cursor = to.min(n - 1);
    }

    /// Where `Enter` goes: the section's query, as an addressable route.
    ///
    /// Sections are GitHub search syntax, and the surface that executes
    /// GitHub search syntax is Search (`spec/30-ui.md` §3.2 —
    /// `search?q=is:pr+review-requested:@me`). Routing an `is:pr` section to
    /// the pull-request list instead would presuppose that list takes a raw
    /// query; its routes are repo-scoped.
    fn route_for(section: &DashboardSectionData) -> Route {
        Route {
            surface: SurfaceId::Search,
            arg: Some(format!("q={}", section.query)),
        }
    }

    /// One section: cursor, glyph, title, right-aligned count.
    ///
    /// Exactly `width` cells, because a line one cell too wide wraps and
    /// silently halves the list.
    fn section_line(&self, s: &DashboardSectionData, cursor: bool, width: usize) -> Line<'static> {
        let icons = self.icons;
        let clear = s.count == 0;
        let count = if clear {
            CLEAR.to_owned()
        } else {
            s.count.to_string()
        };
        let count_w = cells(&count);
        let title_w = width.saturating_sub(GUTTER + 1 + count_w);

        // Glyph, weight and word all say the same thing, so the row survives
        // a monochrome theme and a terminal without bold.
        let (glyph, glyph_role) = if clear {
            (Icon::CaughtUp, Role::Success)
        } else {
            (Icon::Unread, Role::Accent)
        };
        let title_style = if clear {
            Role::Muted.style()
        } else {
            Role::Default.style().add_modifier(Modifier::BOLD)
        };
        let count_style = if clear {
            Role::Muted.style()
        } else {
            Role::Accent.style().add_modifier(Modifier::BOLD)
        };

        let title = pad(&s.title, title_w, icons);
        Line::from(vec![
            Span::styled(
                if cursor { icons.cursor() } else { " " },
                Role::Accent.style(),
            ),
            Span::raw(" "),
            Span::styled(icons.get(glyph).to_owned(), glyph_role.style()),
            Span::raw(" "),
            Span::styled(title, title_style),
            Span::raw(" "),
            Span::styled(count, count_style),
        ])
    }

    /// One line naming where `Enter` would take you, for the focused section.
    ///
    /// The query is the section's whole identity and is invisible otherwise;
    /// a section with nothing in it says so in words here, because a tick and
    /// the word "nothing" on the row are compact rather than reassuring.
    fn detail_line(&self, width: usize) -> Line<'static> {
        let Some(s) = self.focused() else {
            return Line::raw("");
        };
        let (icon, text) = if s.count == 0 {
            (
                Icon::CaughtUp,
                format!("Nothing matches {} right now.", s.query),
            )
        } else {
            (Icon::Search, s.query.clone())
        };
        Line::from(vec![
            Span::raw(" "),
            Span::styled(self.icons.get(icon).to_owned(), Role::Muted.style()),
            Span::raw(" "),
            Span::styled(
                elide(&text, width.saturating_sub(3), self.icons),
                Role::Muted.style(),
            ),
        ])
    }

    /// The rows, plus the detail line when there is room for it.
    fn render_sections(&mut self, f: &mut Frame, area: Rect) {
        let sections = self.sections();
        if sections.is_empty() || area.height == 0 || area.width == 0 {
            return;
        }
        let width = area.width as usize;

        // The detail line sits directly under the sections rather than at the
        // bottom of the body: it describes the focused one, and a line about
        // the cursor twenty rows away from it is a line nobody reads. It is
        // also the first thing to go on a short terminal — a section that is
        // not visible is worse than a query that is not.
        let n = sections.len() as u16;
        let detail = area.height >= n + 2;
        let rows = Rect {
            height: if detail { n } else { area.height },
            ..area
        };

        let cursor = self.cursor;
        let items: Vec<ListItem> = sections
            .iter()
            .enumerate()
            .map(|(i, s)| ListItem::new(self.section_line(s, i == cursor, width)))
            .collect();
        self.list.select(Some(self.cursor));
        f.render_stateful_widget(
            List::new(items).highlight_style(Role::Selected.style()),
            rows,
            &mut self.list,
        );

        if detail {
            f.render_widget(
                Paragraph::new(self.detail_line(width)),
                Rect {
                    y: area.y + n + 1,
                    height: 1,
                    ..area
                },
            );
        }
    }
}

#[async_trait]
impl Surface for Dashboard {
    /// The header is drawn by the shell from this string, and the shell
    /// passes it no [`Fresh`] — so the provenance note §8 puts in the header
    /// has to travel as part of the title. Recorded as a contract-change
    /// request: `App::render` should take the surface's freshness. Until it
    /// does, stale data would otherwise be silently indistinguishable from
    /// current data, and stale data is never hidden.
    fn title(&self) -> String {
        let mut t = String::from("Dashboard");
        let sections = self.sections();
        match self.data.as_ref() {
            Some(d) if d.fetched_at.is_none() => t.push_str("  not fetched"),
            Some(d) => {
                if !sections.is_empty() {
                    t.push_str(&format!(
                        "  {} of {} sections need you",
                        self.live(),
                        sections.len()
                    ));
                }
                if d.needs_provenance_note() {
                    t.push_str(" · cached");
                }
            }
            None => {}
        }
        t
    }

    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &Ctx) {
        let state = self.state();
        let view = StateView::new(&state)
            .icons(self.icons)
            .retry("r", "try again")
            .empty(EmptyCopy {
                // The dashboard's sections come from config, so "empty" can
                // only mean the config has none — never "all caught up",
                // which here is four sections each reading "nothing".
                headline: "No sections configured",
                detail: "Your config defines no dashboard sections; the default set has four.",
                action: None,
            });
        if let Some(rows) = view.render(f, area).rows() {
            self.render_sections(f, rows);
        }
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_cursor(1);
                Outcome::Redraw
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_cursor(-1);
                Outcome::Redraw
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.jump(0);
                Outcome::Redraw
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.jump(usize::MAX);
                Outcome::Redraw
            }
            // A section with nothing in it is still worth opening: seeing the
            // empty search is how you find out the query is wrong.
            KeyCode::Enter => match self.focused() {
                Some(s) => Outcome::Push(Self::route_for(s)),
                None => Outcome::Ignored,
            },
            _ => Outcome::Ignored,
        }
    }

    /// A failed read is kept, not propagated.
    ///
    /// `App` applies `?` to this, so returning `Err` would take the whole
    /// program down on a DNS failure — and §8 requires offline, forbidden and
    /// rate-limited to be *designed screens*. The error is held and rendered.
    async fn load(&mut self, ctx: &Ctx) -> Result<()> {
        match ctx.store.dashboard(&self.cfg).await {
            Ok(data) => {
                self.data = Some(data);
                self.error = None;
            }
            Err(err) => self.error = Some(err),
        }
        // Sections can disappear between reads if the config changed.
        let n = self.sections().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
        Ok(())
    }

    fn cares_about(&self, ev: &StoreEvent) -> bool {
        ev.is_global() || matches!(ev.target(), Some(RefreshTarget::Dashboard))
    }

    fn keymap(&self) -> &[Binding] {
        BINDINGS
    }

    /// The header's note comes from here; without it `App` had nothing to
    /// ask and §8's stale indicator was unreachable.
    fn freshness(&self) -> Option<Freshness> {
        self.data.as_ref().map(Freshness::of)
    }

    /// What `r` means while this surface is on top.
    fn reconfigure(&mut self, ctx: &Ctx) {
        self.cfg = (*ctx.dashboard).clone();
    }

    fn refresh_target(&self) -> Option<RefreshTarget> {
        Some(RefreshTarget::Dashboard)
    }

    fn on_enter(&mut self, ctx: &Ctx) {
        ctx.store.refresh(RefreshTarget::Dashboard);
    }

    fn on_leave(&mut self, ctx: &Ctx) {
        ctx.store.cancel(&RefreshTarget::Dashboard);
    }
}

/// Elide to `width` cells and pad to exactly that.
///
/// `widgets::list` has `fit`, but it is private: a surface drawing anything
/// that is not shaped like a `Row` has to repeat this. Noted in the W1.3
/// feedback.
fn pad(s: &str, width: usize, icons: Icons) -> String {
    let mut out = elide(s, width, icons);
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(cells(&out))));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::IconMode;
    use crate::widgets::test_support::{buffer, render, text};
    use omaghy_model::LimitKind;
    use omaghy_store::{
        DashboardSection, FakeStore, Store,
        fake::{Behaviour, FIXTURE_NOW},
    };
    use std::sync::Arc;
    use time::Duration;

    fn ctx(store: Arc<dyn Store>) -> Ctx {
        Ctx::new(store, FIXTURE_NOW, Icons::new(IconMode::Unicode))
    }

    /// A loaded dashboard, exactly as the router would produce one.
    async fn loaded(store: FakeStore) -> (Dashboard, Ctx) {
        let ctx = ctx(Arc::new(store));
        let mut d = Dashboard::new();
        d.on_enter(&ctx);
        d.load(&ctx).await.expect("load never fails outward");
        (d, ctx)
    }

    /// Counts the fake cannot produce: it gives every section the same one,
    /// so a mixed dashboard — the ordinary case — has to be built by hand.
    fn mixed() -> Dashboard {
        let counts = [3u32, 7, 0, 1];
        let cfg = DashboardConfig::default();
        let sections = cfg
            .sections
            .iter()
            .zip(counts)
            .map(|(s, count)| DashboardSectionData {
                title: s.title.clone(),
                query: s.query.clone(),
                count,
            })
            .collect();
        Dashboard {
            cfg,
            data: Some(Fresh::from_network(DashboardData { sections }, FIXTURE_NOW)),
            ..Default::default()
        }
    }

    fn screen(d: &mut Dashboard, w: u16, h: u16) -> String {
        let store: Arc<dyn Store> = Arc::new(FakeStore::empty());
        let c = ctx(store);
        render(w, h, |f, a| d.render(f, a, &c))
    }

    // ------------------------------------------------------- behaviour

    #[tokio::test]
    async fn every_configured_section_renders_with_its_count() {
        let (mut d, _) = loaded(FakeStore::with_corpus()).await;
        let out = screen(&mut d, 80, 10);
        for title in [
            "Needs my review",
            "My pull requests",
            "Assigned to me",
            "Recently mentioned",
        ] {
            assert!(out.contains(title), "{title} missing from: {out}");
        }
        assert!(
            d.title().contains("sections need you"),
            "the header summarises: {}",
            d.title()
        );
    }

    #[test]
    fn a_section_with_nothing_in_it_reads_as_good_news() {
        // Not a bare zero, and not a blank: three encodings, none of them
        // colour — a tick, muted weight, and the word.
        let mut d = mixed();
        let out = screen(&mut d, 80, 10);
        assert!(out.contains("Assigned to me"), "{out}");
        assert!(out.contains(CLEAR), "the clear section says so: {out}");
        assert!(!out.contains(" 0"), "never a bare zero: {out}");

        let line = out
            .lines()
            .find(|l| l.contains("Assigned to me"))
            .expect("the clear section");
        assert!(
            line.contains(Icons::UNICODE.get(Icon::CaughtUp)),
            "a tick, not only a colour: {line:?}"
        );
    }

    #[test]
    fn a_section_that_needs_you_is_bold_as_well_as_marked() {
        let mut d = mixed();
        let store: Arc<dyn Store> = Arc::new(FakeStore::empty());
        let c = ctx(store);
        let buf = buffer(80, 10, |f, a| d.render(f, a, &c));

        // Column 4 is inside the title, past the cursor and the glyph.
        let bold: Vec<u16> = (0..4)
            .filter(|&y| buf[(4, y)].style().add_modifier.contains(Modifier::BOLD))
            .collect();
        assert_eq!(
            bold,
            vec![0, 1, 3],
            "exactly the sections with a count are bold: {}",
            text(&buf)
        );
        assert_eq!(
            buf[(2, 2)].symbol(),
            Icons::UNICODE.get(Icon::CaughtUp),
            "the clear section carries a tick"
        );
        assert_eq!(
            buf[(2, 0)].symbol(),
            Icons::UNICODE.get(Icon::Unread),
            "a section with work carries a marker"
        );
    }

    #[tokio::test]
    async fn the_cursor_is_a_glyph_as_well_as_a_highlight_and_moves() {
        let (mut d, c) = loaded(FakeStore::with_corpus()).await;
        let cursor_at = |d: &mut Dashboard| -> (Vec<u16>, String) {
            let buf = buffer(80, 10, |f, a| d.render(f, a, &c));
            let reversed = (0..4)
                .filter(|&y| {
                    buf[(4, y)]
                        .style()
                        .add_modifier
                        .contains(Modifier::REVERSED)
                })
                .collect();
            (reversed, buf[(0, 0)].symbol().to_owned())
        };

        let (reversed, first) = cursor_at(&mut d);
        assert_eq!(reversed, vec![0], "the cursor starts on the first section");
        assert_eq!(first, Icons::UNICODE.cursor(), "and is a glyph too");

        d.on_key(key(KeyCode::Char('j')), &c);
        d.on_key(key(KeyCode::Char('j')), &c);
        let (reversed, first) = cursor_at(&mut d);
        assert_eq!(reversed, vec![2], "j moved it down twice");
        assert_eq!(first, " ", "and off the first row");

        d.on_key(key(KeyCode::Char('G')), &c);
        assert_eq!(d.cursor, 3, "G goes to the last section");
        d.on_key(key(KeyCode::Char('G')), &c);
        assert_eq!(d.cursor, 3, "and stops there");
        d.on_key(key(KeyCode::Char('g')), &c);
        assert_eq!(d.cursor, 0);
        d.on_key(key(KeyCode::Char('k')), &c);
        assert_eq!(d.cursor, 0, "and stops at the top");
    }

    #[tokio::test]
    async fn enter_navigates_to_the_section_query() {
        let (mut d, c) = loaded(FakeStore::with_corpus()).await;
        let out = d.on_key(key(KeyCode::Enter), &c);
        let Outcome::Push(route) = out else {
            panic!("Enter must navigate, got {out:?}");
        };
        assert_eq!(route.surface, SurfaceId::Search);
        assert_eq!(
            route.arg.as_deref(),
            Some("q=is:open is:pr review-requested:@me")
        );
        // Addressable: what the palette pushes and `omaghy <route>` opens.
        assert_eq!(
            route.to_string().parse::<Route>().expect("round trips"),
            route
        );

        d.on_key(key(KeyCode::Char('j')), &c);
        let Outcome::Push(next) = d.on_key(key(KeyCode::Enter), &c) else {
            panic!("the second section navigates too");
        };
        assert_ne!(next, route, "each section goes somewhere different");
    }

    #[test]
    fn a_section_with_nothing_in_it_still_opens() {
        // Seeing the empty search is how you find out the query is wrong.
        let mut d = mixed();
        let store: Arc<dyn Store> = Arc::new(FakeStore::empty());
        let c = ctx(store);
        d.cursor = 2;
        assert!(matches!(
            d.on_key(key(KeyCode::Enter), &c),
            Outcome::Push(_)
        ));
    }

    #[tokio::test]
    async fn entering_schedules_a_dashboard_refresh_and_leaving_cancels_it() {
        let store = Arc::new(FakeStore::with_corpus());
        let c = ctx(store.clone());
        let mut d = Dashboard::new();
        d.on_enter(&c);
        assert_eq!(store.scheduled(), vec![RefreshTarget::Dashboard]);
        d.on_leave(&c);
        assert!(store.scheduled().is_empty());

        // And it reloads when the dashboard changes, not when the inbox does.
        assert!(d.cares_about(&StoreEvent::Updated(RefreshTarget::Dashboard)));
        assert!(!d.cares_about(&StoreEvent::Updated(RefreshTarget::Notifications)));
        assert!(d.cares_about(&StoreEvent::RateLimited {
            kind: LimitKind::Primary,
            until: FIXTURE_NOW + Duration::hours(1),
        }));
    }

    #[tokio::test]
    async fn a_failed_read_becomes_a_screen_rather_than_an_exit() {
        // `App` applies `?` to load(), so an Err here takes the program down
        // on a DNS failure.
        let store = FakeStore::with_corpus();
        store.set_behaviour(Behaviour::failing(StoreError::Offline("dns".into())));
        let (d, _) = loaded(store).await;
        assert!(d.error.is_some());
        assert_eq!(d.state(), SurfaceState::OfflineWithoutCache);
    }

    #[tokio::test]
    async fn cached_sections_survive_a_failed_refresh() {
        // Offline with a cache is a banner over content, not an empty screen.
        let store = Arc::new(FakeStore::with_corpus());
        let c = ctx(store.clone());
        let mut d = Dashboard::new();
        d.load(&c).await.unwrap();

        store.set_behaviour(Behaviour::failing(StoreError::Offline("dns".into())));
        d.load(&c).await.unwrap();
        assert_eq!(d.state(), SurfaceState::OfflineWithCache);

        let out = screen(&mut d, 80, 10);
        assert!(out.contains("Offline"), "the banner names the cause: {out}");
        assert!(
            out.contains("Needs my review"),
            "the sections survive it: {out}"
        );
    }

    #[tokio::test]
    async fn a_cold_cache_is_a_skeleton_rather_than_four_lies() {
        // The section list comes from config and exists before any fetch, so
        // its length is no evidence that counts were ever read.
        let store = FakeStore::with_corpus();
        store.set_behaviour(Behaviour::offline_without_cache());
        let (mut d, _) = loaded(store).await;
        assert_eq!(d.state(), SurfaceState::Cold);

        let out = screen(&mut d, 80, 10);
        assert!(out.contains("Loading"), "{out}");
        assert!(
            !out.contains("Needs my review"),
            "a count we do not have is not rendered as nothing: {out}"
        );
        assert!(d.title().contains("not fetched"), "{}", d.title());
    }

    #[tokio::test]
    async fn stale_is_admitted_in_the_header_even_though_the_shell_drops_it() {
        let store = FakeStore::with_corpus();
        store.set_behaviour(Behaviour::offline_with_cache());
        let (mut d, _) = loaded(store).await;
        assert_eq!(d.state(), SurfaceState::Stale);
        assert!(
            d.title().contains("cached"),
            "stale data is never hidden: {}",
            d.title()
        );
        let out = screen(&mut d, 80, 10);
        assert!(out.contains("Needs my review"), "shown normally: {out}");
    }

    #[tokio::test]
    async fn refreshing_does_not_replace_the_sections_with_a_spinner() {
        let store = FakeStore::with_corpus();
        store.set_behaviour(Behaviour {
            refreshing: true,
            ..Default::default()
        });
        let (mut d, _) = loaded(store).await;
        assert_eq!(d.state(), SurfaceState::Refreshing);
        let out = screen(&mut d, 80, 10);
        assert!(out.contains("Needs my review"), "{out}");
    }

    #[tokio::test]
    async fn every_section_reading_nothing_is_still_four_sections() {
        // "All caught up" on a dashboard is four ticks, not a blank screen:
        // the sections are configuration, and hiding them would hide the
        // queries that produced the good news.
        let store = FakeStore::with_corpus();
        store.set_behaviour(Behaviour {
            empty: true,
            ..Default::default()
        });
        let (mut d, _) = loaded(store).await;
        assert_eq!(d.state(), SurfaceState::Populated);
        let out = screen(&mut d, 80, 10);
        assert_eq!(out.matches(CLEAR).count(), 4, "{out}");
        assert!(d.title().starts_with("Dashboard  0 of 4"), "{}", d.title());
    }

    #[tokio::test]
    async fn a_config_with_no_sections_says_so() {
        // The only thing "empty" can mean here, and it is a config problem
        // rather than good news.
        let store = Arc::new(FakeStore::with_corpus());
        let c = ctx(store);
        let mut d = Dashboard {
            cfg: DashboardConfig { sections: vec![] },
            ..Default::default()
        };
        d.load(&c).await.unwrap();
        assert_eq!(d.state(), SurfaceState::Empty);
        let out = screen(&mut d, 80, 10);
        assert!(out.contains("No sections configured"), "{out}");
        assert!(matches!(
            d.on_key(key(KeyCode::Enter), &c),
            Outcome::Ignored
        ));
    }

    #[tokio::test]
    async fn the_query_of_the_focused_section_is_on_screen() {
        let (mut d, c) = loaded(FakeStore::with_corpus()).await;
        let out = screen(&mut d, 80, 10);
        assert!(out.contains("review-requested:@me"), "{out}");

        d.on_key(key(KeyCode::Char('j')), &c);
        let out = screen(&mut d, 80, 10);
        assert!(out.contains("author:@me"), "it follows the cursor: {out}");
    }

    #[test]
    fn a_short_terminal_keeps_the_sections_and_drops_the_query() {
        let mut d = mixed();
        // Four sections and no room for the detail line.
        let out = screen(&mut d, 80, 4);
        assert!(out.contains("Recently mentioned"), "{out}");
        assert!(!out.contains("review-requested"), "{out}");
    }

    #[test]
    fn no_line_ever_overflows_its_terminal() {
        // A line one cell too wide wraps, and a wrapped line silently halves
        // the list without failing anything.
        for icons in [Icons::UNICODE, Icons::ASCII] {
            for w in WIDTHS {
                let mut d = mixed().icons(icons);
                for line in screen(&mut d, w, 10).lines() {
                    assert!(line.chars().count() <= w as usize, "{line:?} at width {w}");
                }
            }
        }
    }

    #[test]
    fn a_long_section_title_elides_rather_than_pushing_the_count_off() {
        let cfg = DashboardConfig {
            sections: vec![DashboardSection {
                title: "Pull requests in a repository with an extremely long name \
                        that nobody would type twice"
                    .into(),
                query: "is:open is:pr".into(),
                limit: 10,
            }],
        };
        let sections = vec![DashboardSectionData {
            title: cfg.sections[0].title.clone(),
            query: cfg.sections[0].query.clone(),
            count: 42,
        }];
        let mut d = Dashboard {
            cfg,
            data: Some(Fresh::from_network(DashboardData { sections }, FIXTURE_NOW)),
            ..Default::default()
        };
        let out = screen(&mut d, 60, 10);
        let line = out.lines().next().unwrap();
        assert!(line.ends_with("42"), "the count survives: {line:?}");
        assert!(line.contains('…'), "the title elides: {line:?}");
    }

    // ------------------------------------------------------- snapshots

    /// The breakpoints of `spec/30-ui.md` §4.1, plus one either side.
    const WIDTHS: [u16; 8] = [40, 59, 60, 79, 80, 99, 100, 120];

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    /// How a scene sets the fake up: the behaviour to read under, and
    /// whether a good read happened first — a banner over content is only
    /// reachable when there is content.
    struct Scene {
        name: &'static str,
        behaviour: Behaviour,
        warm: bool,
    }

    fn scene(name: &'static str, behaviour: Behaviour) -> Scene {
        Scene {
            name,
            behaviour,
            warm: false,
        }
    }

    fn failing(name: &'static str, err: StoreError) -> Scene {
        scene(name, Behaviour::failing(err))
    }

    /// Every arm of §8 the dashboard can be in, each produced by `FakeStore`.
    ///
    /// `filtered-empty` is the one arm that does not apply: the dashboard has
    /// no filter, so nothing can reach it. `empty` is reachable only with a
    /// config that defines no sections, which is covered by its own test
    /// rather than here, where every scene shares one config.
    async fn state_matrix(icons: Icons) -> String {
        let scenes = [
            scene("cold", Behaviour::offline_without_cache()),
            scene("populated", Behaviour::default()),
            scene(
                "populated · every section clear",
                Behaviour {
                    empty: true,
                    ..Default::default()
                },
            ),
            scene("stale", Behaviour::offline_with_cache()),
            scene(
                "refreshing",
                Behaviour {
                    refreshing: true,
                    ..Default::default()
                },
            ),
            Scene {
                warm: true,
                ..failing(
                    "offline-with-cache",
                    StoreError::Offline("dns lookup failed".into()),
                )
            },
            failing(
                "offline-without-cache",
                StoreError::Offline("dns lookup failed".into()),
            ),
            failing(
                "rate-limited",
                StoreError::RateLimited {
                    kind: LimitKind::Primary,
                    at: FIXTURE_NOW + Duration::hours(1),
                },
            ),
            failing("forbidden", StoreError::Forbidden),
            failing(
                "error",
                StoreError::Upstream {
                    status: 502,
                    message: "bad gateway".into(),
                },
            ),
        ];

        let mut out = String::new();
        for s in scenes {
            let store = Arc::new(FakeStore::with_corpus());
            let c = ctx(store.clone());
            let mut d = Dashboard::new().icons(icons);
            if s.warm {
                d.load(&c).await.unwrap();
            }
            store.set_behaviour(s.behaviour);
            d.load(&c).await.unwrap();
            assert!(
                s.name.starts_with(d.state().name()),
                "the scene `{}` actually produced `{}`",
                s.name,
                d.state().name()
            );
            out.push_str(&format!("── {} · {} ──\n", s.name, d.title()));
            out.push_str(&render(88, 10, |f, a| d.render(f, a, &c)));
            out.push_str("\n\n");
        }
        out
    }

    #[tokio::test]
    async fn the_state_matrix_in_unicode() {
        insta::assert_snapshot!(
            "dashboard_state_matrix_unicode",
            state_matrix(Icons::UNICODE).await
        );
    }

    #[tokio::test]
    async fn the_state_matrix_in_ascii() {
        insta::assert_snapshot!(
            "dashboard_state_matrix_ascii",
            state_matrix(Icons::ASCII).await
        );
    }

    #[test]
    fn the_width_breakpoints() {
        let mut out = String::new();
        for icons in [Icons::UNICODE, Icons::ASCII] {
            for w in WIDTHS {
                let mut d = mixed().icons(icons);
                out.push_str(&format!(
                    "── {w} cols · {} ──\n",
                    if icons.unicode() { "unicode" } else { "ascii" }
                ));
                out.push_str(&screen(&mut d, w, 8));
                out.push_str("\n\n");
            }
        }
        insta::assert_snapshot!("dashboard_breakpoints", out);
    }
}

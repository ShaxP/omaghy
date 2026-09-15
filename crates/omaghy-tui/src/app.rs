//! The app shell: navigation stack, event loop, global keys, layout.

use crate::surfaces::notifications::Variants;
use crate::{
    keys::{self, GLOBAL_BINDINGS, Global},
    route::{Route, SurfaceId},
    surface::{Ctx, Outcome, Surface},
    surfaces, terminal,
    theme::{IconMode, Icons, Role},
    widgets,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt as _;
use omaghy_model::Result;
use omaghy_store::{RefreshTarget, Store, StoreEvent, query::DashboardConfig};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Clear, Paragraph},
};
use std::sync::Arc;
use time::OffsetDateTime;

/// Below this the layout is not attempted — a degraded layout nobody tested
/// is worse than saying so (`spec/30-ui.md` §4.1).
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 10;

/// One entry of the navigation stack. Each keeps its own state, so returning
/// from a detail view lands where you left the list.
struct Entry {
    surface: Box<dyn Surface>,
    id: SurfaceId,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("depth", &self.stack.len())
            .field("surface", &self.stack.last().map(|e| e.id))
            .field("help_open", &self.help_open)
            .finish_non_exhaustive()
    }
}

pub struct App {
    stack: Vec<Entry>,
    ctx: Ctx,
    help_open: bool,
    dirty: bool,
    quit: bool,
    status: Option<String>,
    /// Which refresh `status` is announcing, when it is announcing one.
    ///
    /// "Refreshing…" was cleared only by the next keypress, so it outlived
    /// the work it described: an idle terminal claimed to be refreshing for
    /// as long as you left it alone. Remembering the target clears the
    /// message when that target lands, without wiping a message about
    /// something else.
    progress: Option<RefreshTarget>,
}

impl App {
    pub fn new(store: Arc<dyn Store>, now: OffsetDateTime) -> Self {
        // Resolved once here rather than in each surface, so the ASCII
        // fallback is reachable at all (`40-config.md` §3).
        // `IconMode::resolve` is pure; reading the environment is the
        // caller's job, which is here.
        let icons = Icons::new(IconMode::resolve(
            None,
            std::env::var("TERM").ok().as_deref(),
            std::env::var("LANG").ok().as_deref(),
        ));
        let ctx = Ctx::new(store, now, icons);
        Self {
            stack: Vec::new(),
            ctx,
            help_open: false,
            dirty: true,
            quit: false,
            status: None,
            progress: None,
        }
    }

    /// Apply the configuration read at startup (`40-config.md` §1).
    ///
    /// Before `start`, because a surface takes its configuration when it is
    /// built. Separate from [`App::new`] so that the hundred tests which do
    /// not care about configuration keep a one-line constructor.
    #[must_use]
    pub fn with_config(mut self, inbox: Variants, dashboard: DashboardConfig) -> Self {
        self.ctx.inbox = inbox;
        self.ctx.dashboard = Arc::new(dashboard);
        self
    }

    pub async fn start(&mut self, route: Route) -> Result<()> {
        self.push(route).await
    }

    fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Which surface is on top, if any.
    pub fn current(&self) -> Option<SurfaceId> {
        self.stack.last().map(|e| e.id)
    }

    fn top(&mut self) -> &mut Box<dyn Surface> {
        &mut self
            .stack
            .last_mut()
            .expect("stack is never empty after start")
            .surface
    }

    async fn push(&mut self, route: Route) -> Result<()> {
        if let Some(e) = self.stack.last_mut() {
            e.surface.on_leave(&self.ctx);
        }
        let mut surface = surfaces::build(&route, &self.ctx);
        surface.on_enter(&self.ctx);
        surface.load(&self.ctx).await?;
        self.stack.push(Entry {
            surface,
            id: route.surface,
        });
        self.dirty = true;
        Ok(())
    }

    async fn replace(&mut self, route: Route) -> Result<()> {
        if let Some(mut e) = self.stack.pop() {
            e.surface.on_leave(&self.ctx);
        }
        self.push(route).await
    }

    fn pop(&mut self) {
        if self.stack.len() > 1
            && let Some(mut e) = self.stack.pop()
        {
            e.surface.on_leave(&self.ctx);
            self.dirty = true;
        }
    }

    // ------------------------------------------------------------ input

    async fn on_key(&mut self, key: crossterm::event::KeyEvent) -> Result<()> {
        // Windows reports press and release; act once.
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }
        if self.help_open {
            self.help_open = false;
            self.dirty = true;
            return Ok(());
        }

        // A surface taking free text gets everything except the two keys that
        // must always escape, or it would lose most of the alphabet mid-word.
        let raw = self.top().wants_raw_input();
        let escapes = matches!(key.code, KeyCode::Esc)
            || (key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c')));

        // Otherwise the surface gets first refusal on keys the globals do not
        // claim.
        if (!raw || escapes)
            && let Some(action) = keys::resolve(key, self.depth())
        {
            match action {
                Global::Quit => self.quit = true,
                Global::Back => self.pop(),
                Global::Help => self.help_open = true,
                Global::Palette => self.status = Some("Command palette arrives in W1.3".into()),
                Global::Refresh => match self.top().refresh_target() {
                    // The surface says what `r` means here; App used to
                    // hardcode the inbox, so `r` on the dashboard refreshed
                    // the wrong thing.
                    Some(target) => {
                        self.ctx.store.refresh(target.clone());
                        self.status = Some("Refreshing…".into());
                        self.progress = Some(target);
                    }
                    None => self.status = Some("Nothing to refresh here".into()),
                },
                Global::OpenInBrowser => {
                    self.status = Some("Opening in a browser arrives with the real surfaces".into())
                }
                Global::Surface(i) => {
                    // Re-entering the surface you are already on would discard
                    // its cursor and filter for no reason.
                    if let Some(id) = SurfaceId::ALL.get(i).copied()
                        && self.current() != Some(id)
                    {
                        self.replace(Route::surface(id)).await?;
                    }
                }
            }
            self.dirty = true;
            return Ok(());
        }

        let ctx = self.ctx.clone();
        match self.top().on_key(key, &ctx) {
            Outcome::Ignored => {}
            // Moving the cursor must not await a store read.
            Outcome::Redraw => self.dirty = true,
            Outcome::Reload => {
                self.top().load(&ctx).await?;
                self.dirty = true;
            }
            Outcome::Push(r) => self.push(r).await?,
            Outcome::Replace(r) => self.replace(r).await?,
            Outcome::Pop => self.pop(),
            Outcome::Quit => self.quit = true,
        }
        Ok(())
    }

    async fn on_store(&mut self, ev: StoreEvent) -> Result<()> {
        let ctx = self.ctx.clone();
        if self.top().cares_about(&ev) {
            if let StoreEvent::Updated(_) = ev {
                self.top().load(&ctx).await?;
            }
            self.dirty = true;
        }
        // The work "Refreshing…" announced has landed, one way or the other.
        //
        // **Only a terminal event ends it.** `Store::refresh` emits
        // `RefreshStarted` synchronously, so reacting to any event for the
        // target forgot what we were waiting for before the answer arrived,
        // and the message stayed until the next keypress — the very bug this
        // is here to fix, shipped once because the test fed `Updated` alone
        // instead of the sequence the loop really delivers.
        //
        // Matching on the target as well means a tick finishing elsewhere
        // does not clear a message about the refresh you asked for.
        let landed = matches!(
            ev,
            StoreEvent::Updated(_) | StoreEvent::RefreshFailed { .. }
        );
        if landed && self.progress.as_ref() == ev.target() {
            self.progress = None;
            if matches!(ev, StoreEvent::Updated(_)) {
                self.status = None;
            }
            self.dirty = true;
        }
        if let StoreEvent::RefreshFailed { error, .. } = &ev {
            self.status = Some(error.terse());
            self.dirty = true;
        }
        Ok(())
    }

    // ----------------------------------------------------------- render

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            // The message has to fit in the terminal it is complaining
            // about, so it wraps rather than being truncated into nonsense.
            f.render_widget(
                Paragraph::new(vec![
                    Line::raw("Terminal too small."),
                    Line::raw(format!("Needs {MIN_WIDTH}x{MIN_HEIGHT}.")),
                    Line::raw(format!("Have {}x{}.", area.width, area.height)),
                ])
                .wrap(ratatui::widgets::Wrap { trim: true }),
                area,
            );
            return;
        }

        let [head, body, foot] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);

        let title = self.top().title();
        let viewer = self.ctx.viewer().login.clone();
        let mut header = widgets::chrome::Header::new(&title, &viewer).icons(self.ctx.icons);
        if let Some(fr) = self.top().freshness() {
            header = header.freshness(fr);
        }
        header.render(f, head);

        let ctx = self.ctx.clone();
        self.top().render(f, body, &ctx);

        let hints = self.footer_hints(foot.width);
        let refs: Vec<(&str, &str)> = hints.iter().map(|(a, b)| (*a, *b)).collect();
        widgets::footer(f, foot, &refs);

        if let Some(msg) = &self.status {
            let w = (msg.len() as u16 + 2).min(area.width);
            let r = Rect {
                x: area.x,
                y: foot.y,
                width: w,
                height: 1,
            };
            f.render_widget(Clear, r);
            f.render_widget(
                Paragraph::new(Line::styled(format!(" {msg}"), Role::Warning.style())),
                r,
            );
        }

        if self.help_open {
            self.render_help(f, area);
        }
    }

    /// Contextual bindings, with the way out reserved first.
    ///
    /// These used to be concatenated and the overflow dropped from the end,
    /// which took `q` — the only documented way to quit — off the screen once
    /// a surface had more than about six bindings. Being stuck with no visible
    /// exit is the worst thing this footer can do, so `? help` and `q quit`
    /// are placed first and the surface's own bindings fill what is left.
    /// Found by W2.3.
    fn footer_hints(&mut self, width: u16) -> Vec<(&'static str, &'static str)> {
        const ESCAPE: [(&str, &str); 2] = [("?", "help"), ("q", "quit")];
        let cost = |(k, d): &(&str, &str)| k.chars().count() + 1 + d.chars().count() + 2;

        let reserved: usize = ESCAPE.iter().map(cost).sum();
        let mut budget = (width as usize).saturating_sub(reserved + 1);

        let mut v: Vec<(&'static str, &'static str)> = Vec::new();
        for b in self.top().keymap() {
            let pair = (b.keys, b.description);
            let c = cost(&pair);
            if c > budget {
                break;
            }
            budget -= c;
            v.push(pair);
        }
        v.extend(ESCAPE);
        v
    }

    fn render_help(&mut self, f: &mut Frame, area: Rect) {
        let mut lines = vec![Line::styled("  Keys", Role::Accent.style()), Line::raw("")];
        // Generated from the keymap, never written — a help screen maintained
        // separately is a help screen that lies.
        for b in self.top().keymap() {
            lines.push(Line::raw(format!("  {:<12} {}", b.keys, b.description)));
        }
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        for b in GLOBAL_BINDINGS {
            lines.push(Line::raw(format!("  {:<12} {}", b.keys, b.description)));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled("  any key to close", Role::Muted.style()));

        let h = (lines.len() as u16 + 2).min(area.height);
        let w = 48.min(area.width);
        let r = Rect {
            x: area.x + (area.width.saturating_sub(w)) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, r);
        f.render_widget(
            Paragraph::new(lines).block(ratatui::widgets::Block::bordered().title(" omaghy ")),
            r,
        );
    }

    // ------------------------------------------------------------- loop

    pub async fn run(&mut self, tui: &mut terminal::Tui) -> Result<()> {
        let mut events = crossterm::event::EventStream::new();
        let mut store_rx = self.ctx.store.subscribe();

        loop {
            if self.dirty {
                tui.draw(|f| self.render(f))
                    .map_err(|e| omaghy_model::StoreError::Offline(format!("draw failed: {e}")))?;
                self.dirty = false;
            }
            if self.quit {
                return Ok(());
            }

            tokio::select! {
                Some(Ok(ev)) = events.next() => match ev {
                    Event::Key(k) => {
                        self.status = None;
                        self.progress = None;
                        self.on_key(k).await?;
                    }
                    Event::Resize(_, _) => self.dirty = true,
                    _ => {}
                },
                Ok(ev) = store_rx.recv() => self.on_store(ev).await?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Binding;
    use crossterm::event::KeyEvent;
    use omaghy_store::{FakeStore, RefreshTarget};
    use std::sync::Arc;

    fn app() -> App {
        App::new(
            Arc::new(FakeStore::with_corpus()),
            omaghy_store::fake::FIXTURE_NOW,
        )
    }

    #[tokio::test]
    async fn the_way_out_survives_a_surface_with_many_bindings() {
        // The footer used to concatenate every binding and drop the overflow,
        // taking `q` — the only documented way to quit — off the screen.
        let mut a = app();
        a.start(Route::surface(SurfaceId::Notifications))
            .await
            .unwrap();

        for width in [40u16, 60, 80, 100, 200] {
            let hints = a.footer_hints(width);
            assert!(
                hints.iter().any(|(k, _)| *k == "q"),
                "no way out at {width} columns: {hints:?}"
            );
            assert!(
                hints.iter().any(|(k, _)| *k == "?"),
                "no help at {width} columns: {hints:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_narrow_footer_sheds_surface_bindings_not_the_exit() {
        let mut a = app();
        a.start(Route::surface(SurfaceId::Notifications))
            .await
            .unwrap();

        let wide = a.footer_hints(200).len();
        let narrow = a.footer_hints(40).len();
        assert!(
            narrow < wide,
            "narrow should carry fewer hints: {narrow} vs {wide}"
        );
        assert!(narrow >= 2, "the two escape hints are never shed");
    }

    /// Reported from a smoke test: "Refreshing…" stayed on screen for ever.
    ///
    /// It was cleared only by the next keypress, so an idle terminal claimed
    /// to be refreshing long after the refresh had landed.
    ///
    /// **This test pumps the events the store really emitted**, in order,
    /// rather than the one event the assertion is about. The first fix for
    /// this shipped broken precisely because a hand-written `Updated` skipped
    /// the `RefreshStarted` that `Store::refresh` emits synchronously — and
    /// reacting to that one threw away what we were waiting for. A test that
    /// picks its own events cannot catch a bug about which events arrive.
    #[tokio::test]
    async fn the_refreshing_message_ends_when_the_refresh_does() {
        let store = Arc::new(FakeStore::with_corpus());
        let mut a = App::new(store.clone(), omaghy_store::fake::FIXTURE_NOW);
        a.start(Route::surface(SurfaceId::Notifications))
            .await
            .unwrap();

        // Subscribed after `start`, so entering the surface is not in the
        // queue we are about to drain.
        let mut rx = store.subscribe();
        a.on_key(KeyEvent::from(crossterm::event::KeyCode::Char('r')))
            .await
            .unwrap();
        assert_eq!(a.status.as_deref(), Some("Refreshing…"));

        let mut pumped = 0;
        while let Ok(ev) = rx.try_recv() {
            a.on_store(ev).await.unwrap();
            pumped += 1;
        }
        assert!(
            pumped >= 2,
            "a refresh emits a start and a landing; got {pumped}"
        );
        assert_eq!(
            a.status, None,
            "the refresh landed; nothing should still claim it is running"
        );
    }

    /// And a landing elsewhere does not clear it: the dashboard's poll tick
    /// finishing says nothing about the inbox refresh you asked for.
    #[tokio::test]
    async fn another_targets_refresh_does_not_clear_the_message() {
        let store = Arc::new(FakeStore::with_corpus());
        let mut a = App::new(store.clone(), omaghy_store::fake::FIXTURE_NOW);
        a.start(Route::surface(SurfaceId::Notifications))
            .await
            .unwrap();

        a.on_key(KeyEvent::from(crossterm::event::KeyCode::Char('r')))
            .await
            .unwrap();
        a.on_store(StoreEvent::RefreshStarted(RefreshTarget::Dashboard))
            .await
            .unwrap();
        a.on_store(StoreEvent::Updated(RefreshTarget::Dashboard))
            .await
            .unwrap();
        assert_eq!(a.status.as_deref(), Some("Refreshing…"));
    }

    #[tokio::test]
    async fn r_refreshes_what_the_surface_says_not_always_the_inbox() {
        let store = Arc::new(FakeStore::with_corpus());
        let mut a = App::new(store.clone(), omaghy_store::fake::FIXTURE_NOW);

        a.start(Route::surface(SurfaceId::Dashboard)).await.unwrap();
        let before = store.scheduled().len();
        a.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
            .await
            .unwrap();
        let scheduled = store.scheduled();
        assert!(
            scheduled.len() > before,
            "`r` scheduled nothing on the dashboard"
        );
        assert!(
            !matches!(scheduled.last(), Some(RefreshTarget::Notifications)),
            "`r` on the dashboard must not refresh the inbox: {scheduled:?}"
        );
    }

    #[test]
    fn every_global_binding_is_dotted_and_described() {
        for b in GLOBAL_BINDINGS {
            let _: &Binding = b;
            assert!(b.action.contains('.'), "{} should be dotted", b.action);
            assert!(!b.description.is_empty());
        }
    }
}

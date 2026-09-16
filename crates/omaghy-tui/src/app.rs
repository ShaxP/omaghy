//! The app shell: navigation stack, event loop, global keys, layout.

use crate::surfaces::notifications::Variants;
use crate::{
    keys::Binding,
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
    settings_open: bool,
    settings: crate::widgets::settings::Settings,
    /// The configuration as it stands, including changes made this session.
    config: crate::config::Config,
    /// Why the last change could not be saved, if it could not (§6.3).
    settings_note: Option<String>,
    /// The command palette, while it is open (`30-ui.md` §5.1).
    palette: Option<crate::widgets::Palette>,
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
            settings_open: false,
            settings: crate::widgets::settings::Settings::new(),
            config: crate::config::Config::default(),
            settings_note: None,
            palette: None,
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
        self.config.inbox = inbox;
        self.config.dashboard = (*self.ctx.dashboard).clone();
        self
    }

    /// The whole configuration, as read from the file.
    ///
    /// Carries the provenance the settings surface shows, which
    /// [`App::with_config`] cannot — it takes only the two values surfaces
    /// need. Both exist because most tests want the short one.
    #[must_use]
    pub fn with_settings(mut self, config: crate::config::Config) -> Self {
        self.ctx.inbox = config.inbox;
        self.ctx.dashboard = Arc::new(config.dashboard.clone());
        self.config = config;
        self
    }

    /// Where a changed setting is persisted. Defaults to keeping nothing.
    #[must_use]
    pub fn with_config_writer(mut self, writer: Arc<dyn crate::config::ConfigWriter>) -> Self {
        self.ctx.config_writer = writer;
        self
    }

    /// The configuration as it stands, for a caller that needs to see a
    /// change made from the settings surface — the poll loop's intervals, say.
    pub fn config(&self) -> &crate::config::Config {
        &self.config
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
        if self.settings_open {
            self.on_settings_key(key);
            self.dirty = true;
            return Ok(());
        }
        if self.palette.is_some() {
            return self.on_palette_key(key).await;
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
                Global::Settings => {
                    self.settings_open = true;
                    self.settings_note = None;
                }
                Global::Palette => self.open_palette(),
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

    // ---------------------------------------------------------- palette

    /// Everything runnable from here, by name.
    ///
    /// The globals, one entry per surface, and the surface's own keymap — the
    /// same [`Binding`] slices the footer and the help overlay read, so an
    /// action cannot exist without being searchable and a searchable action
    /// cannot fail to exist.
    fn palette_actions(&mut self) -> Vec<Binding> {
        let mut actions: Vec<Binding> = keys::GLOBAL_BINDINGS.to_vec();
        actions.extend(keys::surface_bindings());
        actions.extend(self.top().keymap().iter().cloned());
        // `1–7` is a row in the help overlay, not something to search for; the
        // seven named entries above replace it.
        actions.retain(|b| b.action != "app.surface");
        actions
    }

    fn open_palette(&mut self) {
        let actions = self.palette_actions();
        self.palette = Some(crate::widgets::Palette::new(actions).icons(self.ctx.icons));
    }

    /// Run an action by name.
    ///
    /// **By pressing its key.** `30-ui.md` §5.1 requires that anything
    /// reachable by key be reachable by name; resolving the name to the key
    /// and replaying it makes that true by construction rather than by a
    /// second implementation that has to be kept honest. A name with no key
    /// cannot be run, and a test asserts there are none.
    async fn run_action(&mut self, action: &str) -> Result<()> {
        let event = self
            .palette_actions()
            .into_iter()
            .find(|b| b.action == action)
            .and_then(|b| b.run_event());
        match event {
            Some(ev) => Box::pin(self.on_key(ev)).await,
            None => {
                self.status = Some(format!("`{action}` has no key to press"));
                Ok(())
            }
        }
    }

    async fn on_palette_key(&mut self, key: crossterm::event::KeyEvent) -> Result<()> {
        use crate::widgets::PaletteOutcome;

        let Some(p) = &mut self.palette else {
            return Ok(());
        };
        self.dirty = true;
        match p.on_key(key) {
            PaletteOutcome::Consumed => Ok(()),
            PaletteOutcome::Dismissed => {
                self.palette = None;
                Ok(())
            }
            PaletteOutcome::Run(action) => {
                // Closed *before* running: the action may open the settings
                // panel or push a surface, and both would arrive underneath a
                // palette that is still on screen.
                self.palette = None;
                self.run_action(action).await
            }
        }
    }

    // --------------------------------------------------------- settings

    /// Keys while the settings panel is open.
    ///
    /// It takes everything: it is a modal panel, and a `j` that fell through
    /// to the list behind it would move a cursor the user cannot see.
    fn on_settings_key(&mut self, key: crossterm::event::KeyEvent) {
        use crate::widgets::settings::SettingsOutcome;

        let outcome = match key.code {
            KeyCode::Char(',') | KeyCode::Esc | KeyCode::Char('q') => SettingsOutcome::Dismissed,
            KeyCode::Char('j') | KeyCode::Down => {
                self.settings.next();
                SettingsOutcome::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.settings.prev();
                SettingsOutcome::Consumed
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                let id = self.settings.selected();
                self.change(id, true);
                SettingsOutcome::Changed(id)
            }
            KeyCode::Char('h') | KeyCode::Left => {
                let id = self.settings.selected();
                self.change(id, false);
                SettingsOutcome::Changed(id)
            }
            _ => SettingsOutcome::Consumed,
        };

        if outcome == SettingsOutcome::Dismissed {
            self.settings_open = false;
            self.settings_note = None;
        }
    }

    /// Cycle one setting, apply it, and persist it.
    ///
    /// The order matters: applying comes first and never depends on the write
    /// succeeding. §6.3 — a change that cannot be saved still applies for the
    /// session, and says so.
    fn change(&mut self, id: crate::config::SettingId, forward: bool) {
        use crate::config::Source;

        let scalar = id.cycle(&mut self.config, forward);

        // Apply: into the context every surface reads, then into the surfaces
        // already built, which took their copy when they were constructed.
        self.ctx.inbox = self.config.inbox;
        self.ctx.dashboard = Arc::new(self.config.dashboard.clone());
        let ctx = self.ctx.clone();
        for entry in &mut self.stack {
            entry.surface.reconfigure(&ctx);
        }
        self.dirty = true;

        // Persist. Only what differs from the default is written, and a
        // setting returning to its default takes its key with it (§6.3).
        let value = if id.is_default(&self.config) {
            None
        } else {
            Some(scalar)
        };
        match self.ctx.config_writer.write(id.section(), id.key(), value) {
            Ok(()) => {
                self.settings_note = None;
                // Written, so it is the file's now — and a value back at its
                // default is the default's again, not "this session".
                let source = if id.is_default(&self.config) {
                    Source::Default
                } else {
                    Source::File
                };
                id.set_source_public(&mut self.config, source);
            }
            Err(e) => {
                id.set_source_public(&mut self.config, Source::Unsaved);
                self.settings_note = Some(format!("applied, but not saved — {e}"));
            }
        }
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
        if self.settings_open {
            self.settings
                .render(f, area, &self.config, self.settings_note.as_deref());
        }
        if let Some(p) = &self.palette {
            p.render(f, area);
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
    // ------------------------------------------------------- the settings panel

    mod palette {
        use super::super::*;
        use crossterm::event::KeyEvent;
        use omaghy_store::{FakeStore, Store};
        use ratatui::{Terminal, backend::TestBackend};

        async fn app_on(id: SurfaceId) -> App {
            let store: Arc<dyn Store> = Arc::new(FakeStore::with_corpus());
            let mut app = App::new(store, omaghy_store::fake::FIXTURE_NOW);
            app.start(Route::surface(id)).await.expect("surface loads");
            app
        }

        async fn press(app: &mut App, c: char) {
            app.on_key(KeyEvent::from(KeyCode::Char(c))).await.unwrap();
        }

        async fn type_in(app: &mut App, text: &str) {
            for c in text.chars() {
                press(app, c).await;
            }
        }

        fn draw(app: &mut App) -> String {
            let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
            t.draw(|f| app.render(f)).unwrap();
            let buf = t.backend().buffer().clone();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        /// The invariant `30-ui.md` §5.1 is built on: *anything reachable by
        /// key must be reachable by name*.
        ///
        /// Running an action replays its key, so the only way to break this is
        /// a binding with no key at all. There must be none.
        #[tokio::test]
        async fn every_action_the_palette_offers_can_actually_be_run() {
            for id in [SurfaceId::Dashboard, SurfaceId::Notifications] {
                let mut app = app_on(id).await;
                let actions = app.palette_actions();
                assert!(!actions.is_empty());
                for b in &actions {
                    assert!(
                        b.run_event().is_some(),
                        "`{}` is offered by name on {id:?} but has no key to press",
                        b.action
                    );
                }
            }
        }

        /// The other half of §5.1, from the palette's own side: no action may
        /// be nameless.
        #[tokio::test]
        async fn no_action_is_unreachable_by_name() {
            let mut app = app_on(SurfaceId::Notifications).await;
            let actions = app.palette_actions();
            let bad = crate::widgets::unreachable_by_name(&actions);
            assert!(bad.is_empty(), "not addressable by name: {bad:?}");
        }

        #[tokio::test]
        async fn colon_opens_it_and_it_lists_this_surface_s_actions() {
            let app = &mut app_on(SurfaceId::Notifications).await;
            press(app, ':').await;
            let out = draw(app);
            assert!(
                out.contains("notification."),
                "the inbox's own actions should be there:\n{out}"
            );
            assert!(out.contains("app.quit"), "and the globals:\n{out}");
        }

        /// `40-config.md` §6: settings is reached "by `,`, **and from the
        /// command palette**".
        #[tokio::test]
        async fn settings_is_reachable_by_name() {
            let app = &mut app_on(SurfaceId::Notifications).await;
            press(app, ':').await;
            type_in(app, "settings").await;
            app.on_key(KeyEvent::from(KeyCode::Enter)).await.unwrap();

            assert!(app.palette.is_none(), "the palette closes behind itself");
            let out = draw(app);
            assert!(
                out.contains("Settings"),
                "running `app.settings` by name should open the panel:\n{out}"
            );
        }

        /// Running by name does exactly what pressing the key does — the
        /// property that makes replaying the key the right implementation.
        #[tokio::test]
        async fn running_an_action_by_name_matches_pressing_its_key() {
            let by_key = {
                let app = &mut app_on(SurfaceId::Notifications).await;
                press(app, 'j').await;
                press(app, 'j').await;
                draw(app)
            };
            let by_name = {
                let app = &mut app_on(SurfaceId::Notifications).await;
                for _ in 0..2 {
                    press(app, ':').await;
                    type_in(app, "notification.next").await;
                    app.on_key(KeyEvent::from(KeyCode::Enter)).await.unwrap();
                }
                draw(app)
            };
            assert_eq!(by_key, by_name, "the name and the key must agree");
        }

        /// A surface is reachable by its own name, not by "jump to surface".
        #[tokio::test]
        async fn a_surface_can_be_opened_by_name() {
            let app = &mut app_on(SurfaceId::Notifications).await;
            press(app, ':').await;
            type_in(app, "pull-requests").await;
            app.on_key(KeyEvent::from(KeyCode::Enter)).await.unwrap();
            assert_eq!(app.current(), Some(SurfaceId::PullRequests));
        }

        #[tokio::test]
        async fn escape_closes_it_and_runs_nothing() {
            let app = &mut app_on(SurfaceId::Notifications).await;
            let before = draw(app);
            press(app, ':').await;
            app.on_key(KeyEvent::from(KeyCode::Esc)).await.unwrap();
            assert!(app.palette.is_none());
            assert_eq!(draw(app), before, "dismissing changes nothing");
        }
    }

    mod settings {
        use super::super::*;
        use crate::config::{Config, ConfigWriter, Scalar, SettingId, Source};
        use crossterm::event::KeyEvent;
        use omaghy_store::{FakeStore, Store};
        use ratatui::{Terminal, backend::TestBackend};
        use std::sync::Mutex;

        /// A writer that remembers, and can be told to fail.
        #[derive(Debug)]
        struct Recording {
            writes: Mutex<Vec<(String, String, Option<Scalar>)>>,
            fail: bool,
            error: String,
        }

        impl Default for Recording {
            fn default() -> Self {
                Self {
                    writes: Mutex::new(Vec::new()),
                    fail: false,
                    error: "the disk is read-only".to_owned(),
                }
            }
        }

        impl Recording {
            fn failing() -> Self {
                Self {
                    fail: true,
                    ..Self::default()
                }
            }

            /// Fails with the message a read-only config directory really
            /// produces — long, and mostly path.
            fn failing_with(error: &str) -> Self {
                Self {
                    fail: true,
                    error: error.to_owned(),
                    ..Self::default()
                }
            }
            fn writes(&self) -> Vec<(String, String, Option<Scalar>)> {
                self.writes.lock().unwrap().clone()
            }
        }

        impl ConfigWriter for Recording {
            fn write(&self, section: &str, key: &str, value: Option<Scalar>) -> Result<(), String> {
                self.writes
                    .lock()
                    .unwrap()
                    .push((section.to_owned(), key.to_owned(), value));
                if self.fail {
                    Err(self.error.clone())
                } else {
                    Ok(())
                }
            }
        }

        async fn app_with(writer: Arc<Recording>) -> App {
            let store: Arc<dyn Store> = Arc::new(FakeStore::with_corpus());
            let mut app = App::new(store, omaghy_store::fake::FIXTURE_NOW)
                .with_settings(Config::default())
                .with_config_writer(writer);
            app.start(Route::surface(SurfaceId::Notifications))
                .await
                .expect("surface loads");
            app
        }

        async fn press(app: &mut App, c: char) {
            send(app, KeyEvent::from(KeyCode::Char(c))).await;
        }

        async fn send(app: &mut App, key: KeyEvent) {
            app.on_key(key).await.expect("a key never fails here");
        }

        fn draw(app: &mut App) -> String {
            let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
            t.draw(|f| app.render(f)).unwrap();
            let buf = t.backend().buffer().clone();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        #[tokio::test]
        async fn comma_opens_it_and_every_setting_is_listed() {
            let app = &mut app_with(Arc::new(Recording::default())).await;
            press(app, ',').await;
            let out = draw(app);

            for id in SettingId::ALL {
                assert!(
                    out.contains(id.key()),
                    "`{}` should be on the panel:\n{out}",
                    id.key()
                );
            }
            assert!(out.contains("settings"), "titled:\n{out}");
        }

        /// §6.1: each row states where its value came from.
        #[tokio::test]
        async fn a_row_says_where_its_value_came_from() {
            let store: Arc<dyn Store> = Arc::new(FakeStore::with_corpus());
            let mut cfg = Config::default();
            cfg.sources.group = Source::File;
            let mut app = App::new(store, omaghy_store::fake::FIXTURE_NOW).with_settings(cfg);
            app.start(Route::surface(SurfaceId::Notifications))
                .await
                .unwrap();

            press(&mut app, ',').await;
            let out = draw(&mut app);
            assert!(
                out.contains("config.toml"),
                "the file's value says so:\n{out}"
            );
            assert!(
                out.contains("default"),
                "and the others say default:\n{out}"
            );
        }

        /// §6.2: a change takes effect on the frame after it, with no restart —
        /// and the inbox *behind* the panel is what has to change.
        #[tokio::test]
        async fn a_change_redraws_the_inbox_behind_the_panel() {
            let app = &mut app_with(Arc::new(Recording::default())).await;
            let before = draw(app);

            press(app, ',').await;
            // Down to `rows`, then change it.
            for _ in 0..3 {
                press(app, 'j').await;
            }
            assert_eq!(app.config().inbox.rows.label(), "two-line");
            press(app, 'l').await;
            assert_eq!(
                app.config().inbox.rows.label(),
                "one-line",
                "the setting itself changed"
            );

            // Close the panel: what is underneath must have been reconfigured.
            press(app, ',').await;
            let after = draw(app);
            assert_ne!(
                before, after,
                "the inbox behind the panel should be drawing differently"
            );
        }

        #[tokio::test]
        async fn a_change_is_written_immediately() {
            let writer = Arc::new(Recording::default());
            let app = &mut app_with(writer.clone()).await;

            press(app, ',').await;
            for _ in 0..3 {
                press(app, 'j').await;
            }
            press(app, 'l').await;

            let writes = writer.writes();
            assert_eq!(writes.len(), 1, "one change, one write: {writes:?}");
            assert_eq!(writes[0].0, "notifications");
            assert_eq!(writes[0].1, "rows");
            assert_eq!(writes[0].2, Some(Scalar::Str("one-line".into())));
        }

        /// §6.3: only settings that differ from the default are written, so
        /// cycling back to the default removes the key rather than writing it.
        #[tokio::test]
        async fn returning_to_the_default_asks_for_the_key_to_be_removed() {
            let writer = Arc::new(Recording::default());
            let app = &mut app_with(writer.clone()).await;

            press(app, ',').await;
            for _ in 0..3 {
                press(app, 'j').await;
            }
            press(app, 'l').await; // two-line -> one-line
            press(app, 'l').await; // one-line -> two-line, the default again

            let writes = writer.writes();
            assert_eq!(writes.len(), 2);
            assert_eq!(writes[1].2, None, "back at the default: remove the key");
            assert_eq!(
                SettingId::Rows.source(app.config()),
                Source::Default,
                "and the row says default again, not `this session`"
            );
        }

        /// §6.3: if the file cannot be written the change still applies for the
        /// session, and the surface says it could not be saved.
        #[tokio::test]
        async fn a_failed_write_still_applies_and_says_so() {
            let app = &mut app_with(Arc::new(Recording::failing())).await;

            press(app, ',').await;
            for _ in 0..3 {
                press(app, 'j').await;
            }
            press(app, 'l').await;

            assert_eq!(
                app.config().inbox.rows.label(),
                "one-line",
                "the change is not refused"
            );
            assert_eq!(SettingId::Rows.source(app.config()), Source::Unsaved);

            let out = draw(app);
            assert!(
                out.contains("not saved"),
                "it must not fail silently:\n{out}"
            );
            assert!(out.contains("read-only"), "and it names why:\n{out}");
        }

        /// Reported from a smoke test: the panel showed
        /// `applied, but not saved — /home/…/config.toml could ` and the rest
        /// of the sentence — including *why* — was off the edge.
        ///
        /// A `Paragraph` does not wrap, so a note longer than the panel was
        /// simply cut, and the reason is the end of that sentence.
        #[tokio::test]
        async fn a_long_failure_reason_wraps_instead_of_being_cut() {
            let reason = "/home/shahram/.config/omaghy/config.toml could not be \
                          written: Permission denied (os error 13)";
            let app = &mut app_with(Arc::new(Recording::failing_with(reason))).await;

            press(app, ',').await;
            for _ in 0..3 {
                press(app, 'j').await;
            }
            press(app, 'l').await;

            let out = draw(app);
            assert!(
                out.contains("Permission denied"),
                "the reason is the point of the message:\n{out}"
            );
            assert!(
                out.contains("os error 13"),
                "and it must not stop before the end:\n{out}"
            );
            for line in out.lines() {
                assert!(
                    line.chars().count() <= 100,
                    "a line overflowed the terminal: {line}"
                );
            }
        }

        #[tokio::test]
        async fn h_and_l_cycle_in_opposite_directions() {
            let app = &mut app_with(Arc::new(Recording::default())).await;
            press(app, ',').await;
            press(app, 'j').await; // reason

            press(app, 'l').await;
            let forward = app.config().inbox.reason.label().to_owned();
            press(app, 'h').await;
            assert_eq!(
                app.config().inbox.reason.label(),
                "glyph",
                "`h` should undo `l`, got {forward} then this"
            );
        }

        #[tokio::test]
        async fn escape_closes_it_without_touching_the_surface_underneath() {
            let app = &mut app_with(Arc::new(Recording::default())).await;
            let before = draw(app);
            press(app, ',').await;
            send(app, KeyEvent::from(KeyCode::Esc)).await;
            assert_eq!(draw(app), before, "closing changes nothing");
        }
    }
}

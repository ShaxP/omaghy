//! The app shell: navigation stack, event loop, global keys, layout.

use crate::{
    keys::{self, Binding, GLOBAL_BINDINGS, Global},
    route::{Route, SurfaceId},
    surface::{Ctx, Outcome, Surface},
    surfaces, terminal,
    theme::Role,
    widgets,
};
use crossterm::event::{Event, KeyEventKind};
use futures_util::StreamExt as _;
use omaghy_model::Result;
use omaghy_store::{RefreshTarget, Store, StoreEvent};
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
}

impl App {
    pub fn new(store: Arc<dyn Store>, now: OffsetDateTime) -> Self {
        let ctx = Ctx { store, now };
        Self {
            stack: Vec::new(),
            ctx,
            help_open: false,
            dirty: true,
            quit: false,
            status: None,
        }
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
        let mut surface = surfaces::build(route.surface);
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

        // The surface gets first refusal on keys the globals do not claim.
        if let Some(action) = keys::resolve(key, self.depth()) {
            match action {
                Global::Quit => self.quit = true,
                Global::Back => self.pop(),
                Global::Help => self.help_open = true,
                Global::Palette => self.status = Some("Command palette arrives in W1.3".into()),
                Global::Refresh => {
                    self.ctx.store.refresh(RefreshTarget::Notifications);
                    self.status = Some("Refreshing…".into());
                }
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
            Outcome::Redraw => {
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
        widgets::header::<()>(f, head, &title, &viewer, None);

        let ctx = self.ctx.clone();
        self.top().render(f, body, &ctx);

        let hints = self.footer_hints();
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

    fn footer_hints(&mut self) -> Vec<(&'static str, &'static str)> {
        let mut v: Vec<_> = self
            .top()
            .keymap()
            .iter()
            .map(|b: &Binding| (b.keys, b.description))
            .collect();
        v.push(("?", "help"));
        v.push(("q", "quit"));
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
                    Event::Key(k) => { self.status = None; self.on_key(k).await?; }
                    Event::Resize(_, _) => self.dirty = true,
                    _ => {}
                },
                Ok(ev) = store_rx.recv() => self.on_store(ev).await?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        }
    }
}

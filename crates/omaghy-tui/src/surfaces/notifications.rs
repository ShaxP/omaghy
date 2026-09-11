//! The notifications inbox.
//!
//! **This is a shell-era placeholder, not the real surface.** It exists to
//! prove the Store reaches the screen and that the cursor, the state matrix
//! and the keymap are wired. The real one — triage, filtering, grouping,
//! responsive columns — is W2.3.

use crate::{
    keys::Binding,
    surface::{Ctx, Outcome, Surface},
    theme::Role,
    widgets,
};
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent};
use omaghy_model::{Notification, Result, age};
use omaghy_store::{Fresh, NotificationQuery, Page, ReadFilter, RefreshTarget, StoreEvent};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem, ListState},
};

const BINDINGS: &[Binding] = &[
    Binding::new("notification.next", "j / ↓", "next"),
    Binding::new("notification.prev", "k / ↑", "previous"),
    Binding::new("notification.toggle-read", "Enter", "toggle read"),
    Binding::new("notification.unread-only", "u", "unread only"),
];

#[derive(Debug, Default)]
pub struct Notifications {
    page: Option<Fresh<Page<Notification>>>,
    query: NotificationQuery,
    cursor: usize,
    list: ListState,
}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    fn rows(&self) -> &[Notification] {
        self.page
            .as_ref()
            .map(|p| p.value.items.as_slice())
            .unwrap_or_default()
    }

    fn move_cursor(&mut self, delta: isize) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        let next = (self.cursor as isize + delta).clamp(0, n as isize - 1);
        self.cursor = next as usize;
        self.list.select(Some(self.cursor));
    }
}

#[async_trait]
impl Surface for Notifications {
    fn title(&self) -> String {
        match self.page.as_ref() {
            Some(p) => {
                let unread = p.value.items.iter().filter(|n| n.unread).count();
                format!("Notifications  {unread} unread · {} total", p.value.len())
            }
            None => "Notifications".into(),
        }
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let Some(page) = self.page.as_ref() else {
            widgets::empty_state(f, area, "Loading…", "Reading from cache.");
            return;
        };
        if page.value.is_empty() {
            // Filtered-to-empty is a different state from empty.
            if self.query.read == ReadFilter::UnreadOnly {
                widgets::empty_state(f, area, "Nothing unread", "Press u to show everything.");
            } else if page.fetched_at.is_none() {
                widgets::empty_state(
                    f,
                    area,
                    "No data yet",
                    "Nothing cached, and no fetch has completed.",
                );
            } else {
                widgets::empty_state(f, area, "All caught up", "Nothing needs your attention.");
            }
            return;
        }

        let items: Vec<ListItem> = page
            .value
            .items
            .iter()
            .map(|n| {
                let marker = if n.unread { "\u{2503}" } else { " " };
                let age = age::relative(n.updated_at, ctx.now);
                let title_style = if n.unread {
                    Role::Unread.style()
                } else {
                    Role::Muted.style()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(marker, Role::Accent.style()),
                    Span::raw(" "),
                    Span::styled(n.title.clone(), title_style),
                    Span::raw("  "),
                    Span::styled(n.repo.to_string(), Role::Muted.style()),
                    Span::raw("  "),
                    Span::styled(age, Role::Muted.style()),
                ]))
            })
            .collect();

        self.list.select(Some(self.cursor));
        f.render_stateful_widget(
            List::new(items).highlight_style(Role::Selected.style()),
            area,
            &mut self.list,
        );
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
            KeyCode::Char('u') => {
                self.query.read = match self.query.read {
                    ReadFilter::All => ReadFilter::UnreadOnly,
                    ReadFilter::UnreadOnly => ReadFilter::All,
                };
                self.cursor = 0;
                // The router reloads on Redraw; W2.3 will make this explicit.
                Outcome::Redraw
            }
            _ => Outcome::Ignored,
        }
    }

    async fn load(&mut self, ctx: &Ctx) -> Result<()> {
        let page = ctx.store.notifications(&self.query).await?;
        self.cursor = self.cursor.min(page.value.len().saturating_sub(1));
        self.page = Some(page);
        Ok(())
    }

    fn cares_about(&self, ev: &StoreEvent) -> bool {
        ev.is_global() || matches!(ev.target(), Some(RefreshTarget::Notifications))
    }

    fn keymap(&self) -> &[Binding] {
        BINDINGS
    }

    fn on_enter(&mut self, ctx: &Ctx) {
        ctx.store.refresh(RefreshTarget::Notifications);
    }

    fn on_leave(&mut self, ctx: &Ctx) {
        ctx.store.cancel(&RefreshTarget::Notifications);
    }
}

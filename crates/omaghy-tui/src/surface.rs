//! What a surface is.
//!
//! A surface never renders another surface and never reaches into the router.
//! Navigation is expressed as an [`Outcome`] the router executes.
//!
//! See `spec/30-ui.md` §3.

use crate::widgets::chrome::Freshness;
use crate::{keys::Binding, route::Route, theme::Icons};
use async_trait::async_trait;
use crossterm::event::KeyEvent;
use omaghy_model::Result;
use omaghy_store::{RefreshTarget, Store, StoreEvent, Viewer};
use ratatui::{Frame, layout::Rect};
use std::sync::Arc;
use time::OffsetDateTime;

/// What a surface is given. Deliberately small: a surface that needs more is
/// usually a surface doing something the model should.
///
/// Cheap to clone — an `Arc` and a `Copy` timestamp — which is how the app
/// hands it to a surface while still holding `&mut self`.
#[derive(Clone)]
pub struct Ctx {
    pub store: Arc<dyn Store>,
    pub now: OffsetDateTime,
    /// Resolved once, at startup, from what the terminal and font can draw.
    /// Not a preference — `40-config.md` §3 — but a surface still needs it,
    /// and hardcoding `Unicode` in each one made the fallback unreachable.
    pub icons: Icons,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("now", &self.now)
            .finish_non_exhaustive()
    }
}

impl Ctx {
    pub fn viewer(&self) -> &Viewer {
        self.store.viewer()
    }
}

/// What a surface asks the router to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not mine — try the next handler.
    Ignored,
    /// Handled; redraw. Does **not** re-read the store.
    Redraw,
    /// Handled, and the surface's data is stale — re-read before drawing.
    ///
    /// Separate from [`Outcome::Redraw`] because every `j`/`k` used to await
    /// a store read, putting cursor movement on a fallible and eventually
    /// slow path. Found by W2.3.
    Reload,
    Push(Route),
    Pop,
    Replace(Route),
    Quit,
}

#[async_trait]
pub trait Surface: Send {
    fn title(&self) -> String;

    /// Sync, because it runs inside the draw loop. Data is loaded by
    /// [`Surface::load`] and held by the surface.
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx);

    fn on_key(&mut self, key: KeyEvent, ctx: &Ctx) -> Outcome;

    /// Fetch from the store. Called on entry, and again when a relevant
    /// [`StoreEvent::Updated`] arrives.
    async fn load(&mut self, ctx: &Ctx) -> Result<()>;

    /// Whether this event concerns this surface. Default: only global ones.
    fn cares_about(&self, ev: &StoreEvent) -> bool {
        ev.is_global()
    }

    /// Surface-local bindings, shown in help and the footer.
    fn keymap(&self) -> &[Binding] {
        &[]
    }

    /// How fresh this surface's data is, for the header's note.
    ///
    /// Without this `App::render` had no way to ask, so it passed `None` and
    /// §8's "stale shows a note" was unreachable from every surface — the
    /// whole `Fresh<T>` provenance design was decorative. Found by W2.2.
    fn freshness(&self) -> Option<Freshness> {
        None
    }

    /// What `r` should refresh while this surface is on top.
    ///
    /// `App` used to hardcode `RefreshTarget::Notifications`, so `r` on the
    /// dashboard refreshed the inbox. Found by W2.2.
    fn refresh_target(&self) -> Option<RefreshTarget> {
        None
    }

    /// Whether this surface wants keys before the globals see them.
    ///
    /// Globals claim `q`, `r`, `o`, `:`, `?` and `1`–`7`, so a surface taking
    /// free text would lose most of the alphabet mid-word — which is why §5's
    /// `/` filter is unimplementable as typed input. A surface that returns
    /// `true` receives everything except `Esc` and `Ctrl-C`, which always
    /// escape. Found by W2.3.
    fn wants_raw_input(&self) -> bool {
        false
    }

    /// Schedule refreshes here.
    fn on_enter(&mut self, _ctx: &Ctx) {}

    /// Cancel them here.
    fn on_leave(&mut self, _ctx: &Ctx) {}
}

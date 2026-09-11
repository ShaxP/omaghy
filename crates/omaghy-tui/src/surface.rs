//! What a surface is.
//!
//! A surface never renders another surface and never reaches into the router.
//! Navigation is expressed as an [`Outcome`] the router executes.
//!
//! See `spec/30-ui.md` §3.

use crate::{keys::Binding, route::Route};
use async_trait::async_trait;
use crossterm::event::KeyEvent;
use omaghy_model::Result;
use omaghy_store::{Store, StoreEvent, Viewer};
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
    /// Handled; redraw.
    Redraw,
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

    /// Schedule refreshes here.
    fn on_enter(&mut self, _ctx: &Ctx) {}

    /// Cancel them here.
    fn on_leave(&mut self, _ctx: &Ctx) {}
}

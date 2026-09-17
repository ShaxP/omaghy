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
/// Cheap to clone — `Arc`s, a `Copy` timestamp and a handful of enums — which
/// is how the app
/// hands it to a surface while still holding `&mut self`.
#[derive(Clone)]
pub struct Ctx {
    pub store: Arc<dyn Store>,
    pub now: OffsetDateTime,
    /// How the inbox draws itself (`40-config.md` §2 `[notifications]`).
    /// Read once at startup and handed to each surface as it is built — the
    /// settings surface (§6) will rebuild them when it changes one.
    pub inbox: crate::surfaces::notifications::Variants,
    /// The dashboard's sections (§2 `[dashboard]`). The same value the syncer
    /// counts, so the screen cannot show sections nothing fetches.
    pub dashboard: Arc<omaghy_store::query::DashboardConfig>,
    /// Resolved once, at startup, from what the terminal and font can draw.
    /// Not a preference — `40-config.md` §3 — but a surface still needs it,
    /// and hardcoding `Unicode` in each one made the fallback unreachable.
    pub icons: Icons,
    /// Where a changed setting is persisted (`40-config.md` §6.3).
    ///
    /// A seam, not a path: writing a file is I/O and this crate performs none
    /// (`CONTRIBUTING.md`). The binary supplies the format-preserving writer;
    /// the fixture path and most tests supply [`Discard`].
    ///
    /// [`Discard`]: crate::config::Discard
    pub config_writer: Arc<dyn crate::config::ConfigWriter>,
    /// How `o` reaches a browser. A seam for the same reason
    /// [`Self::config_writer`] is: spawning a process is I/O.
    pub opener: Arc<dyn crate::open::Opener>,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("now", &self.now)
            .finish_non_exhaustive()
    }
}

impl Ctx {
    /// A context carrying the default configuration.
    ///
    /// The configured one comes from `App::with_config`; this is what the
    /// app builds before it, and what tests use when the setting under test
    /// is not a configured one.
    pub fn new(store: Arc<dyn Store>, now: OffsetDateTime, icons: Icons) -> Self {
        Self {
            store,
            now,
            inbox: crate::surfaces::notifications::Variants::default(),
            dashboard: Arc::new(omaghy_store::query::DashboardConfig::default()),
            icons,
            config_writer: Arc::new(crate::config::Discard),
            opener: Arc::new(crate::open::NoOpener),
        }
    }

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

    /// Configuration changed; take the new values from `ctx`.
    ///
    /// `40-config.md` §6.2: a setting takes effect on the frame after it
    /// changes, with no restart. Surfaces take their configuration when they
    /// are built, so without this the only way to apply a change would be to
    /// rebuild them — which would throw away the cursor and any filter, and
    /// §6.2's whole point is watching the list you are looking at change.
    ///
    /// Default: nothing to reconfigure.
    fn reconfigure(&mut self, _ctx: &Ctx) {}

    /// What `o` opens here, if anything.
    ///
    /// The thing under the cursor, not the surface — `00-overview.md` §1 says
    /// `o` opens *the current thing*. `None` means there is nothing
    /// addressable, which is a real answer: a check-suite notification has no
    /// URL and never will (`10-domain-model.md` §3.5).
    fn browser_url(&self) -> Option<String> {
        None
    }

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

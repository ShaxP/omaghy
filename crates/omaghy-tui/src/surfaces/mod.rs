//! The seven surfaces.
//!
//! All seven are registered from the start, each wired to a stub. An agent
//! implementing one replaces its own module and touches nothing shared — the
//! registry is written once and never again, so it cannot become the file that
//! conflicts on every merge (`spec/90-plan.md` §2.2).

pub mod dashboard;
pub mod notifications;
pub mod stub;

use crate::{
    route::{Route, SurfaceId},
    surface::Surface,
};

/// Build the surface for an id. The registry.
/// Build the surface for a route.
///
/// Takes the whole [`Route`], not just its id: `App::push` used to discard
/// `Route.arg`, so `search?q=…` arrived with its query thrown away. Found by
/// W2.2. Surfaces that take an argument read it here; the rest ignore it.
pub fn build(route: &Route, ctx: &crate::surface::Ctx) -> Box<dyn Surface> {
    let id = route.surface;
    match id {
        // Configuration reaches a surface here, at construction: it decides
        // how the surface draws, and `render` has no `Ctx` to read it from.
        SurfaceId::Dashboard => Box::new(dashboard::Dashboard::new().with_config(&ctx.dashboard)),
        SurfaceId::Notifications => {
            Box::new(notifications::Notifications::new().with_variants(ctx.inbox))
        }
        // Awaiting their waves; each is replaced in place.
        SurfaceId::PullRequests | SurfaceId::Issues => stub::boxed(id, "M2"),
        SurfaceId::Actions | SurfaceId::Repositories | SurfaceId::Search => stub::boxed(id, "M4"),
    }
}

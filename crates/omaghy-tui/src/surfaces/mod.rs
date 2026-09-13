//! The seven surfaces.
//!
//! All seven are registered from the start, each wired to a stub. An agent
//! implementing one replaces its own module and touches nothing shared — the
//! registry is written once and never again, so it cannot become the file that
//! conflicts on every merge (`spec/90-plan.md` §2.2).

pub mod dashboard;
pub mod notifications;
pub mod stub;

use crate::{route::SurfaceId, surface::Surface};

/// Build the surface for an id. The registry.
pub fn build(id: SurfaceId) -> Box<dyn Surface> {
    match id {
        SurfaceId::Dashboard => Box::new(dashboard::Dashboard::new()),
        SurfaceId::Notifications => Box::new(notifications::Notifications::new()),
        // Awaiting their waves; each is replaced in place.
        SurfaceId::PullRequests | SurfaceId::Issues => stub::boxed(id, "M2"),
        SurfaceId::Actions | SurfaceId::Repositories | SurfaceId::Search => stub::boxed(id, "M4"),
    }
}

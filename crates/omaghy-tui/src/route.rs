//! Routes are addressable: what the palette pushes, what `omaghy <route>`
//! opens from a shell, and what a notification action will eventually invoke.
//!
//! Parsing a route must never require a network call.
//!
//! See `spec/30-ui.md` §3.2.

use std::{fmt, str::FromStr};

/// The seven surfaces. Registered, not hardcoded — the router looks them up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SurfaceId {
    Dashboard,
    Notifications,
    PullRequests,
    Issues,
    Actions,
    Repositories,
    Search,
}

impl SurfaceId {
    pub const ALL: [SurfaceId; 7] = [
        Self::Dashboard,
        Self::Notifications,
        Self::PullRequests,
        Self::Issues,
        Self::Actions,
        Self::Repositories,
        Self::Search,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Dashboard => "dashboard",
            Self::Notifications => "notifications",
            Self::PullRequests => "pr",
            Self::Issues => "issues",
            Self::Actions => "actions",
            Self::Repositories => "repo",
            Self::Search => "search",
        }
    }

    /// The palette's name for "go here", stable like every other action name.
    ///
    /// Beside [`Self::slug`] rather than derived from it because an action
    /// name is a contract a config override refers to, and a slug is a route
    /// in a URL-ish string. They agree today and are free not to.
    pub fn palette_action(self) -> &'static str {
        match self {
            Self::Dashboard => "surface.dashboard",
            Self::Notifications => "surface.notifications",
            Self::PullRequests => "surface.pull-requests",
            Self::Issues => "surface.issues",
            Self::Actions => "surface.actions",
            Self::Repositories => "surface.repositories",
            Self::Search => "surface.search",
        }
    }

    /// The number key that jumps here, as the palette shows it.
    pub fn palette_key(self) -> &'static str {
        match self {
            Self::Dashboard => "1",
            Self::Notifications => "2",
            Self::PullRequests => "3",
            Self::Issues => "4",
            Self::Actions => "5",
            Self::Repositories => "6",
            Self::Search => "7",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Notifications => "Notifications",
            Self::PullRequests => "Pull requests",
            Self::Issues => "Issues",
            Self::Actions => "Actions",
            Self::Repositories => "Repositories",
            Self::Search => "Search",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub surface: SurfaceId,
    /// Everything after the colon, uninterpreted by the router.
    pub arg: Option<String>,
}

impl Route {
    pub fn surface(surface: SurfaceId) -> Self {
        Self { surface, arg: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseRouteError(pub String);

impl fmt::Display for ParseRouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown route: {}", self.0)
    }
}

impl std::error::Error for ParseRouteError {}

impl FromStr for Route {
    type Err = ParseRouteError;

    /// `dashboard` · `notifications` · `pr:ShaxP/shax#61` · `search?q=…`
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (head, arg) = match s.split_once([':', '?']) {
            Some((h, a)) => (h, Some(a.to_owned())),
            None => (s, None),
        };
        let surface = SurfaceId::ALL
            .into_iter()
            .find(|id| id.slug() == head)
            .ok_or_else(|| ParseRouteError(s.to_owned()))?;
        Ok(Self { surface, arg })
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.arg {
            Some(a) => write!(f, "{}:{}", self.surface.slug(), a),
            None => f.write_str(self.surface.slug()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_surfaces() {
        assert_eq!(
            "dashboard".parse::<Route>().unwrap().surface,
            SurfaceId::Dashboard
        );
        assert_eq!(
            "notifications".parse::<Route>().unwrap().surface,
            SurfaceId::Notifications
        );
    }

    #[test]
    fn parses_arguments_without_interpreting_them() {
        let r: Route = "pr:ShaxP/shax#61".parse().unwrap();
        assert_eq!(r.surface, SurfaceId::PullRequests);
        assert_eq!(r.arg.as_deref(), Some("ShaxP/shax#61"));

        let r: Route = "search?q=is:pr review-requested:@me".parse().unwrap();
        assert_eq!(r.surface, SurfaceId::Search);
        assert_eq!(r.arg.as_deref(), Some("q=is:pr review-requested:@me"));
    }

    #[test]
    fn rejects_unknown_surfaces_without_guessing() {
        assert!("nope".parse::<Route>().is_err());
        assert!("".parse::<Route>().is_err());
    }

    #[test]
    fn round_trips_through_display() {
        for s in ["dashboard", "notifications", "pr:ShaxP/shax#61"] {
            let r: Route = s.parse().unwrap();
            assert_eq!(r.to_string(), s);
        }
    }

    #[test]
    fn every_surface_has_a_unique_slug_and_a_title() {
        let slugs: std::collections::HashSet<_> = SurfaceId::ALL.iter().map(|s| s.slug()).collect();
        assert_eq!(slugs.len(), SurfaceId::ALL.len());
        assert!(SurfaceId::ALL.iter().all(|s| !s.title().is_empty()));
    }
}

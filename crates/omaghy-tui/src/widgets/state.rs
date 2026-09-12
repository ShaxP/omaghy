//! The state matrix, rendered centrally so no surface invents its own empty
//! screen — `spec/30-ui.md` §8.
//!
//! Two halves, and the split is the point:
//!
//! [`classify`] turns what a surface already knows — how many rows it has,
//! whether they were ever fetched, whether a filter is on, what the last
//! refresh did — into one of eleven [`SurfaceState`]s. A surface that decides
//! for itself whether it is "empty" will decide differently from the next
//! one, which is exactly how two screens end up disagreeing about whether an
//! inbox is empty or merely filtered.
//!
//! [`StateView`] draws it. Content states hand the area back so the surface
//! can draw its rows; the rest are drawn here, in full, and the surface draws
//! nothing.

use crate::theme::{Icon, Icons, Role};
use omaghy_model::{LimitKind, StoreError};
use omaghy_store::Fresh;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::Modifier,
    text::{Line, Span, Text},
    widgets::Paragraph,
};
use time::{OffsetDateTime, macros::format_description};

/// Every arm of `spec/30-ui.md` §8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceState {
    /// No cache and nothing fetched yet: skeleton rows, never a blank frame.
    Cold,
    Populated,
    /// Shown normally; the header carries the freshness note. Stale data is
    /// never hidden.
    Stale,
    /// Existing content plus a header spinner. Content is never replaced by
    /// a spinner.
    Refreshing,
    /// Fetched, and there is genuinely nothing. A designed state.
    Empty,
    /// Distinct from [`SurfaceState::Empty`]: names the filter and how to
    /// clear it.
    FilteredEmpty {
        filter: String,
    },
    /// Content plus a banner naming the cause.
    OfflineWithCache,
    /// An empty state naming the cause, rather than a generic error.
    OfflineWithoutCache,
    RateLimited {
        kind: LimitKind,
        at: OffsetDateTime,
    },
    /// Access was lost. Not retried.
    Forbidden,
    /// One line, plus a key to retry. Never a stack trace.
    Error {
        message: String,
    },
}

impl SurfaceState {
    /// Whether the surface still draws its own rows underneath.
    pub fn has_content(&self) -> bool {
        matches!(
            self,
            Self::Populated | Self::Stale | Self::Refreshing | Self::OfflineWithCache
        )
    }

    /// A stable name, for tests and for the status line.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Populated => "populated",
            Self::Stale => "stale",
            Self::Refreshing => "refreshing",
            Self::Empty => "empty",
            Self::FilteredEmpty { .. } => "filtered-empty",
            Self::OfflineWithCache => "offline-with-cache",
            Self::OfflineWithoutCache => "offline-without-cache",
            Self::RateLimited { .. } => "rate-limited",
            Self::Forbidden => "forbidden",
            Self::Error { .. } => "error",
        }
    }
}

/// What a surface knows about its own data.
///
/// Deliberately primitive: anything richer would let a surface smuggle a
/// decision in here that [`classify`] is supposed to be making.
#[derive(Debug, Clone, Copy, Default)]
pub struct Conditions<'a> {
    /// Whether a fetch has ever completed. Cold cache is `false`.
    pub fetched: bool,
    pub stale: bool,
    pub refreshing: bool,
    pub items: usize,
    /// A human description of the active filter, e.g. `unread only`.
    pub filter: Option<&'a str>,
    /// What the last refresh failed with, if it failed.
    pub error: Option<&'a StoreError>,
}

impl<'a> Conditions<'a> {
    /// Read provenance straight off a [`Fresh`], so no surface has to
    /// remember what `fetched_at: None` means.
    pub fn from_fresh<T>(fresh: &Fresh<T>, items: usize) -> Self {
        Self {
            fetched: fresh.fetched_at.is_some(),
            stale: fresh.stale,
            refreshing: fresh.refreshing,
            items,
            filter: None,
            error: None,
        }
    }

    pub fn filter(mut self, filter: Option<&'a str>) -> Self {
        self.filter = filter;
        self
    }

    pub fn error(mut self, error: Option<&'a StoreError>) -> Self {
        self.error = error;
        self
    }
}

/// Decide which of the eleven states applies. The only place that decides.
pub fn classify(c: &Conditions<'_>) -> SurfaceState {
    if let Some(err) = c.error {
        return match err {
            StoreError::Forbidden => SurfaceState::Forbidden,
            StoreError::RateLimited { kind, at } => SurfaceState::RateLimited {
                kind: *kind,
                at: *at,
            },
            // An unreachable server does not invalidate what we already have,
            // so the two offline states differ only in whether we have any.
            StoreError::Offline(_) if c.items > 0 && err.keeps_cached_content() => {
                SurfaceState::OfflineWithCache
            }
            StoreError::Offline(_) => SurfaceState::OfflineWithoutCache,
            other => SurfaceState::Error {
                message: other.terse(),
            },
        };
    }
    if c.items > 0 {
        return if c.refreshing {
            SurfaceState::Refreshing
        } else if c.stale {
            SurfaceState::Stale
        } else {
            SurfaceState::Populated
        };
    }
    if !c.fetched {
        return SurfaceState::Cold;
    }
    match c.filter {
        Some(f) => SurfaceState::FilteredEmpty {
            filter: f.to_owned(),
        },
        None => SurfaceState::Empty,
    }
}

/// What "empty" means on this particular surface.
///
/// The widget owns the *shape* of an empty screen so they all look alike;
/// the surface owns the *words*, because "All caught up" is right for an
/// inbox and wrong for a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyCopy<'a> {
    pub headline: &'a str,
    pub detail: &'a str,
    /// The most useful next action: a key and what it does.
    pub action: Option<(&'a str, &'a str)>,
}

impl Default for EmptyCopy<'_> {
    fn default() -> Self {
        Self {
            headline: "All caught up",
            detail: "Nothing needs your attention.",
            action: Some(("r", "check again")),
        }
    }
}

/// What the surface should do once the state widget has drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// Draw your rows into this area. It may be smaller than the area you
    /// passed in — an offline banner takes a row.
    Rows(Rect),
    /// Everything is drawn; draw nothing.
    Done,
}

impl Body {
    /// The area left for rows, if any.
    pub fn rows(self) -> Option<Rect> {
        match self {
            Self::Rows(r) => Some(r),
            Self::Done => None,
        }
    }
}

/// Draws any arm of the state matrix.
#[derive(Debug, Clone)]
pub struct StateView<'a> {
    state: &'a SurfaceState,
    icons: Icons,
    empty: EmptyCopy<'a>,
    spinner: usize,
    retry: (&'a str, &'a str),
    clear_filter: (&'a str, &'a str),
}

impl<'a> StateView<'a> {
    pub fn new(state: &'a SurfaceState) -> Self {
        Self {
            state,
            icons: Icons::UNICODE,
            empty: EmptyCopy::default(),
            spinner: 0,
            retry: ("r", "retry"),
            clear_filter: ("Esc", "clear the filter"),
        }
    }

    pub fn icons(mut self, icons: Icons) -> Self {
        self.icons = icons;
        self
    }

    pub fn empty(mut self, copy: EmptyCopy<'a>) -> Self {
        self.empty = copy;
        self
    }

    /// The tick counter, so the spinner advances. Tests pass 0.
    pub fn spinner(mut self, frame: usize) -> Self {
        self.spinner = frame;
        self
    }

    pub fn retry(mut self, key: &'a str, what: &'a str) -> Self {
        self.retry = (key, what);
        self
    }

    pub fn clear_filter(mut self, key: &'a str, what: &'a str) -> Self {
        self.clear_filter = (key, what);
        self
    }

    pub fn render(&self, f: &mut Frame, area: Rect) -> Body {
        match self.state {
            SurfaceState::Populated | SurfaceState::Stale | SurfaceState::Refreshing => {
                Body::Rows(area)
            }
            SurfaceState::OfflineWithCache => {
                let banner = Rect {
                    height: 1.min(area.height),
                    ..area
                };
                self.banner(
                    f,
                    banner,
                    Icon::Offline,
                    Role::Warning,
                    &format!("Offline {} showing cached data.", self.icons.dot()),
                );
                Body::Rows(Rect {
                    y: area.y.saturating_add(1),
                    height: area.height.saturating_sub(1),
                    ..area
                })
            }
            SurfaceState::Cold => {
                self.skeleton(f, area);
                Body::Done
            }
            SurfaceState::Empty => {
                self.message(f, area, Icon::CaughtUp, Role::Success, &self.empty);
                Body::Done
            }
            SurfaceState::FilteredEmpty { filter } => {
                self.message(
                    f,
                    area,
                    Icon::Filter,
                    Role::Accent,
                    &EmptyCopy {
                        headline: "No matches",
                        detail: &format!("Nothing here matches {filter}."),
                        action: Some(self.clear_filter),
                    },
                );
                Body::Done
            }
            SurfaceState::OfflineWithoutCache => {
                self.message(
                    f,
                    area,
                    Icon::Offline,
                    Role::Warning,
                    &EmptyCopy {
                        headline: "Offline, and nothing cached",
                        detail: "GitHub is unreachable and this surface has never been fetched.",
                        action: Some(self.retry),
                    },
                );
                Body::Done
            }
            SurfaceState::RateLimited { kind, at } => {
                let when = at
                    .format(format_description!("[hour]:[minute] UTC"))
                    .unwrap_or_else(|_| "soon".to_owned());
                self.message(
                    f,
                    area,
                    Icon::RateLimited,
                    Role::Warning,
                    &EmptyCopy {
                        headline: &format!("GitHub {kind} reached"),
                        detail: &format!("Resets at {when}. Cached data is still readable."),
                        action: Some(self.retry),
                    },
                );
                Body::Done
            }
            SurfaceState::Forbidden => {
                self.message(
                    f,
                    area,
                    Icon::Forbidden,
                    Role::Danger,
                    &EmptyCopy {
                        headline: "No access",
                        detail: "You do not have access to this. It will not be retried.",
                        // Deliberately no retry: access is gone, and a key
                        // that cannot work is worse than no key.
                        action: None,
                    },
                );
                Body::Done
            }
            SurfaceState::Error { message } => {
                self.message(
                    f,
                    area,
                    Icon::Error,
                    Role::Danger,
                    &EmptyCopy {
                        headline: "Something failed",
                        detail: message,
                        action: Some(self.retry),
                    },
                );
                Body::Done
            }
        }
    }

    /// One line naming a cause, above content that is still worth reading.
    fn banner(&self, f: &mut Frame, area: Rect, icon: Icon, role: Role, text: &str) {
        if area.height == 0 {
            return;
        }
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(self.icons.get(icon).to_owned(), role.style()),
                Span::raw(" "),
                Span::styled(text.to_owned(), role.style()),
            ])),
            area,
        );
    }

    /// Skeleton rows plus a loading indicator — never a blank frame.
    fn skeleton(&self, f: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        // Varying lengths so it reads as a list of rows rather than a block;
        // fixed rather than random, so a snapshot does not change per run.
        const SHAPE: [u16; 5] = [70, 48, 82, 58, 64];
        let bar = self.icons.skeleton();
        let rows = area.height.saturating_sub(1).max(1);
        let mut lines = Vec::with_capacity(rows as usize);
        for i in 0..rows {
            let pct = SHAPE[i as usize % SHAPE.len()];
            let len = (u32::from(area.width.saturating_sub(4)) * u32::from(pct) / 100) as usize;
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(bar.repeat(len.max(1)), Role::Muted.style()),
            ]));
        }
        if area.height > 1 {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(self.icons.spinner(self.spinner), Role::Accent.style()),
                Span::raw(" "),
                Span::styled("Loading", Role::Muted.style()),
            ]));
        }
        f.render_widget(Paragraph::new(Text::from(lines)), area);
    }

    /// The centred block every non-content state uses, so they look alike.
    fn message(&self, f: &mut Frame, area: Rect, icon: Icon, role: Role, copy: &EmptyCopy<'_>) {
        notice(f, area, self.icons, icon, role, copy);
    }
}

/// The centred glyph-headline-detail-action block, in one place so that every
/// screen without rows looks like every other one.
pub fn notice(
    f: &mut Frame,
    area: Rect,
    icons: Icons,
    icon: Icon,
    role: Role,
    copy: &EmptyCopy<'_>,
) {
    let EmptyCopy {
        headline,
        detail,
        action,
    } = *copy;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(icons.get(icon).to_owned(), role.style()),
            Span::raw("  "),
            Span::styled(
                headline.to_owned(),
                Role::Default.style().add_modifier(Modifier::BOLD),
            ),
        ])
        .alignment(Alignment::Center),
        Line::raw(""),
        Line::styled(detail.to_owned(), Role::Muted.style()).alignment(Alignment::Center),
    ];
    if let Some((key, what)) = action {
        lines.push(Line::raw(""));
        lines.push(
            Line::from(vec![
                Span::styled(
                    key.to_owned(),
                    Role::Accent.style().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(what.to_owned(), Role::Muted.style()),
            ])
            .alignment(Alignment::Center),
        );
    }
    let h = lines.len() as u16;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let centred = Rect {
        x: area.x,
        y,
        width: area.width,
        height: h.min(area.height),
    };
    f.render_widget(Paragraph::new(Text::from(lines)), centred);
}

/// An empty state says *why*, and what to do next.
///
/// **Compatibility shim.** The P0.4 shell and its stub surfaces call this;
/// new code builds a [`SurfaceState`] and renders a [`StateView`], so that
/// the reason for the empty screen is classified centrally rather than
/// chosen at the call site. Removed when the surfaces land in Wave 2.
pub fn empty_state(f: &mut Frame, area: Rect, headline: &str, detail: &str) {
    // Informational rather than [`SurfaceState::Empty`]: the shell's stub
    // surfaces use this to say "not implemented yet", and a green tick is
    // the wrong thing to put beside that.
    notice(
        f,
        area,
        Icons::UNICODE,
        Icon::Info,
        Role::Muted,
        &EmptyCopy {
            headline,
            detail,
            action: None,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use omaghy_model::CacheError;
    use omaghy_store::fake::FIXTURE_NOW;
    use time::Duration;

    fn cond<'a>() -> Conditions<'a> {
        Conditions {
            fetched: true,
            ..Default::default()
        }
    }

    #[test]
    fn an_empty_inbox_is_not_a_cold_cache() {
        // The single distinction the whole matrix turns on.
        assert_eq!(classify(&cond()), SurfaceState::Empty);
        assert_eq!(
            classify(&Conditions {
                fetched: false,
                ..Default::default()
            }),
            SurfaceState::Cold
        );
    }

    #[test]
    fn filtered_to_empty_is_a_different_screen_from_empty() {
        let s = classify(&cond().filter(Some("unread only")));
        assert_eq!(
            s,
            SurfaceState::FilteredEmpty {
                filter: "unread only".into()
            }
        );
        assert_ne!(s, SurfaceState::Empty);
        // A filter over rows that exist is simply populated.
        assert_eq!(
            classify(&Conditions { items: 3, ..cond() }.filter(Some("unread only"))),
            SurfaceState::Populated
        );
    }

    #[test]
    fn stale_and_refreshing_keep_their_content() {
        for s in [
            classify(&Conditions {
                items: 3,
                stale: true,
                ..cond()
            }),
            classify(&Conditions {
                items: 3,
                refreshing: true,
                ..cond()
            }),
        ] {
            assert!(s.has_content(), "{s:?} must not replace content");
        }
        // Refreshing wins over stale: the spinner is the more useful note
        // while a fetch is actually in flight.
        assert_eq!(
            classify(&Conditions {
                items: 3,
                stale: true,
                refreshing: true,
                ..cond()
            }),
            SurfaceState::Refreshing
        );
    }

    #[test]
    fn offline_splits_on_whether_there_is_anything_to_show() {
        let err = StoreError::Offline("dns".into());
        assert_eq!(
            classify(&Conditions { items: 4, ..cond() }.error(Some(&err))),
            SurfaceState::OfflineWithCache
        );
        assert_eq!(
            classify(&cond().error(Some(&err))),
            SurfaceState::OfflineWithoutCache
        );
        assert!(SurfaceState::OfflineWithCache.has_content());
        assert!(!SurfaceState::OfflineWithoutCache.has_content());
    }

    #[test]
    fn a_corrupt_cache_does_not_keep_showing_its_rows() {
        // Everything else survives offline; a corrupt cache is the one error
        // whose content cannot be trusted (spec/20-store.md §7).
        let err = StoreError::Cache(CacheError::Corrupt("bad header".into()));
        let s = classify(&Conditions { items: 4, ..cond() }.error(Some(&err)));
        assert!(matches!(s, SurfaceState::Error { .. }));
        assert!(!s.has_content());
    }

    #[test]
    fn forbidden_and_rate_limited_are_not_generic_errors() {
        assert_eq!(
            classify(&cond().error(Some(&StoreError::Forbidden))),
            SurfaceState::Forbidden
        );
        let limited = StoreError::RateLimited {
            kind: LimitKind::Primary,
            at: FIXTURE_NOW + Duration::hours(1),
        };
        assert!(matches!(
            classify(&cond().error(Some(&limited))),
            SurfaceState::RateLimited { .. }
        ));
    }

    #[test]
    fn provenance_is_read_off_fresh_rather_than_guessed() {
        let cold: Fresh<Vec<u8>> = Fresh::never(vec![]);
        assert_eq!(
            classify(&Conditions::from_fresh(&cold, 0)),
            SurfaceState::Cold
        );

        let fetched = Fresh::from_network(vec![1u8], FIXTURE_NOW);
        assert_eq!(
            classify(&Conditions::from_fresh(&fetched, 1)),
            SurfaceState::Populated
        );

        let empty_but_fetched = Fresh::from_network(Vec::<u8>::new(), FIXTURE_NOW);
        assert_eq!(
            classify(&Conditions::from_fresh(&empty_but_fetched, 0)),
            SurfaceState::Empty
        );
    }

    #[test]
    fn every_state_has_a_distinct_name() {
        let all = [
            SurfaceState::Cold,
            SurfaceState::Populated,
            SurfaceState::Stale,
            SurfaceState::Refreshing,
            SurfaceState::Empty,
            SurfaceState::FilteredEmpty { filter: "x".into() },
            SurfaceState::OfflineWithCache,
            SurfaceState::OfflineWithoutCache,
            SurfaceState::RateLimited {
                kind: LimitKind::Primary,
                at: FIXTURE_NOW,
            },
            SurfaceState::Forbidden,
            SurfaceState::Error {
                message: "x".into(),
            },
        ];
        let names: std::collections::BTreeSet<_> = all.iter().map(|s| s.name()).collect();
        assert_eq!(names.len(), 11, "the matrix has eleven arms");
    }
}

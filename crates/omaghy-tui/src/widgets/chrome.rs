//! Header and footer. Three zones, always — `spec/30-ui.md` §4.
//!
//! The header is the one place the UI admits it is showing cached data, and
//! it takes that admission from [`Fresh`] rather than from a surface's
//! opinion. The footer shows *this* surface's bindings, never a fixed list —
//! a footer that says the same thing everywhere is decoration.

use crate::{
    keys::Binding,
    theme::{Icon, Icons, Role},
    widgets::toast::Toast,
};
use omaghy_store::Fresh;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};

/// What the header admits about the data underneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Freshness {
    /// The store's own rule for whether this needs saying out loud.
    pub note: bool,
    pub refreshing: bool,
    /// Never fetched at all — distinct from fetched-and-stale.
    pub never_fetched: bool,
}

impl Freshness {
    /// Data that needs no note: fetched from the network, just now.
    pub const CURRENT: Self = Self {
        note: false,
        refreshing: false,
        never_fetched: false,
    };

    pub fn of<T>(fresh: &Fresh<T>) -> Self {
        Self {
            note: fresh.needs_provenance_note(),
            refreshing: fresh.refreshing,
            never_fetched: fresh.fetched_at.is_none(),
        }
    }

    fn spans(&self, icons: Icons, spinner: usize) -> Vec<Span<'static>> {
        let mut out = Vec::new();
        if self.refreshing {
            out.push(Span::styled(icons.spinner(spinner), Role::Accent.style()));
            out.push(Span::raw(" "));
            out.push(Span::styled("refreshing", Role::Muted.style()));
            out.push(Span::raw(" "));
        }
        if self.never_fetched {
            out.push(Span::styled(
                icons.get(Icon::Offline).to_owned(),
                Role::Warning.style(),
            ));
            out.push(Span::raw(" "));
            out.push(Span::styled("not fetched", Role::Warning.style()));
            out.push(Span::raw(" "));
        } else if self.note {
            out.push(Span::styled(
                icons.get(Icon::Stale).to_owned(),
                Role::Warning.style(),
            ));
            out.push(Span::raw(" "));
            out.push(Span::styled("cached", Role::Warning.style()));
            out.push(Span::raw(" "));
        }
        out
    }
}

/// `title · scope · counts` on the left; `freshness · viewer` on the right.
#[derive(Debug, Clone)]
pub struct Header<'a> {
    title: &'a str,
    scope: Option<&'a str>,
    counts: Option<&'a str>,
    viewer: &'a str,
    freshness: Freshness,
    icons: Icons,
    spinner: usize,
}

impl<'a> Header<'a> {
    pub fn new(title: &'a str, viewer: &'a str) -> Self {
        Self {
            title,
            scope: None,
            counts: None,
            viewer,
            freshness: Freshness::CURRENT,
            icons: Icons::UNICODE,
            spinner: 0,
        }
    }

    /// What the surface is scoped to — a repository, a query, a filter.
    ///
    /// This is where a repository elided out of the rows gets named
    /// (`spec/30-ui.md` §9).
    pub fn scope(mut self, scope: &'a str) -> Self {
        self.scope = Some(scope);
        self
    }

    /// `10 unread · 29 total`, built by the surface because only it knows
    /// what is worth counting.
    pub fn counts(mut self, counts: &'a str) -> Self {
        self.counts = Some(counts);
        self
    }

    pub fn freshness(mut self, freshness: Freshness) -> Self {
        self.freshness = freshness;
        self
    }

    pub fn icons(mut self, icons: Icons) -> Self {
        self.icons = icons;
        self
    }

    pub fn spinner(mut self, frame: usize) -> Self {
        self.spinner = frame;
        self
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }
        let area = Rect { height: 1, ..area };
        let dot = self.icons.dot();

        let mut left = vec![
            Span::raw(" "),
            Span::styled(
                self.title.to_owned(),
                Role::Default.style().add_modifier(Modifier::BOLD),
            ),
        ];
        for extra in [self.scope, self.counts].into_iter().flatten() {
            left.push(Span::styled(format!(" {dot} "), Role::Muted.style()));
            left.push(Span::styled(extra.to_owned(), Role::Muted.style()));
        }

        let mut right = self.freshness.spans(self.icons, self.spinner);
        right.push(Span::styled(
            self.icons.get(Icon::Actor).to_owned(),
            Role::Muted.style(),
        ));
        right.push(Span::raw(" "));
        right.push(Span::styled(self.viewer.to_owned(), Role::Muted.style()));
        right.push(Span::raw(" "));

        // Split rather than overlay: two right-aligned paragraphs on one area
        // collide silently, and the left half is the one that loses.
        let rw: u16 = right
            .iter()
            .map(|s| s.content.chars().count() as u16)
            .sum::<u16>()
            + 1;
        let split = area.width.saturating_sub(rw);
        f.render_widget(
            Paragraph::new(Line::from(left)),
            Rect {
                width: split,
                ..area
            },
        );
        f.render_widget(
            Paragraph::new(Line::from(right).alignment(Alignment::Right)),
            Rect {
                x: area.x + split,
                width: area.width - split,
                ..area
            },
        );
    }
}

/// Contextual keys on the left, a transient message on the right.
#[derive(Debug, Clone)]
pub struct Footer<'a> {
    hints: &'a [Binding],
    toast: Option<&'a Toast>,
    icons: Icons,
    spinner: usize,
}

impl<'a> Footer<'a> {
    /// The bindings come from `Surface::keymap()`, so the footer cannot say
    /// anything the surface does not actually do.
    pub fn new(hints: &'a [Binding]) -> Self {
        Self {
            hints,
            toast: None,
            icons: Icons::UNICODE,
            spinner: 0,
        }
    }

    pub fn toast(mut self, toast: Option<&'a Toast>) -> Self {
        self.toast = toast;
        self
    }

    pub fn icons(mut self, icons: Icons) -> Self {
        self.icons = icons;
        self
    }

    pub fn spinner(mut self, frame: usize) -> Self {
        self.spinner = frame;
        self
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }
        let area = Rect { height: 1, ..area };
        let reserved = self.toast.map_or(0, |t| t.width() + 2);
        let pairs: Vec<(&str, &str)> = self.hints.iter().map(|b| (b.keys, b.description)).collect();
        render_hints(f, area, &pairs, area.width.saturating_sub(reserved));

        if let Some(toast) = self.toast {
            let mut spans = toast.spans(self.icons, self.spinner);
            spans.push(Span::raw(" "));
            f.render_widget(
                Paragraph::new(Line::from(spans).alignment(Alignment::Right)),
                area,
            );
        }
    }
}

/// Key hints, dropping whole pairs that do not fit rather than truncating
/// one into nonsense.
fn render_hints(f: &mut Frame, area: Rect, hints: &[(&str, &str)], budget: u16) {
    let mut spans = vec![Span::raw(" ")];
    let mut used: u16 = 1;
    for (i, (key, what)) in hints.iter().enumerate() {
        let sep = if i > 0 { 2 } else { 0 };
        let cost = sep + key.chars().count() as u16 + 1 + what.chars().count() as u16;
        if used + cost > budget {
            break;
        }
        used += cost;
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            Role::Accent.style().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled((*what).to_owned(), Role::Muted.style()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ------------------------------------------------------ compatibility shims
//
// The P0.4 shell calls these free functions. New code builds a [`Header`] or
// a [`Footer`], which carry the scope, counts, icon mode and toast the shell
// versions cannot express. They go when the surfaces land in Wave 2.

/// Title on the left, viewer and freshness on the right.
pub fn header<T>(f: &mut Frame, area: Rect, title: &str, viewer: &str, fresh: Option<&Fresh<T>>) {
    Header::new(title, viewer)
        .freshness(fresh.map_or(Freshness::CURRENT, Freshness::of))
        .render(f, area);
}

/// Contextual keys, never a fixed list.
pub fn footer(f: &mut Frame, area: Rect, hints: &[(&str, &str)]) {
    if area.height == 0 {
        return;
    }
    let area = Rect { height: 1, ..area };
    render_hints(f, area, hints, area.width);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::test_support::render;
    use omaghy_store::fake::FIXTURE_NOW;
    use time::Duration;

    #[test]
    fn the_header_admits_cached_data_and_says_nothing_when_it_is_current() {
        let current = Fresh::from_network(1, FIXTURE_NOW);
        let out = render(60, 1, |f, a| {
            Header::new("Notifications", "ShaxP")
                .freshness(Freshness::of(&current))
                .render(f, a)
        });
        assert!(!out.contains("cached"), "fresh data needs no note: {out:?}");
        assert!(out.contains("ShaxP"));

        let stale = Fresh::from_cache(
            1,
            FIXTURE_NOW - Duration::hours(2),
            Duration::minutes(5),
            FIXTURE_NOW,
        );
        let out = render(60, 1, |f, a| {
            Header::new("Notifications", "ShaxP")
                .freshness(Freshness::of(&stale))
                .render(f, a)
        });
        assert!(
            out.contains("cached"),
            "stale data is never hidden: {out:?}"
        );
    }

    #[test]
    fn a_cold_cache_says_not_fetched_rather_than_cached() {
        let cold: Fresh<Vec<u8>> = Fresh::never(vec![]);
        let out = render(60, 1, |f, a| {
            Header::new("Notifications", "ShaxP")
                .freshness(Freshness::of(&cold))
                .render(f, a)
        });
        assert!(out.contains("not fetched"), "{out:?}");
        assert!(!out.contains("cached"), "{out:?}");
    }

    #[test]
    fn the_header_carries_scope_and_counts_when_given_them() {
        let out = render(80, 1, |f, a| {
            Header::new("Notifications", "ShaxP")
                .scope("ShaxP/shax")
                .counts("10 unread · 29 total")
                .render(f, a)
        });
        assert!(out.contains("ShaxP/shax"), "{out:?}");
        assert!(out.contains("10 unread"), "{out:?}");
    }

    #[test]
    fn a_long_title_is_truncated_rather_than_eating_the_viewer() {
        // Two right-aligned paragraphs on one area collide silently, and it
        // is always the half naming who you are acting as that disappears.
        let out = render(60, 1, |f, a| {
            Header::new(
                "Pull requests in a repository with an extremely long name",
                "ShaxP",
            )
            .scope("some-very-long-organization-name/an-equally-long-repo")
            .counts("412 open · 90 draft")
            .render(f, a)
        });
        assert!(
            out.ends_with("ShaxP"),
            "the viewer is never the half that loses: {out:?}"
        );
        assert!(out.chars().count() <= 60, "{out:?}");
        assert!(out.starts_with(" Pull requests"), "{out:?}");
    }

    #[test]
    fn the_footer_drops_whole_hints_rather_than_truncating_one() {
        let hints = [
            Binding::new("a.one", "j", "next"),
            Binding::new("a.two", "k", "previous"),
            Binding::new("a.three", "Enter", "open the thing in a browser"),
        ];
        let wide = render(80, 1, |f, a| Footer::new(&hints).render(f, a));
        assert!(wide.contains("open the thing in a browser"), "{wide:?}");

        let narrow = render(24, 1, |f, a| Footer::new(&hints).render(f, a));
        assert!(narrow.contains("next"), "{narrow:?}");
        assert!(
            !narrow.contains("open the thing"),
            "a half-rendered hint is worse than none: {narrow:?}"
        );
        // And it never overflows the line it was given.
        assert!(narrow.chars().count() <= 24, "{narrow:?}");
    }

    #[test]
    fn a_toast_takes_the_right_of_the_footer_without_eating_the_hints() {
        let hints = [Binding::new("a.one", "j", "next")];
        let toast = Toast::success("Marked 3 read");
        let out = render(60, 1, |f, a| {
            Footer::new(&hints).toast(Some(&toast)).render(f, a)
        });
        assert!(out.contains("next"), "{out:?}");
        assert!(out.contains("Marked 3 read"), "{out:?}");
    }
}

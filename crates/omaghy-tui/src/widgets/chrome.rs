//! Header and footer. Three zones, always — `spec/30-ui.md` §4.

use crate::theme::Role;
use omaghy_store::Fresh;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::Paragraph,
};

/// Title on the left, viewer and freshness on the right.
///
/// The header is the one place the UI admits it is showing cached data.
pub fn header<T>(f: &mut Frame, area: Rect, title: &str, viewer: &str, fresh: Option<&Fresh<T>>) {
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            format!(" {title}"),
            Role::Accent.style(),
        )])),
        area,
    );

    let mut right = vec![];
    if let Some(fr) = fresh {
        if fr.refreshing {
            right.push(Span::styled("refreshing ", Role::Muted.style()));
        }
        if fr.stale {
            right.push(Span::styled("cached ", Role::Warning.style()));
        }
    }
    right.push(Span::styled(format!("{viewer} "), Role::Muted.style()));
    f.render_widget(
        Paragraph::new(Line::from(right)).alignment(Alignment::Right),
        area,
    );
}

/// Contextual keys, never a fixed list.
pub fn footer(f: &mut Frame, area: Rect, hints: &[(&str, &str)]) {
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, what)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Role::Muted.style()));
        }
        spans.push(Span::styled(*key, Role::Accent.style()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(*what, Role::Muted.style()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

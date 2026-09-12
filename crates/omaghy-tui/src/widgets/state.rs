//! The state matrix, rendered centrally so no surface invents its own empty
//! screen — `spec/30-ui.md` §8.

use crate::theme::Role;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    text::{Line, Span, Text},
    widgets::Paragraph,
};

/// An empty state says *why*, and what to do next. "All caught up" is a real
/// state that deserves design, not a shrug.
pub fn empty_state(f: &mut Frame, area: Rect, headline: &str, detail: &str) {
    let lines = vec![
        Line::from(Span::styled(headline, Role::Default.style())).alignment(Alignment::Center),
        Line::raw(""),
        Line::from(Span::styled(detail, Role::Muted.style())).alignment(Alignment::Center),
    ];
    let y = area.y + area.height.saturating_sub(3) / 2;
    let centred = Rect {
        x: area.x,
        y,
        width: area.width,
        height: 3.min(area.height),
    };
    f.render_widget(Paragraph::new(Text::from(lines)), centred);
}

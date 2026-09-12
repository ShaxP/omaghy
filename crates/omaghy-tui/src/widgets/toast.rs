//! Transient messages: "Marked 3 read", "Refreshing…", "GitHub is
//! unreachable".
//!
//! A toast lives in the footer rather than in a modal, because a modal
//! interrupts and a transient message has not earned that. Its level is
//! carried by a glyph as well as a colour — on a monochrome Omarchy theme
//! "marked read" and "that failed" are otherwise the same sentence in the
//! same colour.

use crate::theme::{Icon, Icons, Role};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::Paragraph,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    #[default]
    Info,
    Progress,
    Success,
    Warning,
    Error,
}

impl Level {
    pub fn icon(self) -> Icon {
        match self {
            Self::Info => Icon::Info,
            Self::Progress => Icon::Refreshing,
            Self::Success => Icon::CheckPass,
            Self::Warning => Icon::Warning,
            Self::Error => Icon::Error,
        }
    }

    pub fn role(self) -> Role {
        match self {
            Self::Info => Role::Muted,
            Self::Progress => Role::Accent,
            Self::Success => Role::Success,
            Self::Warning => Role::Warning,
            Self::Error => Role::Danger,
        }
    }
}

/// One transient message.
///
/// The widget holds no clock: when a toast expires is the app's business,
/// and a widget that measured time would be doing I/O by another name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub level: Level,
    pub message: String,
}

impl Toast {
    pub fn new(level: Level, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(Level::Info, message)
    }

    /// Something is in flight. Rendered with the spinner rather than a
    /// static glyph, so a stuck operation is visibly stuck.
    pub fn progress(message: impl Into<String>) -> Self {
        Self::new(Level::Progress, message)
    }

    pub fn success(message: impl Into<String>) -> Self {
        Self::new(Level::Success, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(Level::Warning, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(Level::Error, message)
    }

    /// From a store error, in one line. Never a stack trace — §8.
    pub fn from_error(err: &omaghy_model::StoreError) -> Self {
        Self::new(Level::Error, err.terse())
    }

    /// Glyph, space, message. The glyph is what survives a monochrome theme.
    pub fn spans(&self, icons: Icons, spinner: usize) -> Vec<Span<'static>> {
        let glyph = if self.level == Level::Progress {
            icons.spinner(spinner)
        } else {
            icons.get(self.level.icon())
        };
        vec![
            Span::styled(glyph, self.level.role().style()),
            Span::raw(" "),
            Span::styled(self.message.clone(), self.level.role().style()),
        ]
    }

    /// Cells this toast needs, so the footer can decide what to drop.
    pub fn width(&self) -> u16 {
        (self.message.chars().count() + 2) as u16
    }
}

/// Draw a toast on its own line, right-aligned.
///
/// Used when the toast is not sharing the footer with key hints — a status
/// line above a detail view, say.
pub fn render(f: &mut Frame, area: Rect, toast: &Toast, icons: Icons, spinner: usize) {
    if area.height == 0 {
        return;
    }
    let mut spans = toast.spans(icons, spinner);
    spans.push(Span::raw(" "));
    f.render_widget(
        Paragraph::new(Line::from(spans).alignment(Alignment::Right)),
        Rect { height: 1, ..area },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use omaghy_model::StoreError;

    #[test]
    fn every_level_carries_a_glyph_as_well_as_a_colour() {
        // Osaka Jade resolves accent and foreground to the same value, so a
        // level distinguished only by colour is not distinguished at all.
        let mut glyphs = std::collections::BTreeSet::new();
        for level in [
            Level::Info,
            Level::Progress,
            Level::Success,
            Level::Warning,
            Level::Error,
        ] {
            let t = Toast::new(level, "something happened");
            let spans = t.spans(Icons::UNICODE, 0);
            assert_eq!(spans[0].content.chars().count(), 1);
            assert!(glyphs.insert(spans[0].content.to_string()), "{level:?}");
            // And the same glyph budget without a Nerd Font.
            assert_eq!(
                t.spans(Icons::ASCII, 0)[0].content.chars().count(),
                1,
                "{level:?}"
            );
        }
    }

    #[test]
    fn progress_spins_while_the_others_stay_put() {
        let p = Toast::progress("Refreshing…");
        assert_ne!(
            p.spans(Icons::UNICODE, 0)[0].content,
            p.spans(Icons::UNICODE, 1)[0].content
        );
        let e = Toast::error("boom");
        assert_eq!(
            e.spans(Icons::UNICODE, 0)[0].content,
            e.spans(Icons::UNICODE, 1)[0].content
        );
    }

    #[test]
    fn a_store_error_becomes_one_line() {
        let t = Toast::from_error(&StoreError::Forbidden);
        assert_eq!(t.level, Level::Error);
        assert_eq!(t.message, "access denied");
        assert!(!t.message.contains('\n'));
    }
}

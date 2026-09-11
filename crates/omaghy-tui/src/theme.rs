//! Semantic colour roles.
//!
//! The 16 ANSI colours must be sufficient: Omarchy themes the terminal, so
//! omaghy inherits the active theme for free. Roles are named by meaning, so
//! no call site names a colour.
//!
//! **Colour is never the only encoding** — several Omarchy themes resolve
//! `accent` and `foreground` to the same value (Osaka Jade: both `#cacccc`).
//! A failing check is a glyph that is *also* red, never red text alone.
//!
//! See `spec/30-ui.md` §7.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Default,
    Muted,
    Accent,
    Success,
    Warning,
    Danger,
    Selected,
    Unread,
}

impl Role {
    pub fn style(self) -> Style {
        match self {
            Self::Default => Style::default(),
            Self::Muted => Style::default().fg(Color::DarkGray),
            Self::Accent => Style::default().fg(Color::Cyan),
            Self::Success => Style::default().fg(Color::Green),
            Self::Warning => Style::default().fg(Color::Yellow),
            Self::Danger => Style::default().fg(Color::Red),
            // Reversed rather than coloured, so the cursor is visible on a
            // monochrome theme where accent == foreground.
            Self::Selected => Style::default().add_modifier(Modifier::REVERSED),
            Self::Unread => Style::default().add_modifier(Modifier::BOLD),
        }
    }
}

/// Icon roles, resolved to a glyph. Every role has an ASCII fallback of the
/// **same cell width**, so layout does not shift between modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    PrOpen,
    PrDraft,
    PrMerged,
    PrClosed,
    IssueOpen,
    IssueClosed,
    CheckPass,
    CheckFail,
    CheckPending,
    Review,
    Comment,
    Mention,
    Security,
    Repo,
    Private,
}

impl Icon {
    /// Nerd Font Octicons, present in Omarchy's default JetBrainsMono Nerd
    /// Font. Verified codepoints.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::PrOpen => "\u{f407}",
            Self::PrDraft => "\u{f430}",
            Self::PrMerged => "\u{f419}",
            Self::PrClosed => "\u{f41b}",
            Self::IssueOpen => "\u{f41b}",
            Self::IssueClosed => "\u{f41d}",
            Self::CheckPass => "\u{f42e}",
            Self::CheckFail => "\u{f467}",
            Self::CheckPending => "\u{f4a1}",
            Self::Review => "\u{f420}",
            Self::Comment => "\u{f41f}",
            Self::Mention => "\u{f427}",
            Self::Security => "\u{f441}",
            Self::Repo => "\u{f401}",
            Self::Private => "\u{f46a}",
        }
    }

    /// One cell wide, matching the glyph, so nothing reflows.
    pub fn ascii(self) -> &'static str {
        match self {
            Self::PrOpen | Self::IssueOpen => "o",
            Self::PrDraft => "d",
            Self::PrMerged => "M",
            Self::PrClosed | Self::IssueClosed => "x",
            Self::CheckPass => "+",
            Self::CheckFail => "!",
            Self::CheckPending => "~",
            Self::Review => "R",
            Self::Comment => "c",
            Self::Mention => "@",
            Self::Security => "!",
            Self::Repo => "#",
            Self::Private => "P",
        }
    }

    pub fn render(self, unicode: bool) -> &'static str {
        if unicode { self.glyph() } else { self.ascii() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_does_not_rely_on_colour() {
        // Several Omarchy themes have accent == foreground, so a coloured
        // cursor would be invisible.
        let s = Role::Selected.style();
        assert!(s.add_modifier.contains(Modifier::REVERSED));
        assert!(s.fg.is_none());
    }

    #[test]
    fn unread_is_weight_not_colour() {
        let s = Role::Unread.style();
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn every_icon_has_an_ascii_fallback_of_equal_width() {
        for i in [
            Icon::PrOpen,
            Icon::PrDraft,
            Icon::PrMerged,
            Icon::PrClosed,
            Icon::IssueOpen,
            Icon::IssueClosed,
            Icon::CheckPass,
            Icon::CheckFail,
            Icon::CheckPending,
            Icon::Review,
            Icon::Comment,
            Icon::Mention,
            Icon::Security,
            Icon::Repo,
            Icon::Private,
        ] {
            assert_eq!(i.ascii().chars().count(), 1, "{i:?} ascii must be one cell");
            assert_eq!(i.glyph().chars().count(), 1, "{i:?} glyph must be one cell");
        }
    }
}

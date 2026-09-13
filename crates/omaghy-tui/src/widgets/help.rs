//! The help overlay — `spec/30-ui.md` §5.
//!
//! **Generated from the keymap, never hand-written.** A help screen
//! maintained separately is a help screen that lies, and it lies silently:
//! nothing fails when a binding changes and its help line does not.
//!
//! The only input is [`Binding`] slices — the same ones the footer shows and
//! the palette searches — so a binding cannot exist without being documented,
//! and a documented binding cannot fail to exist. Each row also prints the
//! action name, which is what the palette matches and what a `config.toml`
//! override refers to.

use crate::{
    keys::Binding,
    theme::{Icon, Icons, Role},
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

/// A titled group of bindings: "This surface", "Everywhere".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpSection<'a> {
    pub title: &'a str,
    pub bindings: &'a [Binding],
}

impl<'a> HelpSection<'a> {
    pub fn new(title: &'a str, bindings: &'a [Binding]) -> Self {
        Self { title, bindings }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpOutcome {
    /// Still open; redraw.
    Consumed,
    Dismissed,
}

#[derive(Debug, Clone)]
pub struct Help<'a> {
    sections: &'a [HelpSection<'a>],
    scroll: u16,
    icons: Icons,
}

impl<'a> Help<'a> {
    pub fn new(sections: &'a [HelpSection<'a>]) -> Self {
        Self {
            sections,
            scroll: 0,
            icons: Icons::UNICODE,
        }
    }

    pub fn icons(mut self, icons: Icons) -> Self {
        self.icons = icons;
        self
    }

    pub fn scroll(&self) -> u16 {
        self.scroll
    }

    pub fn set_scroll(&mut self, scroll: u16) {
        self.scroll = scroll.min(self.max_scroll());
    }

    fn max_scroll(&self) -> u16 {
        self.lines().len().saturating_sub(1) as u16
    }

    /// Every line of the overlay, generated. Public so a test can assert the
    /// overlay says exactly what the keymap does.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for (i, section) in self.sections.iter().enumerate() {
            if section.bindings.is_empty() {
                continue;
            }
            if i > 0 {
                lines.push(Line::raw(""));
            }
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    section.title.to_owned(),
                    Role::Accent.style().add_modifier(Modifier::BOLD),
                ),
            ]));
            for b in section.bindings {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{:<12}", b.keys),
                        Role::Default.style().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{:<22}", b.description), Role::Default.style()),
                    Span::styled(b.action.to_owned(), Role::Muted.style()),
                ]));
            }
        }
        lines
    }

    pub fn on_key(&mut self, key: KeyEvent) -> HelpOutcome {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll = (self.scroll + 1).min(self.max_scroll());
                HelpOutcome::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                HelpOutcome::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll = 0;
                HelpOutcome::Consumed
            }
            // Anything else closes: the overlay is a reference, not a mode,
            // and needing to learn a key to leave the help is a joke.
            _ => HelpOutcome::Dismissed,
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let lines = self.lines();
        let width = area.width.saturating_sub(4).clamp(1, 72);
        // Two border rows plus the "press any key" footer.
        let height = ((lines.len() as u16) + 4).min(area.height);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        };
        f.render_widget(Clear, rect);

        let visible = height.saturating_sub(4) as usize;
        let mut shown: Vec<Line<'static>> = lines
            .into_iter()
            .skip(self.scroll as usize)
            .take(visible)
            .collect();
        shown.push(Line::raw(""));
        shown.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(self.icons.get(Icon::Help).to_owned(), Role::Muted.style()),
            Span::raw(" "),
            Span::styled(
                format!("j / k scroll {} any key closes", self.icons.dot()),
                Role::Muted.style(),
            ),
        ]));

        f.render_widget(
            Paragraph::new(shown).block(Block::bordered().title(" keys ")),
            rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{keys::GLOBAL_BINDINGS, widgets::test_support::render};
    use crossterm::event::KeyModifiers;

    const SURFACE: &[Binding] = &[
        Binding::new("notification.next", "j / ↓", "next"),
        Binding::new("notification.mark-read", "Enter", "toggle read"),
    ];

    fn sections() -> Vec<HelpSection<'static>> {
        vec![
            HelpSection::new("This surface", SURFACE),
            HelpSection::new("Everywhere", GLOBAL_BINDINGS),
        ]
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn every_binding_appears_and_nothing_else_does() {
        // The point of generating it: the overlay cannot describe a key that
        // does not exist, and cannot omit one that does.
        let s = sections();
        let help = Help::new(&s);
        let body: Vec<String> = help.lines().iter().map(text).collect();
        let joined = body.join("\n");

        for b in SURFACE.iter().chain(GLOBAL_BINDINGS) {
            assert!(joined.contains(b.keys), "missing keys {:?}", b.keys);
            assert!(
                joined.contains(b.description),
                "missing description {:?}",
                b.description
            );
            assert!(
                joined.contains(b.action),
                "help must name the action so the palette and help agree: {:?}",
                b.action
            );
        }

        // Two section titles plus one row per binding, and no invented rows.
        let rows = body
            .iter()
            .filter(|l| l.starts_with("  ") && !l.trim().is_empty())
            .count();
        assert_eq!(rows, SURFACE.len() + GLOBAL_BINDINGS.len());
    }

    #[test]
    fn an_empty_section_does_not_print_a_heading() {
        // A surface with no local bindings should not advertise an empty
        // "This surface" list.
        let empty: &[Binding] = &[];
        let s = [
            HelpSection::new("This surface", empty),
            HelpSection::new("Everywhere", GLOBAL_BINDINGS),
        ];
        let body: String = Help::new(&s)
            .lines()
            .iter()
            .map(text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!body.contains("This surface"), "{body}");
        assert!(body.contains("Everywhere"));
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        let s = sections();
        let mut help = Help::new(&s);
        assert_eq!(help.scroll(), 0);
        help.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(help.scroll(), 0, "must not scroll above the first line");
        for _ in 0..200 {
            help.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        assert_eq!(help.scroll(), help.lines().len() as u16 - 1);
        help.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(help.scroll(), 0);
    }

    #[test]
    fn any_other_key_closes_it() {
        let s = sections();
        let mut help = Help::new(&s);
        assert_eq!(
            help.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            HelpOutcome::Consumed
        );
        for c in ['q', '?', 'x'] {
            let mut help = Help::new(&s);
            assert_eq!(
                help.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                HelpOutcome::Dismissed,
                "{c}"
            );
        }
        let mut help = Help::new(&s);
        assert_eq!(
            help.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            HelpOutcome::Dismissed
        );
    }

    #[test]
    fn it_fits_the_terminal_it_is_drawn_in() {
        let s = sections();
        // A small terminal must still get a usable overlay rather than an
        // overlay drawn off-screen.
        for (w, h) in [(40, 10), (80, 24), (200, 60)] {
            let out = render(w, h, |f, a| Help::new(&s).render(f, a));
            for line in out.lines() {
                assert!(line.chars().count() <= w as usize, "{line:?} at width {w}");
            }
            assert!(out.contains("closes"), "the way out is always shown");
        }
    }
}

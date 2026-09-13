//! The command palette — `spec/30-ui.md` §5.1.
//!
//! Every action carries a stable, dotted name (`notification.mark-read`),
//! `:` fuzzy-searches them, and **anything reachable by key is reachable by
//! name**. That is what makes a binding discoverable rather than trivia, and
//! it is what a `config.toml` override refers to.
//!
//! The palette therefore searches the same [`Binding`] slices that feed the
//! footer and the help overlay. There is no second list to forget to update.

use crate::{
    keys::Binding,
    theme::{Icon, Icons, Role},
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};
use tui_input::{Input, backend::crossterm::EventHandler as _};

/// Characters after which a match counts as "at the start of a word".
fn is_boundary(c: char) -> bool {
    matches!(c, '.' | '-' | '_' | ' ' | '/' | ':')
}

/// A case-insensitive subsequence match, scored so the obvious candidate
/// wins.
///
/// Returns the score and the byte-independent *character* indices that
/// matched, which is what lets the palette underline them.
pub fn fuzzy(needle: &str, haystack: &str) -> Option<(i32, Vec<usize>)> {
    let hay: Vec<char> = haystack.chars().map(|c| c.to_ascii_lowercase()).collect();
    let pat: Vec<char> = needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if pat.is_empty() {
        return Some((0, Vec::new()));
    }

    let mut positions = Vec::with_capacity(pat.len());
    let mut score = 0;
    let mut h = 0usize;
    for (i, p) in pat.iter().enumerate() {
        let found = hay[h..].iter().position(|c| c == p)? + h;
        // Matching at the start, or just after a separator, is what the user
        // almost always meant.
        if found == 0 {
            score += 16;
        } else if is_boundary(hay[found - 1]) {
            score += 8;
        }
        if i > 0 && found == h {
            score += 8; // contiguous
        }
        score -= (found - h).min(8) as i32;
        positions.push(found);
        h = found + 1;
    }
    // A short haystack matched in full beats a long one matched sparsely.
    score -= (hay.len() / 8) as i32;
    Some((score, positions))
}

/// One candidate, already scored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Index into the palette's action list.
    pub index: usize,
    pub score: i32,
    /// Character indices of the action name that matched.
    pub positions: Vec<usize>,
}

/// Close the palette, or run something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteOutcome {
    /// Still open; redraw.
    Consumed,
    /// Closed, nothing run.
    Dismissed,
    /// Closed; run this action name.
    Run(&'static str),
}

/// Bindings a config override could never name, and the palette can never
/// reach. Every surface's keymap should return an empty vector here.
pub fn unreachable_by_name(bindings: &[Binding]) -> Vec<&Binding> {
    bindings
        .iter()
        .filter(|b| b.action.trim().is_empty() || !b.action.contains('.'))
        .collect()
}

#[derive(Debug, Clone)]
pub struct Palette {
    input: Input,
    actions: Vec<Binding>,
    matches: Vec<Match>,
    selected: usize,
    icons: Icons,
}

impl Palette {
    /// Open over a list of actions — typically the global bindings plus the
    /// current surface's keymap.
    pub fn new(actions: Vec<Binding>) -> Self {
        let mut p = Self {
            input: Input::default(),
            actions,
            matches: Vec::new(),
            selected: 0,
            icons: Icons::UNICODE,
        };
        p.refilter();
        p
    }

    pub fn icons(mut self, icons: Icons) -> Self {
        self.icons = icons;
        self
    }

    pub fn query(&self) -> &str {
        self.input.value()
    }

    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    pub fn selected(&self) -> Option<&Binding> {
        self.matches
            .get(self.selected)
            .and_then(|m| self.actions.get(m.index))
    }

    fn refilter(&mut self) {
        let q = self.input.value().to_owned();
        let mut hits: Vec<Match> = self
            .actions
            .iter()
            .enumerate()
            .filter_map(|(index, b)| match fuzzy(&q, b.action) {
                Some((score, positions)) => Some(Match {
                    index,
                    score,
                    positions,
                }),
                // Falling back to the description means `:` finds "toggle
                // read" as well as `notification.mark-read`, without the
                // description outranking a real name match.
                None => fuzzy(&q, b.description).map(|(score, _)| Match {
                    index,
                    score: score - 32,
                    positions: Vec::new(),
                }),
            })
            .collect();
        // Ties break on the action name, never on iteration order, so the
        // list does not reshuffle between identical queries.
        hits.sort_by(|a, b| {
            b.score.cmp(&a.score).then_with(|| {
                self.actions[a.index]
                    .action
                    .cmp(self.actions[b.index].action)
            })
        });
        self.matches = hits;
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PaletteOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => PaletteOutcome::Dismissed,
            (KeyCode::Enter, _) => match self.selected() {
                Some(b) => PaletteOutcome::Run(b.action),
                // Enter on no matches must not close the palette: the user is
                // mid-typo, and closing discards what they typed.
                None => PaletteOutcome::Consumed,
            },
            (KeyCode::Down, _) | (KeyCode::Char('n'), true) => {
                self.move_selection(1);
                PaletteOutcome::Consumed
            }
            (KeyCode::Up, _) | (KeyCode::Char('p'), true) => {
                self.move_selection(-1);
                PaletteOutcome::Consumed
            }
            _ => {
                let before = self.input.value().to_owned();
                self.input.handle_event(&Event::Key(key));
                if self.input.value() != before {
                    // A changed query invalidates the cursor position: the
                    // best match is now at the top and that is what Enter
                    // should take.
                    self.selected = 0;
                    self.refilter();
                }
                PaletteOutcome::Consumed
            }
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let width = area.width.saturating_sub(4).clamp(1, 72);
        let rows = (self.matches.len() as u16).clamp(1, 10);
        let height = (rows + 4).min(area.height);
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + 1,
            width,
            height,
        };
        f.render_widget(Clear, rect);

        let inner_w = width.saturating_sub(2) as usize;
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    self.icons.get(Icon::Palette).to_owned(),
                    Role::Accent.style(),
                ),
                Span::raw(" "),
                Span::styled(
                    self.input.value().to_owned(),
                    Role::Default.style().add_modifier(Modifier::BOLD),
                ),
                Span::styled(self.icons.caret(), Role::Accent.style()),
            ]),
            Line::styled("─".repeat(inner_w), Role::Muted.style()),
        ];

        if self.matches.is_empty() {
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(self.icons.get(Icon::Search).to_owned(), Role::Muted.style()),
                Span::raw(" "),
                Span::styled("No action matches.", Role::Muted.style()),
            ]));
        }

        // Keep the selection on screen without a scrollbar nobody can size.
        let first = self.selected.saturating_sub(rows as usize - 1);
        for (i, m) in self
            .matches
            .iter()
            .enumerate()
            .skip(first)
            .take(rows as usize)
        {
            let binding = &self.actions[m.index];
            let picked = i == self.selected;
            let name_w = inner_w.saturating_sub(20).clamp(8, 30);
            let mut spans = vec![
                Span::styled(
                    if picked { self.icons.cursor() } else { " " },
                    Role::Accent.style(),
                ),
                Span::raw(" "),
            ];
            spans.extend(highlight(binding.action, &m.positions, name_w, picked));
            spans.push(Span::raw("  "));
            let keys_w = 10usize;
            let desc_w = inner_w.saturating_sub(2 + name_w + 2 + keys_w + 1);
            spans.push(Span::styled(
                pad(binding.description, desc_w),
                Role::Muted.style(),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                pad_left(binding.keys, keys_w),
                Role::Accent.style(),
            ));
            let line = Line::from(spans);
            lines.push(if picked {
                line.style(Role::Selected.style())
            } else {
                line
            });
        }

        f.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(" command ")),
            rect,
        );
    }
}

/// Underline the characters that matched — a modifier, not a colour, because
/// the accent colour equals the foreground on several Omarchy themes.
fn highlight(text: &str, positions: &[usize], width: usize, picked: bool) -> Vec<Span<'static>> {
    let base = if picked {
        Role::Default.style().add_modifier(Modifier::BOLD)
    } else {
        Role::Default.style()
    };
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut buf_hit = false;
    let mut shown = 0usize;
    for (i, c) in text.chars().enumerate() {
        if shown >= width {
            break;
        }
        let hit = positions.contains(&i);
        if hit != buf_hit && !buf.is_empty() {
            out.push(Span::styled(
                std::mem::take(&mut buf),
                if buf_hit {
                    base.add_modifier(Modifier::UNDERLINED)
                } else {
                    base
                },
            ));
        }
        buf_hit = hit;
        buf.push(c);
        shown += 1;
    }
    if !buf.is_empty() {
        out.push(Span::styled(
            buf,
            if buf_hit {
                base.add_modifier(Modifier::UNDERLINED)
            } else {
                base
            },
        ));
    }
    if shown < width {
        out.push(Span::raw(" ".repeat(width - shown)));
    }
    out
}

fn pad(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    let len = out.chars().count();
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(len)));
    out
}

fn pad_left(s: &str, width: usize) -> String {
    let cut: String = s.chars().take(width).collect();
    let len = cut.chars().count();
    let mut out: String = std::iter::repeat_n(' ', width.saturating_sub(len)).collect();
    out.push_str(&cut);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::GLOBAL_BINDINGS;

    fn actions() -> Vec<Binding> {
        vec![
            Binding::new("notification.mark-read", "Enter", "toggle read"),
            Binding::new("notification.next", "j / ↓", "next"),
            Binding::new("notification.unread-only", "u", "unread only"),
            Binding::new("pr.approve", "a", "approve"),
            Binding::new("repo.open-in-browser", "o", "open on github.com"),
            Binding::new("app.quit", "q / Ctrl-C", "quit"),
        ]
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn type_in(p: &mut Palette, s: &str) {
        for c in s.chars() {
            p.on_key(key(c));
        }
    }

    #[test]
    fn an_empty_query_offers_everything() {
        let p = Palette::new(actions());
        assert_eq!(p.matches().len(), 6);
    }

    #[test]
    fn abbreviations_find_the_action_they_abbreviate() {
        let mut p = Palette::new(actions());
        type_in(&mut p, "nmr");
        assert_eq!(
            p.selected().map(|b| b.action),
            Some("notification.mark-read")
        );

        let mut p = Palette::new(actions());
        type_in(&mut p, "approve");
        assert_eq!(p.selected().map(|b| b.action), Some("pr.approve"));
    }

    #[test]
    fn a_word_from_the_description_still_finds_the_action() {
        // "anything reachable by key is reachable by name" is the rule; being
        // findable by what it does as well is what makes it usable.
        let mut p = Palette::new(actions());
        type_in(&mut p, "browser");
        assert_eq!(p.selected().map(|b| b.action), Some("repo.open-in-browser"));
    }

    #[test]
    fn spaces_in_a_query_do_not_break_the_match() {
        let mut p = Palette::new(actions());
        type_in(&mut p, "mark read");
        assert_eq!(
            p.selected().map(|b| b.action),
            Some("notification.mark-read")
        );
    }

    #[test]
    fn a_query_that_matches_nothing_says_so_rather_than_guessing() {
        let mut p = Palette::new(actions());
        type_in(&mut p, "zzzz");
        assert!(p.matches().is_empty());
        assert_eq!(p.selected(), None);
        // Enter on nothing keeps the palette open: the user is mid-typo and
        // closing would discard what they typed.
        assert_eq!(
            p.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PaletteOutcome::Consumed
        );
    }

    #[test]
    fn ordering_is_by_score_then_name_so_it_never_reshuffles() {
        let mut a = Palette::new(actions());
        type_in(&mut a, "no");
        let mut b = Palette::new(actions());
        type_in(&mut b, "no");
        let names = |p: &Palette| -> Vec<&'static str> {
            p.matches()
                .iter()
                .map(|m| actions()[m.index].action)
                .collect()
        };
        assert_eq!(names(&a), names(&b));
    }

    #[test]
    fn editing_the_query_resets_the_cursor_to_the_best_match() {
        let mut p = Palette::new(actions());
        p.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        p.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        type_in(&mut p, "quit");
        assert_eq!(p.selected().map(|b| b.action), Some("app.quit"));
    }

    #[test]
    fn backspace_widens_the_search_again() {
        let mut p = Palette::new(actions());
        type_in(&mut p, "quitx");
        assert!(p.matches().is_empty());
        p.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(p.query(), "quit");
        assert_eq!(p.selected().map(|b| b.action), Some("app.quit"));
    }

    #[test]
    fn the_selection_wraps_rather_than_sticking_at_the_ends() {
        let mut p = Palette::new(actions());
        p.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        let last = p.selected().map(|b| b.action);
        p.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_ne!(p.selected().map(|b| b.action), last);
    }

    #[test]
    fn escape_dismisses_and_enter_runs() {
        let mut p = Palette::new(actions());
        type_in(&mut p, "quit");
        assert_eq!(
            p.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PaletteOutcome::Run("app.quit")
        );
        let mut p = Palette::new(actions());
        assert_eq!(
            p.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            PaletteOutcome::Dismissed
        );
    }

    #[test]
    fn every_global_binding_is_reachable_by_name() {
        // The rule of §5.1, enforced rather than asserted in prose.
        assert!(
            unreachable_by_name(GLOBAL_BINDINGS).is_empty(),
            "{:?}",
            unreachable_by_name(GLOBAL_BINDINGS)
        );
        let bad = [Binding::new("quit", "q", "quit")];
        assert_eq!(unreachable_by_name(&bad).len(), 1);
    }

    #[test]
    fn matched_characters_are_underlined_not_merely_coloured() {
        let spans = highlight("notification.mark-read", &[0, 13, 18], 24, false);
        let underlined: String = spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::UNDERLINED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(underlined, "nmr");
        assert!(
            spans.iter().all(|s| s.style.fg.is_none()),
            "the highlight must not depend on a colour"
        );
    }
}

//! The settings overlay — `spec/40-config.md` §6.
//!
//! **A panel, not a surface.** §6 puts settings alongside help and the
//! palette rather than among the seven numbered surfaces, and §6.2 asks that
//! changing `rows` redraw the inbox *behind* it. Both want an overlay: the
//! screen underneath keeps drawing, so the effect of a change is visible in
//! the same frame that makes it.
//!
//! **Every row says where its value came from.** §6.1 calls this the thing a
//! settings screen usually gets wrong — someone edits the file, sees no
//! change, and cannot tell what is winning. It is one column here.
//!
//! **Nothing in this file knows what a setting is.** The rows come from
//! [`SettingId::ALL`], their words from `description()`, their values from
//! `options()`. Adding a setting is one arm in `config.rs`, not an edit here.

use crate::{
    config::{Config, SettingId},
    theme::Role,
    widgets::list::wrap,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

/// Width the panel would like. Wide enough for the longest description plus
/// the value and source columns, and it shrinks to the terminal.
const WANTED_WIDTH: u16 = 76;
const VALUE_COL: usize = 18;
const SOURCE_COL: usize = 13;

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOutcome {
    /// Cursor moved, or nothing happened. Redraw.
    Consumed,
    /// This setting changed; the caller applies and persists it.
    Changed(SettingId),
    Dismissed,
}

/// The cursor, and the rendering. State is the caller's.
#[derive(Debug, Clone, Copy, Default)]
pub struct Settings {
    pub cursor: usize,
}

impl Settings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rows() -> usize {
        SettingId::ALL.len()
    }

    pub fn selected(self) -> SettingId {
        SettingId::ALL[self.cursor.min(SettingId::ALL.len() - 1)]
    }

    pub fn next(&mut self) {
        self.cursor = (self.cursor + 1) % Self::rows();
    }

    pub fn prev(&mut self) {
        self.cursor = (self.cursor + Self::rows() - 1) % Self::rows();
    }

    /// The panel, drawn over whatever is behind it.
    ///
    /// Laid out width-first: the note and the alternatives are wrapped to the
    /// panel's inner width, and only then is the height counted. Building the
    /// lines first and sizing afterwards is what let a note run off the edge —
    /// a `Paragraph` does not wrap, so the half of the sentence that said
    /// *why* a save failed was simply not drawn.
    pub fn render(self, f: &mut Frame, area: Rect, cfg: &Config, note: Option<&str>) {
        let w = WANTED_WIDTH.min(area.width);
        // Two cells of border, two of the indent every line carries.
        let text_width = (w as usize).saturating_sub(4);

        let mut lines: Vec<Line> = Vec::with_capacity(SettingId::ALL.len() + 8);
        lines.push(Line::styled("  Settings", Role::Accent.style()));
        lines.push(Line::raw(""));

        let mut section = "";
        for (i, id) in SettingId::ALL.iter().copied().enumerate() {
            if id.section() != section {
                section = id.section();
                lines.push(Line::styled(
                    format!("  [{section}]"),
                    Role::Muted.style().add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(self.row(id, cfg, i == self.cursor));
        }

        lines.push(Line::raw(""));
        // The description of the focused row only: eight of them at once is a
        // wall of text, and the one that matters is the one under the cursor.
        for l in wrap(self.selected().description(), text_width) {
            lines.push(Line::styled(format!("  {l}"), Role::Muted.style()));
        }
        for l in wrap(&self.alternatives(cfg), text_width) {
            lines.push(Line::styled(format!("  {l}"), Role::Muted.style()));
        }
        lines.push(Line::raw(""));
        match note {
            // §6.3: a failed write does not refuse the change, and does not
            // fail silently. Wrapped, because the part of that sentence worth
            // reading — the reason — is at the end of it.
            Some(n) => {
                for l in wrap(n, text_width) {
                    lines.push(Line::styled(format!("  {l}"), Role::Warning.style()));
                }
            }
            None => lines.push(Line::styled(
                "  j/k move   h/l or Enter change   , or Esc close",
                Role::Muted.style(),
            )),
        }

        let h = (lines.len() as u16 + 2).min(area.height);
        let r = Rect {
            x: area.x + (area.width.saturating_sub(w)) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, r);
        f.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(" settings ")),
            r,
        );
    }

    fn row(self, id: SettingId, cfg: &Config, focused: bool) -> Line<'static> {
        // The cursor is a column, never only a highlight: reverse video is
        // invisible on terminals that do not do it, and in every screenshot
        // (`30-ui.md` §4.1).
        let marker = if focused { "> " } else { "  " };
        let name = format!("{marker}{:<20}", id.key());
        let value = format!("{:<VALUE_COL$}", id.current(cfg));
        let source = format!("{:<SOURCE_COL$}", id.source(cfg).label());

        let name_style = if focused {
            Role::Default.style().add_modifier(Modifier::BOLD)
        } else {
            Role::Default.style()
        };
        // The source is dimmed unless it is the one that needs reading: a
        // change that could not be saved is the only alarming provenance.
        let source_style = match id.source(cfg) {
            crate::config::Source::Unsaved => Role::Danger.style(),
            _ => Role::Muted.style(),
        };
        Line::from(vec![
            Span::styled(name, name_style),
            Span::styled(value, Role::Accent.style()),
            Span::styled(source, source_style),
        ])
    }

    /// The other values, with the current one marked — §6.1 shows the
    /// alternatives, not just what is set.
    fn alternatives(self, cfg: &Config) -> String {
        let id = self.selected();
        let current = id.current(cfg);
        let shown: Vec<String> = id
            .options()
            .into_iter()
            .map(|o| if o == current { format!("[{o}]") } else { o })
            .collect();
        shown.join("  ")
    }
}

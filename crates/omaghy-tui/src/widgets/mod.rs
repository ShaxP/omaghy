//! The shared widget set — `spec/30-ui.md` §4, §5, §6 and §8.
//!
//! Surfaces compose these; they do not reimplement them. That is the whole
//! point: the state matrix looks identical on every surface because there is
//! one renderer for it, the help overlay cannot go stale because it is
//! generated from the keymap, and a row's columns drop at the same widths
//! everywhere because one table says so.
//!
//! Three rules hold across all of them:
//!
//! **No I/O, and no clock.** A widget that needed data would be a `Store`
//! change; a widget that needed the time takes a `now` or a tick counter as
//! a parameter. This is what makes every one of them snapshot-testable.
//!
//! **The sixteen ANSI colours are sufficient.** Truecolor is an enhancement.
//!
//! **Colour is never the only encoding.** Every state carries a glyph, every
//! selection carries a glyph *and* a modifier, and unread is a marker column
//! *and* weight.

pub mod chrome;
pub mod help;
pub mod list;
pub mod palette;
pub mod state;
pub mod toast;

pub use chrome::{Footer, Freshness, Header, footer, header};
pub use help::{Help, HelpOutcome, HelpSection};
pub use list::{Cell, Column, Columns, Row, RowList, elide, elide_owner, row_line, shared_repo};
pub use palette::{Match, Palette, PaletteOutcome, fuzzy, unreachable_by_name};
pub use state::{
    Body, Conditions, EmptyCopy, StateView, SurfaceState, classify, empty_state, notice,
};
pub use toast::{Level, Toast};

#[cfg(test)]
mod snapshots;

/// Rendering helpers shared by the widget tests.
#[cfg(test)]
pub(crate) mod test_support {
    use ratatui::{Frame, Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

    /// Draw into a `TestBackend` of exactly this size and keep the buffer, so
    /// styles can be asserted as well as text.
    pub fn buffer(w: u16, h: u16, draw: impl FnOnce(&mut Frame, Rect)) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(w, h)).expect("test backend");
        t.draw(|f| {
            let area = f.area();
            draw(f, area);
        })
        .expect("draw");
        t.backend().buffer().clone()
    }

    /// The screen as text, trailing spaces trimmed so a snapshot diff shows
    /// content rather than padding.
    pub fn text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn render(w: u16, h: u16, draw: impl FnOnce(&mut Frame, Rect)) -> String {
        text(&buffer(w, h, draw))
    }
}

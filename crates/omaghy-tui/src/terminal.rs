//! Terminal setup and — more importantly — teardown.
//!
//! Raw mode plus the alternate screen means a panic that escapes leaves the
//! user with no echo, no line editing and no prompt. Restoring is therefore
//! not best-effort: it happens on every exit path, including a panic, and it
//! happens *before* the panic message is printed so the message is readable.
//!
//! See `spec/30-ui.md` §2.1.

use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    io::{self, Stdout},
    sync::atomic::{AtomicBool, Ordering},
};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Whether the terminal is currently in raw mode + alternate screen.
///
/// Restoration is idempotent: the panic hook and the `Drop` guard both fire on
/// an unwinding panic, and the second must be a no-op rather than emitting a
/// stray escape sequence into a restored terminal.
static RAW: AtomicBool = AtomicBool::new(false);

/// Enter raw mode and the alternate screen, and install the panic hook.
pub fn init() -> io::Result<Tui> {
    install_panic_hook();
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    RAW.store(true, Ordering::SeqCst);
    let mut tui = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    tui.hide_cursor()?;
    tui.clear()?;
    Ok(tui)
}

/// Leave the alternate screen and raw mode. Safe to call more than once.
pub fn restore() -> io::Result<()> {
    if !RAW.swap(false, Ordering::SeqCst) {
        return Ok(());
    }
    execute!(io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    crossterm::execute!(io::stdout(), crossterm::cursor::Show)?;
    Ok(())
}

/// Whether the terminal still needs restoring. For tests and for `Drop`.
pub fn is_raw() -> bool {
    RAW.load(Ordering::SeqCst)
}

/// Restores on drop, so `?` propagating out of the run loop cannot strand the
/// terminal.
#[derive(Debug)]
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = restore();
    }
}

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Restore the terminal *before* the panic message is printed, then chain to
/// whatever hook was already installed so the message still appears.
fn install_panic_hook() {
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        previous(info);
    }));
}

/// Whether the panic hook has been installed. Asserted by a test, because a
/// missing hook is invisible until the worst possible moment.
pub fn panic_hook_installed() -> bool {
    HOOK_INSTALLED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_is_idempotent_and_safe_when_never_initialised() {
        // The panic hook and the Drop guard both fire on an unwinding panic.
        // The second must not emit escape sequences into a restored terminal.
        assert!(!is_raw());
        assert!(restore().is_ok());
        assert!(restore().is_ok());
    }

    #[test]
    fn the_hook_is_installed_exactly_once() {
        install_panic_hook();
        assert!(panic_hook_installed());
        install_panic_hook();
        assert!(panic_hook_installed());
    }
}

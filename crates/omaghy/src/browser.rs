//! Launching a browser.
//!
//! The half of `o` that `omaghy-tui` may not do, because spawning a process is
//! I/O (`CONTRIBUTING.md`). The seam is `omaghy_tui::open::Opener`.

use omaghy_tui::open::Opener;
use std::process::{Command, Stdio};

/// `$BROWSER` if it is set, `xdg-open` otherwise.
///
/// `$BROWSER` first because it is the answer a user has already given, and a
/// desktop-wide default is a worse guess than an explicit one.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemBrowser;

impl Opener for SystemBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        let program = std::env::var("BROWSER")
            .ok()
            .filter(|b| !b.trim().is_empty())
            .unwrap_or_else(|| "xdg-open".to_owned());

        // Detached, and with every stream closed. A browser that inherits the
        // terminal will print to the screen omaghy is drawing on — and the
        // first thing a stray line does is corrupt the frame. Not waited on
        // either: `xdg-open` returns quickly, but a browser started cold can
        // take seconds, and blocking the event loop on it would look exactly
        // like a hang.
        Command::new(&program)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => format!(
                    "`{program}` is not installed (xdg-utils provides xdg-open, \
                     or set $BROWSER)"
                ),
                _ => format!("`{program}` would not start: {e}"),
            })
    }
}

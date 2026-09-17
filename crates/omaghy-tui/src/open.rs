//! Opening a thing on github.com — `spec/00-overview.md` §1.
//!
//! omaghy is not a browser replacement. Where the web UI is genuinely better —
//! a rich diff on a huge PR, a project board, settings — its job is to get you
//! there fast, and `o` is that.
//!
//! **A seam, not a call.** Launching a browser is spawning a process, and
//! `omaghy-tui` performs no I/O (`CONTRIBUTING.md`). So a surface says *what*
//! to open and something outside this crate does the opening — the same shape
//! as the `Store` and `ConfigWriter` seams, for the same reason. It also keeps
//! every test honest: nothing in this crate can launch a browser during
//! `cargo test`, which is not a thing to discover by having it happen.

/// Opens a URL. Implemented outside `omaghy-tui`.
pub trait Opener: std::fmt::Debug + Send + Sync {
    /// `Err` is the message shown to the user. The URL is included by the
    /// caller, because an error that does not say *what* failed to open is
    /// one you cannot act on.
    fn open(&self, url: &str) -> Result<(), String>;
}

/// The opener that opens nothing and says so.
///
/// The default, so a test or the fixture path cannot start a browser by
/// accident. It reports rather than silently succeeding: a stub that returns
/// `Ok` would make "nothing happened" indistinguishable from "it worked".
#[derive(Debug, Clone, Copy)]
pub struct NoOpener;

impl Opener for NoOpener {
    fn open(&self, _url: &str) -> Result<(), String> {
        Err("no browser is wired up in this build".to_owned())
    }
}

/// Records what it was asked to open. For tests.
#[derive(Debug, Default)]
pub struct RecordOpened {
    opened: std::sync::Mutex<Vec<String>>,
}

impl RecordOpened {
    pub fn urls(&self) -> Vec<String> {
        self.opened.lock().expect("not poisoned").clone()
    }
}

impl Opener for RecordOpened {
    fn open(&self, url: &str) -> Result<(), String> {
        self.opened
            .lock()
            .expect("not poisoned")
            .push(url.to_owned());
        Ok(())
    }
}

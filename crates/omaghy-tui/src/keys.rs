//! Bindings. Every action carries a stable name so it is reachable from the
//! palette as well as by key, and so the help overlay can be generated rather
//! than written — a help screen maintained separately is a help screen that
//! lies.
//!
//! See `spec/30-ui.md` §5.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Stable, dotted, and what a config override refers to.
    pub action: &'static str,
    /// How it is shown in help and the footer.
    pub keys: &'static str,
    pub description: &'static str,
    /// The key the palette presses to run this action.
    ///
    /// `30-ui.md` §5.1: anything reachable by key must be reachable by name.
    /// The cheapest way to *guarantee* that rather than assert it is to make
    /// the name resolve to the key — so running an action from the palette
    /// replays this through the same handler a keypress uses, and the two
    /// cannot drift into doing different things.
    ///
    /// `None` means the binding describes a range rather than one key, like
    /// `1–7`. Those reach the palette another way; see [`SURFACE_BINDINGS`].
    pub run: Option<KeyCode>,
    /// Whether [`Self::run`] is held with Control.
    pub run_ctrl: bool,
}

impl Binding {
    pub const fn new(action: &'static str, keys: &'static str, description: &'static str) -> Self {
        Self {
            action,
            keys,
            description,
            run: None,
            run_ctrl: false,
        }
    }

    /// The key that runs it. `keys` is for reading; this is for pressing.
    #[must_use]
    pub const fn on(mut self, key: KeyCode) -> Self {
        self.run = Some(key);
        self
    }

    #[must_use]
    pub const fn on_ctrl(mut self, key: KeyCode) -> Self {
        self.run = Some(key);
        self.run_ctrl = true;
        self
    }

    /// The event to replay to run this action, if it has one.
    pub fn run_event(&self) -> Option<KeyEvent> {
        self.run.map(|code| {
            KeyEvent::new(
                code,
                if self.run_ctrl {
                    KeyModifiers::CONTROL
                } else {
                    KeyModifiers::NONE
                },
            )
        })
    }
}

/// Keys handled before any surface sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Global {
    Quit,
    Back,
    Help,
    Palette,
    /// `40-config.md` §6: cross-cutting, like help and the palette — and
    /// deliberately not a numbered surface, because renumbering `1`–`7` to
    /// make room would break a keystroke people have in their fingers.
    Settings,
    Refresh,
    OpenInBrowser,
    Surface(usize),
}

pub const GLOBAL_BINDINGS: &[Binding] = &[
    Binding::new("app.quit", "q / Ctrl-C", "quit").on(KeyCode::Char('q')),
    Binding::new("app.back", "Esc", "back").on(KeyCode::Esc),
    Binding::new("app.help", "?", "this screen").on(KeyCode::Char('?')),
    Binding::new("app.palette", ":", "command palette").on(KeyCode::Char(':')),
    Binding::new("app.settings", ",", "settings").on(KeyCode::Char(',')),
    Binding::new("app.refresh", "r", "refresh").on(KeyCode::Char('r')),
    Binding::new("app.open", "o", "open on github.com").on(KeyCode::Char('o')),
    // No single key: `1–7` is seven actions wearing one row in the help
    // overlay. The palette lists them by name instead — see
    // [`SURFACE_BINDINGS`], which is generated so adding a surface adds its
    // palette entry too.
    Binding::new("app.surface", "1–7", "jump to surface"),
];

/// One palette entry per surface, by name.
///
/// "Jump to surface" is not something anyone searches for; "pull requests" is.
/// Generated from [`SurfaceId::ALL`] so a new surface cannot be added without
/// becoming reachable by name.
pub fn surface_bindings() -> Vec<Binding> {
    crate::route::SurfaceId::ALL
        .iter()
        .enumerate()
        .map(|(i, id)| Binding {
            action: id.palette_action(),
            keys: id.palette_key(),
            description: id.title(),
            run: Some(KeyCode::Char(
                char::from_digit(i as u32 + 1, 10).unwrap_or('1'),
            )),
            run_ctrl: false,
        })
        .collect()
}

/// Resolve a key to a global action, or `None` to let the surface have it.
///
/// `q` is deliberately *not* global when a surface is stacked — it pops
/// instead, so leaving a detail view does not quit the program.
pub fn resolve(key: KeyEvent, depth: usize) -> Option<Global> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, ctrl) {
        (KeyCode::Char('c'), true) => Some(Global::Quit),
        (KeyCode::Char('q'), false) if depth <= 1 => Some(Global::Quit),
        (KeyCode::Char('q'), false) | (KeyCode::Esc, _) => Some(Global::Back),
        (KeyCode::Char('?'), false) => Some(Global::Help),
        (KeyCode::Char(':'), false) => Some(Global::Palette),
        (KeyCode::Char(','), false) => Some(Global::Settings),
        (KeyCode::Char('r'), false) => Some(Global::Refresh),
        (KeyCode::Char('o'), false) => Some(Global::OpenInBrowser),
        (KeyCode::Char(c @ '1'..='7'), false) => {
            Some(Global::Surface(c.to_digit(10).unwrap() as usize - 1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn q_quits_at_the_root_and_pops_below_it() {
        // Leaving a detail view must not quit the program.
        assert_eq!(resolve(key('q'), 1), Some(Global::Quit));
        assert_eq!(resolve(key('q'), 2), Some(Global::Back));
    }

    #[test]
    fn ctrl_c_always_quits() {
        let k = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(resolve(k, 1), Some(Global::Quit));
        assert_eq!(resolve(k, 5), Some(Global::Quit));
    }

    #[test]
    fn unclaimed_keys_fall_through_to_the_surface() {
        for c in ['j', 'k', 'a', '/', 'x'] {
            assert_eq!(resolve(key(c), 1), None, "{c} should reach the surface");
        }
    }

    #[test]
    fn digits_select_surfaces_by_index() {
        assert_eq!(resolve(key('1'), 1), Some(Global::Surface(0)));
        assert_eq!(resolve(key('7'), 1), Some(Global::Surface(6)));
        assert_eq!(resolve(key('8'), 1), None);
    }

    #[test]
    fn every_global_binding_has_a_dotted_action_name() {
        for b in GLOBAL_BINDINGS {
            assert!(b.action.contains('.'), "{} should be dotted", b.action);
            assert!(!b.description.is_empty());
        }
    }
}

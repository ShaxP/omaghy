//! Semantic colour roles and icon roles.
//!
//! The 16 ANSI colours must be sufficient: Omarchy themes the terminal, so
//! omaghy inherits the active theme for free. Roles are named by meaning, so
//! no call site names a colour.
//!
//! **Colour is never the only encoding** — several Omarchy themes resolve
//! `accent` and `foreground` to the same value (Osaka Jade: both `#cacccc`).
//! A failing check is a glyph that is *also* red, never red text alone. Every
//! role that carries meaning therefore pairs its colour with a glyph, a
//! modifier, or a position; [`Role::pairs_with_a_glyph`] records which ones
//! the rule applies to, and a test enforces it.
//!
//! See `spec/30-ui.md` §6 and §7.

use ratatui::style::{Color, Modifier, Style};

/// The ten semantic colour roles of `spec/30-ui.md` §7.
///
/// `Added` and `Removed` are listed there and were missing from the P0.4
/// shell; they arrive here so diff rendering in M2 has somewhere to point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Default,
    Muted,
    Accent,
    Success,
    Warning,
    Danger,
    Added,
    Removed,
    Selected,
    Unread,
}

impl Role {
    pub const ALL: [Role; 10] = [
        Self::Default,
        Self::Muted,
        Self::Accent,
        Self::Success,
        Self::Warning,
        Self::Danger,
        Self::Added,
        Self::Removed,
        Self::Selected,
        Self::Unread,
    ];

    pub fn style(self) -> Style {
        match self {
            Self::Default => Style::default(),
            Self::Muted => Style::default().fg(Color::DarkGray),
            Self::Accent => Style::default().fg(Color::Cyan),
            Self::Success => Style::default().fg(Color::Green),
            Self::Warning => Style::default().fg(Color::Yellow),
            Self::Danger => Style::default().fg(Color::Red),
            Self::Added => Style::default().fg(Color::Green),
            Self::Removed => Style::default().fg(Color::Red),
            // Reversed rather than coloured, so the cursor is visible on a
            // monochrome theme where accent == foreground.
            Self::Selected => Style::default().add_modifier(Modifier::REVERSED),
            Self::Unread => Style::default().add_modifier(Modifier::BOLD),
        }
    }

    /// Whether a call site using this role **must** also supply a glyph.
    ///
    /// True for every role whose whole job is to say "this one is different":
    /// on a monochrome theme the colour vanishes and only the glyph is left.
    /// `Selected` and `Unread` are exempt because they already carry a
    /// modifier rather than a colour.
    pub fn pairs_with_a_glyph(self) -> bool {
        matches!(
            self,
            Self::Success | Self::Warning | Self::Danger | Self::Added | Self::Removed
        )
    }
}

/// Whether glyphs are drawn as Nerd Font Octicons or as ASCII.
///
/// A user without a Nerd Font sees `[!]`-style ASCII rather than a row of
/// replacement boxes, and the two modes occupy the same number of cells, so
/// nothing reflows when the mode changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconMode {
    #[default]
    Unicode,
    Ascii,
}

impl IconMode {
    /// Decide from environment *values*, not from the environment.
    ///
    /// The caller reads the variables; this crate performs no I/O. `config`
    /// is `icons = "…"` from `config.toml` and wins outright when it names a
    /// mode we recognise.
    pub fn resolve(config: Option<&str>, term: Option<&str>, lang: Option<&str>) -> Self {
        match config.map(str::trim) {
            Some("ascii") => return Self::Ascii,
            Some("unicode" | "nerdfont") => return Self::Unicode,
            _ => {}
        }
        // `TERM=linux` is the kernel console: no Nerd Font is possible there.
        if term == Some("linux") || term == Some("dumb") {
            return Self::Ascii;
        }
        // A non-UTF-8 locale cannot carry the codepoints at all.
        match lang {
            Some(l) if !l.to_ascii_lowercase().contains("utf") => Self::Ascii,
            _ => Self::Unicode,
        }
    }
}

/// A resolved icon table. Cheap to copy, passed down to every widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Icons(IconMode);

impl Icons {
    pub const UNICODE: Self = Self(IconMode::Unicode);
    pub const ASCII: Self = Self(IconMode::Ascii);

    pub fn new(mode: IconMode) -> Self {
        Self(mode)
    }

    pub fn mode(self) -> IconMode {
        self.0
    }

    pub fn unicode(self) -> bool {
        self.0 == IconMode::Unicode
    }

    /// The glyph for a role, one cell wide in both modes.
    pub fn get(self, icon: Icon) -> &'static str {
        icon.render(self.unicode())
    }

    /// One cell, so an elided string is exactly as wide as its budget.
    pub fn ellipsis(self) -> &'static str {
        if self.unicode() { "…" } else { "~" }
    }

    /// A separator for header segments: `title · scope · counts`.
    pub fn dot(self) -> &'static str {
        if self.unicode() { "·" } else { "-" }
    }

    /// The marker on the palette's and the list's focused row. Position plus
    /// a glyph, because the reverse-video highlight is invisible in a
    /// screenshot and absent on some terminals.
    pub fn cursor(self) -> &'static str {
        if self.unicode() { "▸" } else { ">" }
    }

    /// One frame of the activity spinner.
    ///
    /// Not braille: **JetBrainsMono Nerd Font has no braille block at all**
    /// (verified against the installed font — U+2801 is absent), so the usual
    /// `⠋⠙⠹…` spinner renders as four replacement boxes on Omarchy's default
    /// terminal font. Quadrant blocks are present and rotate just as well.
    pub fn spinner(self, frame: usize) -> &'static str {
        const UNICODE: [&str; 4] = ["▘", "▝", "▗", "▖"];
        const ASCII: [&str; 4] = ["|", "/", "-", "\\"];
        let i = frame % 4;
        if self.unicode() { UNICODE[i] } else { ASCII[i] }
    }

    /// The text cursor in the palette and the filter line.
    pub fn caret(self) -> &'static str {
        if self.unicode() { "▏" } else { "_" }
    }

    /// The bar drawn in a skeleton row while a cold cache loads.
    pub fn skeleton(self) -> &'static str {
        if self.unicode() { "░" } else { "-" }
    }
}

/// Icon roles, resolved to a glyph. Every role has an ASCII fallback of the
/// **same cell width**, so layout does not shift between modes.
///
/// The codepoints are Nerd Font Octicons, and each was checked against the
/// `cmap` of the installed `JetBrainsMonoNerdFont-Regular.ttf` *by glyph
/// name*, not merely for presence. The P0.4 table was verified the weaker
/// way and several entries were wrong as a result: `pr-draft` pointed at
/// `oct-read`, `pr-closed` at `oct-issue_opened`, `mention` at `oct-rocket`,
/// `security` at `oct-eye`, `private` at `oct-sync`, `review` at
/// `oct-question` and `check-pending` at `oct-verified`. All are corrected
/// below, with the Octicon name in a comment so the next reader can check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Icon {
    // --- the fifteen roles named in spec/30-ui.md §6 ---
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

    // --- subject kinds the notifications inbox also has to render ---
    Discussion,
    Release,
    Commit,
    Workflow,

    // --- list and row furniture ---
    Unread,
    Actor,
    Age,

    // --- state-matrix and chrome roles ---
    Refreshing,
    Stale,
    Offline,
    RateLimited,
    Forbidden,
    Error,
    Warning,
    Info,
    CaughtUp,
    Filter,
    Search,
    Palette,
    Help,
}

impl Icon {
    /// Every role, so tests can be exhaustive over the table.
    pub const ALL: [Icon; 35] = [
        Self::PrOpen,
        Self::PrDraft,
        Self::PrMerged,
        Self::PrClosed,
        Self::IssueOpen,
        Self::IssueClosed,
        Self::CheckPass,
        Self::CheckFail,
        Self::CheckPending,
        Self::Review,
        Self::Comment,
        Self::Mention,
        Self::Security,
        Self::Repo,
        Self::Private,
        Self::Discussion,
        Self::Release,
        Self::Commit,
        Self::Workflow,
        Self::Unread,
        Self::Actor,
        Self::Age,
        Self::Refreshing,
        Self::Stale,
        Self::Offline,
        Self::RateLimited,
        Self::Forbidden,
        Self::Error,
        Self::Warning,
        Self::Info,
        Self::CaughtUp,
        Self::Filter,
        Self::Search,
        Self::Palette,
        Self::Help,
    ];

    /// Nerd Font Octicons, present in Omarchy's default JetBrainsMono Nerd
    /// Font. The name in each comment is the font's own glyph name.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::PrOpen => "\u{f407}",       // oct-git_pull_request
            Self::PrDraft => "\u{f4dd}",      // oct-git_pull_request_draft
            Self::PrMerged => "\u{f419}",     // oct-git_merge
            Self::PrClosed => "\u{f4dc}",     // oct-git_pull_request_closed
            Self::IssueOpen => "\u{f41b}",    // oct-issue_opened
            Self::IssueClosed => "\u{f41d}",  // oct-issue_closed
            Self::CheckPass => "\u{f42e}",    // oct-check
            Self::CheckFail => "\u{f467}",    // oct-x
            Self::CheckPending => "\u{f4e3}", // oct-hourglass
            Self::Review => "\u{f4af}",       // oct-code_review
            Self::Comment => "\u{f41f}",      // oct-comment
            Self::Mention => "\u{f486}",      // oct-mention
            Self::Security => "\u{f49c}",     // oct-shield
            Self::Repo => "\u{f401}",         // oct-repo
            Self::Private => "\u{f456}",      // oct-lock
            Self::Discussion => "\u{f442}",   // oct-comment_discussion
            Self::Release => "\u{f412}",      // oct-tag
            Self::Commit => "\u{f417}",       // oct-git_commit
            Self::Workflow => "\u{f52e}",     // oct-workflow
            Self::Unread => "\u{f444}",       // oct-dot_fill
            Self::Actor => "\u{f415}",        // oct-person
            Self::Age => "\u{f43a}",          // oct-clock
            Self::Refreshing => "\u{f46a}",   // oct-sync
            Self::Stale => "\u{f4ab}",        // oct-clock_fill
            Self::Offline => "\u{f4ad}",      // oct-cloud_offline
            Self::RateLimited => "\u{f520}",  // oct-stopwatch
            Self::Forbidden => "\u{f4f4}",    // oct-no_entry
            Self::Error => "\u{f40c}",        // oct-alert_fill
            Self::Warning => "\u{f421}",      // oct-alert
            Self::Info => "\u{f449}",         // oct-info
            Self::CaughtUp => "\u{f49e}",     // oct-check_circle
            Self::Filter => "\u{f4d7}",       // oct-filter
            Self::Search => "\u{f422}",       // oct-search
            Self::Palette => "\u{f4b5}",      // oct-command_palette
            Self::Help => "\u{f420}",         // oct-question
        }
    }

    /// One cell wide, matching the glyph, so nothing reflows.
    ///
    /// These are not mnemonic in isolation and are not meant to be: the row
    /// they sit in, and the word beside them in a detail view, carry the
    /// meaning. What they must do is keep the columns aligned.
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
            Self::Security | Self::Warning => "!",
            Self::Repo => "#",
            Self::Private => "P",
            Self::Discussion => "D",
            Self::Release => "v",
            Self::Commit => "^",
            Self::Workflow => "W",
            Self::Unread => "*",
            Self::Actor => "&",
            Self::Age | Self::Stale => "t",
            Self::Refreshing => "%",
            Self::Offline => "0",
            Self::RateLimited => "T",
            Self::Forbidden => "/",
            Self::Error => "X",
            Self::Info => "i",
            Self::CaughtUp => "=",
            Self::Filter => "F",
            Self::Search => "?",
            Self::Palette => ":",
            Self::Help => "h",
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
    fn every_role_that_encodes_meaning_admits_it_needs_a_glyph() {
        // The roles that say "this one is different" are exactly the ones a
        // monochrome theme flattens, so each must be paired with a glyph at
        // its call sites. Muted and Accent are decoration, not encoding.
        for r in [
            Role::Success,
            Role::Warning,
            Role::Danger,
            Role::Added,
            Role::Removed,
        ] {
            assert!(r.pairs_with_a_glyph(), "{r:?}");
        }
        for r in [Role::Default, Role::Muted, Role::Accent] {
            assert!(!r.pairs_with_a_glyph(), "{r:?}");
        }
    }

    #[test]
    fn the_sixteen_ansi_colours_are_sufficient() {
        // Truecolor is an enhancement, never a requirement: no role may
        // resolve to an Rgb or an indexed colour beyond the base sixteen.
        for r in Role::ALL {
            if let Some(c) = r.style().fg {
                assert!(
                    !matches!(c, Color::Rgb(..)),
                    "{r:?} must not name a truecolor value"
                );
                if let Color::Indexed(i) = c {
                    assert!(i < 16, "{r:?} uses colour {i}, outside the ANSI sixteen");
                }
            }
        }
    }

    #[test]
    fn every_icon_has_an_ascii_fallback_of_equal_width() {
        for i in Icon::ALL {
            assert_eq!(i.ascii().chars().count(), 1, "{i:?} ascii must be one cell");
            assert_eq!(i.glyph().chars().count(), 1, "{i:?} glyph must be one cell");
            assert!(
                i.ascii().is_ascii(),
                "{i:?} fallback must be renderable without a Nerd Font"
            );
        }
    }

    #[test]
    fn the_icon_table_is_complete_and_octicon_ranged() {
        assert_eq!(
            Icon::ALL.len(),
            std::collections::BTreeSet::from(Icon::ALL).len(),
            "Icon::ALL must not repeat a role"
        );
        for i in Icon::ALL {
            let c = i.glyph().chars().next().unwrap() as u32;
            assert!(
                (0xF400..=0xF533).contains(&c),
                "{i:?} is U+{c:04X}, outside the Nerd Font Octicon range"
            );
        }
    }

    #[test]
    fn both_icon_modes_produce_the_same_layout() {
        // The whole point of the fallback: a row must not reflow when the
        // mode changes.
        for i in Icon::ALL {
            assert_eq!(
                Icons::UNICODE.get(i).chars().count(),
                Icons::ASCII.get(i).chars().count(),
                "{i:?} changes width between modes"
            );
        }
        for f in 0..8 {
            assert_eq!(
                Icons::UNICODE.spinner(f).chars().count(),
                Icons::ASCII.spinner(f).chars().count()
            );
        }
        for (u, a) in [
            (Icons::UNICODE.ellipsis(), Icons::ASCII.ellipsis()),
            (Icons::UNICODE.dot(), Icons::ASCII.dot()),
            (Icons::UNICODE.cursor(), Icons::ASCII.cursor()),
            (Icons::UNICODE.skeleton(), Icons::ASCII.skeleton()),
        ] {
            assert_eq!(u.chars().count(), a.chars().count());
            assert_eq!(a.chars().count(), 1);
        }
    }

    #[test]
    fn the_spinner_cycles_rather_than_panicking_on_a_large_frame() {
        assert_eq!(Icons::UNICODE.spinner(0), Icons::UNICODE.spinner(4));
        assert_eq!(
            Icons::UNICODE.spinner(usize::MAX % 4),
            Icons::UNICODE.spinner(3)
        );
    }

    #[test]
    fn ascii_is_forced_where_a_nerd_font_cannot_exist() {
        // The kernel console has one font and it is not JetBrainsMono.
        assert_eq!(
            IconMode::resolve(None, Some("linux"), Some("en_GB.UTF-8")),
            IconMode::Ascii
        );
        assert_eq!(
            IconMode::resolve(None, Some("foot"), Some("C")),
            IconMode::Ascii,
            "a non-UTF-8 locale cannot carry the codepoints"
        );
        assert_eq!(
            IconMode::resolve(None, Some("foot"), Some("en_GB.UTF-8")),
            IconMode::Unicode
        );
        // Config wins outright, in both directions.
        assert_eq!(
            IconMode::resolve(Some("ascii"), Some("foot"), Some("en_GB.UTF-8")),
            IconMode::Ascii
        );
        assert_eq!(
            IconMode::resolve(Some("unicode"), Some("linux"), Some("C")),
            IconMode::Unicode
        );
        // An unrecognised value falls through to detection rather than
        // erroring at a call site that cannot report it.
        assert_eq!(
            IconMode::resolve(Some("emoji"), Some("linux"), None),
            IconMode::Ascii
        );
    }
}

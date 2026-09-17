//! The notifications inbox — and, for this PR, the comparator that decides
//! what it should finally look like.
//!
//! # Why this surface has seven switches in it
//!
//! `spec/30-ui.md` §9's information-design findings were measured against the
//! discarded QML prototype (`00-overview.md` §8): a centred card roughly 875
//! logical pixels wide, proportional text, one row per notification. They are
//! good findings about *GitHub's data* — repo names really do repeat fourteen
//! times, there really are twelve reasons and rarely more than one accent
//! colour — but the medium they were measured in is not this one. §4.1's
//! width breakpoints are explicitly marked reasoned rather than measured.
//!
//! So every contested answer is built here as a **runtime variant**, bound to
//! a key, with the current combination printed along the bottom of the
//! surface so that a screenshot describes itself. The defaults are §9's
//! answers; the alternatives are one keypress away. Once the owner has sat in
//! front of a real terminal and chosen, a later PR bakes the winners in and
//! deletes [`Variants`] entirely.
//!
//! | Key | Cycles | Question |
//! |---|---|---|
//! | `n` | glyph · text · none | is a reason worth a glyph, a word, or nothing? |
//! | `p` | full · owner-elided · hidden-when-shared | how much repo noise to remove |
//! | `i` | unicode · ascii | do Octicons earn their place |
//! | `w` | auto · narrow · wide | which column would you rather lose first |
//! | `t` | one-line · two-line | §4.1 says two-line was rejected; verify it |
//! | `m` | grey · hide · sink | what a row does when you mark it read |
//! | `g` | flat · by repository | the strongest candidate fix for repo noise |
//!
//! # What is not a variant
//!
//! Colour is never the only encoding (§7.1): every reason, state and marker
//! here carries a glyph as well, because Osaka Jade resolves `accent` and
//! `foreground` to the same `#cacccc`. Unread is a marker column *and*
//! weight. The cursor is a glyph *and* reverse video. None of that is
//! switchable, because none of it is in doubt.

use crate::{
    keys::Binding,
    surface::{Ctx, Outcome, Surface},
    theme::{Icon, IconMode, Icons, Role},
    widgets::chrome::Freshness,
    widgets::{
        Conditions, EmptyCopy, StateView, SurfaceState, Toast, classify,
        list::{self, Cell, Column, Columns, Row},
    },
};
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use omaghy_model::{
    CheckRollup, IssueDisplayStatus, Notification, NotificationId, NotificationReason,
    PrDisplayStatus, Result, RollupState, StoreError, SubjectId, SubjectKind, SubjectStatus, age,
};
use omaghy_store::{Fresh, NotificationQuery, Page, ReadFilter, RefreshTarget, StoreEvent};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use time::OffsetDateTime;

// ---------------------------------------------------------------- bindings

/// Every binding, including every variant key, so that help and the palette
/// are generated rather than written (`spec/30-ui.md` §5).
///
/// The keys avoid everything `app.rs` claims globally — `1`–`7`, `q`, `Esc`,
/// `?`, `:`, `r`, `o` — because a surface binding on one of those would never
/// be delivered.
///
/// **Ordered for the footer, which cannot hold all of them.** `app.rs` puts
/// every entry of `keymap()` into the footer and drops whole hints off the
/// end until they fit, so a surface with fifteen bindings pushes the shell's
/// own `? help` and `q quit` off a hundred-column terminal. The order below
/// puts what a reader needs first; the readout line at the bottom of this
/// surface carries the escape hatch until that is fixed — see the PR's
/// contract-change request.
const BINDINGS: &[Binding] = &[
    Binding::new("notification.next", "j / ↓", "next").on(KeyCode::Char('j')),
    Binding::new("notification.prev", "k / ↑", "previous").on(KeyCode::Char('k')),
    Binding::new("notification.toggle-read", "Enter", "toggle read").on(KeyCode::Enter),
    Binding::new("notification.unread-only", "u", "unread only").on(KeyCode::Char('u')),
    Binding::new("notification.filter-repo", "/", "this repo").on(KeyCode::Char('/')),
    Binding::new("notification.page-down", "Ctrl-d", "half page down").on_ctrl(KeyCode::Char('d')),
    Binding::new("notification.page-up", "Ctrl-u", "half page up").on_ctrl(KeyCode::Char('u')),
    Binding::new("notification.last", "G", "last").on(KeyCode::Char('G')),
];

// ---------------------------------------------------------------- variants

/// `n` — twelve reasons, and roughly one accent colour to spend on them.
///
/// In [`ReasonMode::Glyph`] the reason takes the leading icon column and the
/// subject's own state takes the state column; in [`ReasonMode::Text`] they
/// swap, so the comparison is like-for-like rather than one mode simply
/// having more on screen. In [`ReasonMode::Hidden`] the state column is
/// *dropped*, not blanked, so the title actually gets the eight cells back —
/// otherwise "no reason" would look worse than it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasonMode {
    #[default]
    Glyph,
    Text,
    Hidden,
}

/// `p` — `ShaxP/shax` fourteen times in a column is measured noise (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepoMode {
    /// `owner/name`, always, even when every row says the same thing.
    #[default]
    Full,
    /// Drop `owner/` when it is the viewer's own.
    OwnerElided,
    /// Also drop the column entirely when every visible row shares one
    /// repository, naming it in the header instead. §9's full stack.
    HiddenWhenShared,
}

/// `w` — which column would you rather lose first?
///
/// Forcing a width rather than resizing the terminal: the rows are drawn into
/// a sub-rectangle of the given width, so the dropped columns are exactly the
/// ones §4.1's table drops, without the reviewer having to keep resizing a
/// window to find out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WidthMode {
    #[default]
    Auto,
    Narrow,
    Wide,
}

impl WidthMode {
    /// The two forced widths sit inside the `60..80` and `≥120` tiers of
    /// §4.1, which is where the interesting columns disappear.
    const NARROW: u16 = 70;
    const WIDE: u16 = 120;

    /// The value as `40-config.md` §2 names it. Public because the
    /// settings surface (§6) shows the current value of each setting.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Narrow => "narrow",
            Self::Wide => "wide",
        }
    }

    /// Never wider than the terminal: a row one cell too wide wraps, and a
    /// wrapped row silently halves the list.
    fn resolve(self, available: u16) -> u16 {
        match self {
            Self::Auto => available,
            Self::Narrow => available.min(Self::NARROW),
            Self::Wide => available.min(Self::WIDE),
        }
    }
}

/// `t` — `RowList` is single-line; the two-line mode below is built here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowMode {
    OneLine,
    #[default]
    TwoLine,
}

impl RowMode {
    fn height(self) -> usize {
        match self {
            Self::OneLine => 1,
            Self::TwoLine => 2,
        }
    }
}

/// `m` — what a row does the moment you mark it read.
///
/// The whole question is whether bulk triage feels pleasant or error-prone,
/// and that cannot be judged from a still image: it is about where the cursor
/// ends up and whether the list moves underneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TriageMode {
    /// §9's answer: the row stays exactly where it is, greyed.
    #[default]
    Grey,
    /// The row leaves the list, and the cursor lands on the next one.
    Hide,
    /// The row drops below every unread row, keeping its content reachable.
    Sink,
}

/// `g` — the strongest candidate fix for repeated repository names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupMode {
    Flat,
    #[default]
    ByRepo,
}

/// `label` ↔ value, plus the accepted list, for every setting whose value is
/// one of a fixed set of words.
///
/// Generated rather than written out five times because three things have to
/// agree and must not drift: `40-config.md` §2's vocabulary, what the settings
/// surface (§6) offers, and what the config reader accepts. They had already
/// drifted before anything read them — `rows` answered `2-line` where §2 says
/// `two-line`, and `repo` answered `owner` and `shared` for `elide-owner` and
/// `hide-when-shared`. Nothing noticed, because no reader existed to disagree.
macro_rules! labelled {
    ($t:ty { $($v:ident => $s:literal),+ $(,)? }) => {
        impl $t {
            /// Every value, in the order the settings surface cycles them.
            pub const ALL: &'static [Self] = &[$(Self::$v),+];

            /// The value as `40-config.md` §2 names it.
            pub fn label(self) -> &'static str {
                match self {
                    $(Self::$v => $s),+
                }
            }

            /// Parse §2's vocabulary. `None` means "not one of ours", which
            /// the caller reports alongside [`Self::labels`].
            pub fn from_label(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.label() == s)
            }

            /// The accepted values, for a message that names them.
            pub fn labels() -> Vec<&'static str> {
                Self::ALL.iter().map(|v| v.label()).collect()
            }
        }
    };
}

labelled!(ReasonMode { Glyph => "glyph", Text => "text", Hidden => "none" });
labelled!(RepoMode {
    Full => "full",
    OwnerElided => "elide-owner",
    HiddenWhenShared => "hide-when-shared",
});
labelled!(RowMode { TwoLine => "two-line", OneLine => "one-line" });
labelled!(TriageMode { Grey => "grey", Sink => "sink", Hide => "hide" });
labelled!(GroupMode { ByRepo => "by-repo", Flat => "flat" });

/// The seven switches. `Default` is exactly `spec/30-ui.md` §9's hypothesis,
/// so the surface opens on what the spec currently claims and every keypress
/// is a deliberate departure from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Variants {
    pub reason: ReasonMode,
    pub repo: RepoMode,
    pub width: WidthMode,
    pub rows: RowMode,
    pub triage: TriageMode,
    pub group: GroupMode,
}

impl Variants {}

// ------------------------------------------------------------- the mapping

/// A glyph per reason. Twelve of them, each distinct in **both** icon modes —
/// an ASCII fallback that collides with another reason's would make the
/// no-Nerd-Font terminal strictly worse than no glyph at all. The letter in
/// each comment is that fallback.
fn reason_icon(reason: &NotificationReason) -> Icon {
    use NotificationReason as R;
    match reason {
        R::ReviewRequested => Icon::Review, // R
        R::Mention => Icon::Mention,        // @
        R::TeamMention => Icon::Discussion, // D
        R::Assign => Icon::Actor,           // &
        R::Author => Icon::Commit,          // ^
        R::Comment => Icon::Comment,        // c
        R::StateChange => Icon::Refreshing, // %
        R::CiActivity => Icon::Workflow,    // W
        R::Subscribed => Icon::Repo,        // #
        R::Manual => Icon::Private,         // P
        R::Invitation => Icon::Help,        // h
        R::SecurityAlert => Icon::Security, // !
        R::Other(_) => Icon::Info,          // i
    }
}

/// Six cells is all `Column::State` leaves after its glyph and separator, so
/// these are abbreviations rather than §9's `label()`. That constraint is
/// itself evidence in the `n` comparison, and the focused-row line below
/// always prints the full word — §9 requires it there anyway.
fn reason_short(reason: &NotificationReason) -> &str {
    use NotificationReason as R;
    match reason {
        R::ReviewRequested => "review",
        R::Mention => "@me",
        R::TeamMention => "@team",
        R::Assign => "assign",
        R::Author => "author",
        R::Comment => "reply",
        R::StateChange => "state",
        R::CiActivity => "ci",
        R::Subscribed => "subbed",
        R::Manual => "manual",
        R::Invitation => "invite",
        R::SecurityAlert => "sec",
        R::Other(s) => s,
    }
}

/// Colour is a hint on top of the glyph, never the encoding: only the two
/// distinctions worth an accent get one.
fn reason_role(reason: &NotificationReason) -> Role {
    match reason {
        NotificationReason::SecurityAlert => Role::Danger,
        r if r.is_directed_at_me() => Role::Accent,
        _ => Role::Muted,
    }
}

fn kind_icon(kind: &SubjectKind) -> Icon {
    match kind {
        SubjectKind::PullRequest => Icon::PrOpen,
        SubjectKind::Issue => Icon::IssueOpen,
        SubjectKind::Discussion => Icon::Discussion,
        SubjectKind::Release => Icon::Release,
        SubjectKind::CheckSuite => Icon::Workflow,
        SubjectKind::Commit => Icon::Commit,
        SubjectKind::VulnerabilityAlert => Icon::Security,
        SubjectKind::Other(_) => Icon::Comment,
    }
}

fn kind_short(kind: &SubjectKind) -> &str {
    match kind {
        SubjectKind::PullRequest => "pr",
        SubjectKind::Issue => "issue",
        SubjectKind::Discussion => "disc",
        SubjectKind::Release => "rel",
        SubjectKind::CheckSuite => "checks",
        SubjectKind::Commit => "commit",
        SubjectKind::VulnerabilityAlert => "alert",
        SubjectKind::Other(s) => s,
    }
}

fn kind_long(kind: &SubjectKind) -> &str {
    match kind {
        SubjectKind::PullRequest => "pull request",
        SubjectKind::Issue => "issue",
        SubjectKind::Discussion => "discussion",
        SubjectKind::Release => "release",
        SubjectKind::CheckSuite => "check suite",
        SubjectKind::Commit => "commit",
        SubjectKind::VulnerabilityAlert => "vulnerability alert",
        SubjectKind::Other(s) => s,
    }
}

/// `None` for a subject that has no state — a commit, a release, a check
/// suite. The caller falls back to the subject kind, which is what a row for
/// one of those should say.
fn status_cell(status: &SubjectStatus) -> Option<Cell> {
    let (icon, label, role) = match status {
        SubjectStatus::PullRequest(s) => {
            let (icon, role) = match s {
                PrDisplayStatus::Draft => (Icon::PrDraft, Role::Muted),
                PrDisplayStatus::Open => (Icon::PrOpen, Role::Success),
                PrDisplayStatus::Merged => (Icon::PrMerged, Role::Accent),
                PrDisplayStatus::Closed => (Icon::PrClosed, Role::Danger),
            };
            (icon, s.label(), role)
        }
        SubjectStatus::Issue(s) => {
            let (icon, role) = match s {
                IssueDisplayStatus::Open => (Icon::IssueOpen, Role::Success),
                IssueDisplayStatus::Completed => (Icon::IssueClosed, Role::Accent),
                // Closed-as-not-planned is a closure that is not a completion,
                // and reading it as success would be a lie.
                IssueDisplayStatus::NotPlanned | IssueDisplayStatus::Duplicate => {
                    (Icon::IssueClosed, Role::Muted)
                }
            };
            (icon, s.label(), role)
        }
        SubjectStatus::None => return None,
    };
    Some(Cell::new(icon, label, role))
}

/// `None` where there is no CI at all, which is different from everything
/// having been skipped.
fn checks_cell(rollup: &CheckRollup) -> Option<Cell> {
    let total = rollup.passed + rollup.failed + rollup.pending + rollup.skipped;

    // A count of zero means the individual runs were not fetched, not that
    // nothing ran — the enrichment query asks for the rollup state and not the
    // run list. Rendering "0" beside a green tick says something false, so the
    // glyph stands alone until there is a number worth showing.
    let count = |n: u16| if n == 0 { String::new() } else { n.to_string() };

    match rollup.state {
        RollupState::None => None,
        RollupState::Success => Some(Cell::new(
            Icon::CheckPass,
            count(rollup.passed),
            Role::Success,
        )),
        RollupState::Failure => Some(Cell::new(
            Icon::CheckFail,
            if total == 0 {
                String::new()
            } else {
                format!("{}/{total}", rollup.failed)
            },
            Role::Danger,
        )),
        RollupState::Pending => Some(Cell::new(
            Icon::CheckPending,
            count(rollup.pending),
            Role::Warning,
        )),
        RollupState::Neutral => Some(Cell::new(Icon::CheckPending, count(total), Role::Muted)),
    }
}

fn number_of(n: &Notification) -> Option<u64> {
    match n.subject.as_ref().map(|s| &s.id) {
        Some(SubjectId::Number(num)) => Some(*num),
        _ => None,
    }
}

// ------------------------------------------------------------ display model

/// One line of the list. Headers exist only under [`GroupMode::ByRepo`], and
/// the cursor never lands on one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Header {
        label: String,
        count: usize,
    },
    /// An index into the loaded page.
    Row(usize),
}

// -------------------------------------------------------------- the surface

/// The inbox.
#[derive(Debug, Default)]
pub struct Notifications {
    page: Option<Fresh<Page<Notification>>>,
    query: NotificationQuery,
    /// What the last read failed with. Held rather than propagated: an error
    /// is a designed screen (§8), not a reason to tear down the app.
    error: Option<StoreError>,
    /// Index into the *displayed* row order, not into the page.
    cursor: usize,
    list: ListState,
    /// Triage that has not yet been flushed to the store. The rows are
    /// already flipped locally — the store owns the rollback (`20-store.md`
    /// §5), so a failure comes back as a toast and the next read corrects us.
    pending: Vec<(NotificationId, bool)>,
    toast: Option<Toast>,
    /// Rows the last frame had room for, so `Ctrl-d` moves by a real page.
    viewport: u16,
    pub variants: Variants,
}

impl Notifications {
    /// How to draw (`40-config.md` §2 `[notifications]`).
    #[must_use]
    pub fn with_variants(mut self, variants: Variants) -> Self {
        self.variants = variants;
        self
    }

    pub fn new() -> Self {
        Self::default()
    }

    fn items(&self) -> &[Notification] {
        self.page
            .as_ref()
            .map(|p| p.value.items.as_slice())
            .unwrap_or_default()
    }

    /// Always Unicode where the terminal can draw it; the ASCII forms are a
    /// capability fallback, never a preference (`40-config.md` §3). Resolution
    /// moves to `Ctx` once it carries one — filed as a contract change.
    fn icons(&self) -> Icons {
        Icons::new(IconMode::Unicode)
    }

    // ---- what is on screen, and in what order ---------------------------

    /// The page, after triage reordering. Filtering the *store* can do lives
    /// in [`NotificationQuery`]; this is only the part that is a view
    /// decision the owner is being asked to make.
    fn ordered(&self) -> Vec<usize> {
        let items = self.items();
        let all = 0..items.len();
        match self.variants.triage {
            TriageMode::Grey => all.collect(),
            TriageMode::Hide => all.filter(|&i| items[i].unread).collect(),
            TriageMode::Sink => {
                let (unread, read): (Vec<usize>, Vec<usize>) = all.partition(|&i| items[i].unread);
                unread.into_iter().chain(read).collect()
            }
        }
    }

    /// The display list: rows, plus section headers when grouping.
    ///
    /// Groups appear in the order their newest row does, so the list is still
    /// sorted by recency at the group level and the top of the screen is
    /// still the thing that happened last.
    fn entries(&self) -> Vec<Entry> {
        let ordered = self.ordered();
        if self.variants.group == GroupMode::Flat {
            return ordered.into_iter().map(Entry::Row).collect();
        }
        let items = self.items();
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for i in ordered {
            let repo = items[i].repo.to_string();
            match groups.iter_mut().find(|(name, _)| *name == repo) {
                Some((_, rows)) => rows.push(i),
                None => groups.push((repo, vec![i])),
            }
        }
        let mut out = Vec::new();
        for (label, rows) in groups {
            out.push(Entry::Header {
                label,
                count: rows.len(),
            });
            out.extend(rows.into_iter().map(Entry::Row));
        }
        out
    }

    /// The page index the cursor is on.
    fn focused(&self) -> Option<usize> {
        self.display_order().get(self.cursor).copied()
    }

    /// Row indices in the order they are drawn — the order the cursor walks.
    fn display_order(&self) -> Vec<usize> {
        self.entries()
            .into_iter()
            .filter_map(|e| match e {
                Entry::Row(i) => Some(i),
                Entry::Header { .. } => None,
            })
            .collect()
    }

    // ---- rows -----------------------------------------------------------

    fn row_of(&self, n: &Notification, now: OffsetDateTime) -> Row {
        let (icon, state) = match self.variants.reason {
            // The reason leads; the subject's own state keeps the state column.
            ReasonMode::Glyph => (
                reason_icon(&n.reason),
                Some(
                    n.detail
                        .ready()
                        .and_then(|d| status_cell(&d.status))
                        .unwrap_or_else(|| {
                            Cell::new(kind_icon(&n.kind), kind_short(&n.kind), Role::Muted)
                        }),
                ),
            ),
            // They swap, so the two modes carry the same amount of screen.
            ReasonMode::Text => (
                kind_icon(&n.kind),
                Some(Cell::new(
                    reason_icon(&n.reason),
                    reason_short(&n.reason),
                    reason_role(&n.reason),
                )),
            ),
            ReasonMode::Hidden => (kind_icon(&n.kind), None),
        };

        let mut row = Row::new(n.title.clone(), age::relative(n.updated_at, now))
            .icon(icon)
            .unread(n.unread)
            .repo(n.repo.to_string());
        if let Some(cell) = state {
            row = row.state(cell);
        }
        if let Some(num) = number_of(n) {
            row = row.number(num);
        }
        if let Some(d) = n.detail.ready() {
            if let Some(cell) = checks_cell(&d.checks) {
                row = row.checks(cell);
            }
            if let Some(actor) = &d.last_actor {
                row = row.actor(actor.login.clone());
            }
        }
        row
    }

    /// The repository a row should print, given `p`.
    fn repo_text(&self, n: &Notification, viewer: &str) -> String {
        let full = n.repo.to_string();
        match self.variants.repo {
            RepoMode::Full => full,
            RepoMode::OwnerElided | RepoMode::HiddenWhenShared => {
                list::elide_owner(&full, Some(viewer)).to_owned()
            }
        }
    }

    /// The one repository every visible row shares, if `p` says to elide it.
    ///
    /// A single row is not a shared repository: eliding the only name on
    /// screen costs the reader the name and tells them nothing.
    fn elided_repo(&self) -> Option<String> {
        if self.variants.repo != RepoMode::HiddenWhenShared {
            return None;
        }
        let items = self.items();
        let ordered = self.ordered();
        if ordered.len() < 2 {
            return None;
        }
        let first = items[ordered[0]].repo.to_string();
        ordered
            .iter()
            .all(|&i| items[i].repo.to_string() == first)
            .then_some(first)
    }

    /// The columns this frame draws.
    ///
    /// Three drops on top of §4.1's table, each for the same reason the spec
    /// drops a shared repository: a column every row leaves blank is worse
    /// than no column, because the title paid for it.
    fn columns(&self, width: u16, rows: &[Row], repo_is_redundant: bool) -> Columns {
        let mut cols = Columns::for_width(width);
        if !rows.iter().any(|r| r.checks.is_some()) {
            cols = cols.without(Column::Checks);
        }
        if !rows.iter().any(|r| r.actor.is_some()) {
            cols = cols.without(Column::Actor);
        }
        if self.variants.reason == ReasonMode::Hidden {
            cols = cols.without(Column::State);
        }
        if repo_is_redundant {
            cols = cols.without(Column::Repo);
        }
        cols
    }

    // ---- cursor ---------------------------------------------------------

    fn move_cursor(&mut self, delta: isize) {
        let n = self.display_order().len();
        if n == 0 {
            self.cursor = 0;
            return;
        }
        let next = (self.cursor as isize)
            .saturating_add(delta)
            .clamp(0, n as isize - 1);
        self.cursor = next as usize;
    }

    fn clamp_cursor(&mut self) {
        let n = self.display_order().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    fn half_page(&self) -> isize {
        let rows = (self.viewport as usize / self.variants.rows.height()).max(1);
        (rows / 2).max(1) as isize
    }

    // ---- state ----------------------------------------------------------

    /// What the filter line should call the current narrowing, or `None` when
    /// nothing is narrowed. This is what separates "filtered to empty" from
    /// "empty", so it must be honest about every narrowing in force.
    fn filter_label(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(repo) = &self.query.repo {
            parts.push(repo.to_string());
        }
        if self.query.read == ReadFilter::UnreadOnly {
            parts.push("unread only".to_owned());
        }
        if self.variants.triage == TriageMode::Hide {
            parts.push("read hidden".to_owned());
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// The key that undoes the narrowing, named so it is one the user has.
    fn clear_filter_hint(&self) -> (&'static str, &'static str) {
        if self.query.repo.is_some() {
            ("/", "clear the repository filter")
        } else if self.query.read == ReadFilter::UnreadOnly {
            ("u", "show read ones too")
        } else {
            ("m", "stop hiding read rows")
        }
    }

    fn state(&self, filter: Option<&str>) -> SurfaceState {
        let visible = self.display_order().len();
        let conditions = match &self.page {
            Some(page) => Conditions::from_fresh(page, visible),
            None => Conditions {
                fetched: false,
                items: visible,
                ..Default::default()
            },
        };
        classify(&conditions.filter(filter).error(self.error.as_ref()))
    }

    // ---- mutation -------------------------------------------------------

    /// Flip the focused row now and queue the write.
    ///
    /// Optimistic on purpose: a 700ms wait on a keypress feels broken
    /// (`20-store.md` §5). The store owns the rollback; the next read
    /// corrects whatever it decided.
    fn toggle_read(&mut self) -> Outcome {
        let Some(index) = self.focused() else {
            return Outcome::Ignored;
        };
        let Some(page) = self.page.as_mut() else {
            return Outcome::Ignored;
        };
        let row = &mut page.value.items[index];
        row.unread = !row.unread;
        let (id, unread) = (row.id.clone(), row.unread);
        self.pending.push((id, unread));
        // Under `hide` and `sink` the row has just left this position, so the
        // index the cursor holds now names the next row — which is what makes
        // a clean sweep a run of `Enter`. Under `grey` it names the same row,
        // which is what makes an accidental press undoable by pressing again.
        self.clamp_cursor();
        Outcome::Redraw
    }

    /// Send queued triage, then report what happened.
    async fn flush_pending(&mut self, ctx: &Ctx) {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return;
        }
        let read: Vec<NotificationId> = pending
            .iter()
            .filter(|(_, unread)| !unread)
            .map(|(id, _)| id.clone())
            .collect();
        let unread: Vec<NotificationId> = pending
            .iter()
            .filter(|(_, unread)| *unread)
            .map(|(id, _)| id.clone())
            .collect();

        let mut failure = None;
        if !read.is_empty()
            && let Err(e) = ctx.store.mark_read(&read).await
        {
            failure = Some(e);
        }
        if !unread.is_empty()
            && let Err(e) = ctx.store.mark_unread(&unread).await
        {
            failure = Some(e);
        }
        self.toast = Some(match failure {
            Some(e) => Toast::from_error(&e),
            None => Toast::success(match (read.len(), unread.len()) {
                (n, 0) => format!("Marked {n} read"),
                (0, n) => format!("Marked {n} unread"),
                (r, u) => format!("Marked {r} read, {u} unread"),
            }),
        });
    }

    // ---- rendering ------------------------------------------------------

    fn render_rows(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let width = self.variants.width.resolve(area.width);
        let area = Rect { width, ..area };
        let viewer = ctx.viewer().login.clone();
        let icons = self.icons();
        let items = self.items();
        let entries = self.entries();
        let grouped = self.variants.group == GroupMode::ByRepo;
        let shared = self.elided_repo().is_some();

        let rows: Vec<Row> = entries
            .iter()
            .filter_map(|e| match e {
                Entry::Row(i) => Some(self.row_of(&items[*i], ctx.now)),
                Entry::Header { .. } => None,
            })
            .collect();
        let cols = self.columns(width, &rows, grouped || shared);

        // Which entry the cursor is on: headers are skipped, so the cursor
        // index counts rows and this maps it back into the display list.
        let selected = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, Entry::Row(_)))
            .map(|(pos, _)| pos)
            .nth(self.cursor);

        let mut list_items: Vec<ListItem> = Vec::with_capacity(entries.len());
        let mut built = rows.iter();
        for (pos, entry) in entries.iter().enumerate() {
            match entry {
                Entry::Header { label, count } => {
                    list_items.push(ListItem::new(self.header_line(label, *count, width)));
                }
                Entry::Row(index) => {
                    let row = built.next().expect("one built row per Entry::Row");
                    let cursor = selected == Some(pos);
                    let text = self.repo_text(&items[*index], &viewer);
                    let repo = (!shared && !grouped).then_some(text.as_str());
                    list_items.push(match self.variants.rows {
                        RowMode::OneLine => {
                            ListItem::new(list::row_line(row, repo, &cols, icons, cursor))
                        }
                        RowMode::TwoLine => {
                            ListItem::new(self.two_line(&items[*index], row, repo, width, cursor))
                        }
                    });
                }
            }
        }

        self.list.select(selected);
        f.render_stateful_widget(
            List::new(list_items).highlight_style(Role::Selected.style()),
            area,
            &mut self.list,
        );
    }

    /// A section header under [`GroupMode::ByRepo`]. Its prefix is the width
    /// of the rows' gutter, so it reads as a heading rather than as a row.
    fn header_line(&self, label: &str, count: usize, width: u16) -> Line<'static> {
        let icons = self.icons();
        let suffix = format!(" ({count})");
        let room = (width as usize).saturating_sub(4 + suffix.len());
        Line::from(vec![
            Span::raw("  "),
            Span::styled(icons.get(Icon::Repo).to_owned(), Role::Accent.style()),
            Span::raw(" "),
            Span::styled(
                list::elide(label, room, icons),
                Role::Accent.style().add_modifier(Modifier::BOLD),
            ),
            Span::styled(suffix, Role::Muted.style()),
        ])
    }

    /// The two-line row `t` exists to compare against.
    ///
    /// `RowList` is single-line by construction, so this is built here from
    /// the same primitives — the gutter is still cursor glyph, unread marker,
    /// space; the age is still right-aligned and never dropped; widths are
    /// still measured in cells rather than characters.
    fn two_line(
        &self,
        n: &Notification,
        row: &Row,
        repo: Option<&str>,
        width: u16,
        cursor: bool,
    ) -> Vec<Line<'static>> {
        let icons = self.icons();
        let w = width as usize;
        // gutter(3) + icon(1) + space(1) + title + space(1) + age(4)
        let title_w = w.saturating_sub(10);

        let first = Line::from(vec![
            Span::styled(
                if cursor { icons.cursor() } else { " " },
                Role::Accent.style(),
            ),
            Span::styled(
                if row.unread {
                    icons.get(Icon::Unread)
                } else {
                    " "
                },
                Role::Accent.style(),
            ),
            Span::raw(" "),
            Span::styled(
                row.icon.map_or(" ", |i| icons.get(i)).to_owned(),
                Role::Accent.style(),
            ),
            Span::raw(" "),
            Span::styled(
                pad(&list::elide(&row.title, title_w, icons), title_w),
                if row.unread {
                    Role::Unread.style()
                } else {
                    Role::Default.style()
                },
            ),
            Span::raw(" "),
            Span::styled(format!("{:>4}", row.age), Role::Muted.style()),
        ]);

        // The second line is where a two-line layout earns its keep or does
        // not: it can afford the *full* reason word, which no column can.
        let mut parts: Vec<String> = Vec::new();
        if let Some(repo) = repo {
            parts.push(repo.to_owned());
        }
        parts.push(n.reason.label().to_owned());
        parts.push(match number_of(n) {
            Some(num) => format!("{} #{num}", kind_long(&n.kind)),
            None => kind_long(&n.kind).to_owned(),
        });
        // State and checks belong here too. One-line mode carries them as
        // columns; this line was built from repo, reason and number alone, so
        // enrichment was fetched, cached, and then discarded at render time —
        // a merged pull request read exactly like an open one.
        // Only a real state. The state column falls back to the subject kind
        // when there is no detail, and this line has already named the kind in
        // full — "check suite · checks" says one thing twice.
        if n.detail.ready().is_some()
            && let Some(state) = &row.state
        {
            parts.push(state.text.clone());
        }
        if let Some(checks) = &row.checks
            && !checks.text.is_empty()
        {
            parts.push(format!("checks {}", checks.text));
        }
        let secondary = parts.join(&format!(" {} ", icons.dot()));

        let second = Line::from(vec![
            Span::raw("     "),
            Span::styled(
                list::elide(&secondary, w.saturating_sub(5), icons),
                Role::Muted.style(),
            ),
        ]);
        vec![first, second]
    }

    /// The line above the readout: what the cursor is on, or — when there are
    /// no rows — what the screen currently is.
    ///
    /// §9 asks for the reason's full word "in the detail view and on the
    /// focused row"; no column is wide enough for `review requested`, so this
    /// is where it lives.
    fn render_detail(&self, f: &mut Frame, area: Rect, state: &SurfaceState, now: OffsetDateTime) {
        if area.height == 0 {
            return;
        }
        let icons = self.icons();
        let left = match (state.has_content(), self.focused()) {
            (true, Some(index)) => {
                let n = &self.items()[index];
                let mut parts = vec![
                    n.reason.label().to_owned(),
                    match number_of(n) {
                        Some(num) => format!("{} #{num}", kind_long(&n.kind)),
                        None => kind_long(&n.kind).to_owned(),
                    },
                    n.repo.to_string(),
                    age::relative(n.updated_at, now),
                ];
                if !n.detail.is_settled() {
                    parts.push("awaiting details".to_owned());
                }
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(icons.cursor().to_owned(), Role::Accent.style()),
                    Span::raw(" "),
                    Span::styled(
                        parts.join(&format!(" {} ", icons.dot())),
                        Role::Muted.style(),
                    ),
                ])
            }
            _ => {
                let (headline, detail) = state_summary(state);
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(headline, Role::Default.style().add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" {} ", icons.dot()), Role::Muted.style()),
                    Span::styled(detail, Role::Muted.style()),
                ])
            }
        };
        // A toast shares this line with the freshness note: both are about
        // the data rather than about the layout, and the readout below is
        // about nothing else.
        let mut right = match &self.toast {
            Some(t) => {
                let mut spans = t.spans(icons, 0);
                spans.push(Span::raw("  "));
                spans
            }
            None => Vec::new(),
        };
        right.extend(self.freshness_spans());
        split_line(f, area, left, Line::from(right));
    }

    /// Where the data came from. The header would normally carry this, but
    /// the shell's header takes no `Fresh`, so the surface says it here —
    /// stale data is never hidden (§8).
    fn freshness_spans(&self) -> Vec<Span<'static>> {
        let icons = self.icons();
        let Some(page) = &self.page else {
            return Vec::new();
        };
        let (icon, text, role) = if page.refreshing {
            (Icon::Refreshing, "refreshing", Role::Accent)
        } else if page.fetched_at.is_none() {
            (Icon::Offline, "not fetched", Role::Warning)
        } else if page.needs_provenance_note() {
            (Icon::Stale, "cached", Role::Warning)
        } else {
            return Vec::new();
        };
        vec![
            Span::styled(icons.get(icon).to_owned(), role.style()),
            Span::raw(" "),
            Span::styled(text.to_owned(), role.style()),
            Span::raw(" "),
        ]
    }
}

/// Split rather than overlaid: two paragraphs on one area collide silently,
/// and it is always the left half that loses.
fn split_line(f: &mut Frame, area: Rect, left: Line<'static>, right: Line<'static>) {
    let area = Rect { height: 1, ..area };
    let rw = right
        .spans
        .iter()
        .map(|s| list::cells(s.content.as_ref()) as u16)
        .sum::<u16>();
    // One cell of air, so a truncated left half does not read as one
    // sentence running into the right half.
    let rw = if rw > 0 { rw + 1 } else { 0 };
    let split = area.width.saturating_sub(rw);
    f.render_widget(
        Paragraph::new(left),
        Rect {
            width: split,
            ..area
        },
    );
    if rw > 0 {
        f.render_widget(
            Paragraph::new(right.alignment(Alignment::Right)),
            Rect {
                x: area.x + split,
                width: area.width - split,
                ..area
            },
        );
    }
}

fn pad(s: &str, width: usize) -> String {
    let mut out = s.to_owned();
    out.extend(std::iter::repeat_n(
        ' ',
        width.saturating_sub(list::cells(s)),
    ));
    out
}

/// One line naming a state that has no rows, for the detail line.
///
/// `StateView` draws the full centred block; this is the terse restatement
/// beside the readout, so a screenshot of an error screen still says which
/// error it is.
fn state_summary(state: &SurfaceState) -> (String, String) {
    match state {
        SurfaceState::Cold => (
            "No data yet".into(),
            "nothing cached, and no fetch has completed".into(),
        ),
        SurfaceState::Empty => (
            "All caught up".into(),
            "nothing needs your attention".into(),
        ),
        SurfaceState::FilteredEmpty { filter } => {
            ("No matches".into(), format!("nothing matches {filter}"))
        }
        SurfaceState::OfflineWithoutCache => {
            ("Offline".into(), "and nothing has ever been cached".into())
        }
        SurfaceState::RateLimited { kind, .. } => (
            format!("GitHub {kind} reached"),
            "cached data is still readable".into(),
        ),
        SurfaceState::Forbidden => ("No access".into(), "this is not retried".into()),
        SurfaceState::Error { message } => ("Something failed".into(), message.clone()),
        // Content states never reach here — the focused row wins.
        SurfaceState::Populated
        | SurfaceState::Stale
        | SurfaceState::Refreshing
        | SurfaceState::OfflineWithCache => (state.name().to_owned(), String::new()),
    }
}

#[async_trait]
impl Surface for Notifications {
    /// The shell's header takes only a title, so the counts and the scope ride
    /// along in it. `·` is the header's own separator.
    fn title(&self) -> String {
        let Some(page) = &self.page else {
            return "Notifications".to_owned();
        };
        let unread = page.value.items.iter().filter(|n| n.unread).count();
        let mut title = format!(
            "Notifications  {unread} unread · {} total",
            page.value.len()
        );
        if let Some(repo) = self.elided_repo() {
            title.push_str(&format!(" · {repo}"));
        }
        if let Some(filter) = self.filter_label() {
            title.push_str(&format!(" · {filter}"));
        }
        title
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        // Two lines are spent on saying what this screen is: one on the
        // focused row, one on the variant combination. They are the price of
        // a screenshot that can be argued with, and they go with the switcher.
        // The escape line used to live here. `App`'s footer names the way
        // out now, so the row goes back to the list.
        let detail_h = u16::from(area.height >= 6);
        let body_h = area.height.saturating_sub(detail_h);
        let body = Rect {
            height: body_h,
            ..area
        };
        let detail = Rect {
            y: area.y + body_h,
            height: detail_h,
            ..area
        };

        self.viewport = body_h;
        let icons = self.icons();
        let filter = self.filter_label();
        let state = self.state(filter.as_deref());
        let (clear_key, clear_what) = self.clear_filter_hint();

        let view = StateView::new(&state)
            .icons(icons)
            .clear_filter(clear_key, clear_what)
            .empty(EmptyCopy {
                headline: "All caught up",
                detail: "No notification needs your attention.",
                action: Some(("r", "check for new ones")),
            });
        if let Some(rows) = view.render(f, body).rows() {
            self.render_rows(f, rows, ctx);
        }

        self.render_detail(f, detail, &state, ctx.now);
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Char('d'), true) => {
                let d = self.half_page();
                self.move_cursor(d);
                Outcome::Redraw
            }
            (KeyCode::Char('u'), true) => {
                let d = self.half_page();
                self.move_cursor(-d);
                Outcome::Redraw
            }
            (KeyCode::Char('j') | KeyCode::Down, false) => {
                self.move_cursor(1);
                Outcome::Redraw
            }
            (KeyCode::Char('k') | KeyCode::Up, false) => {
                self.move_cursor(-1);
                Outcome::Redraw
            }
            (KeyCode::Home, _) => {
                self.cursor = 0;
                Outcome::Redraw
            }
            (KeyCode::Char('G'), false) | (KeyCode::End, _) => {
                self.move_cursor(isize::MAX);
                Outcome::Redraw
            }
            (KeyCode::Enter, _) => self.toggle_read(),
            (KeyCode::Char('u'), false) => {
                self.query.read = match self.query.read {
                    ReadFilter::All => ReadFilter::UnreadOnly,
                    ReadFilter::UnreadOnly => ReadFilter::All,
                };
                self.cursor = 0;
                Outcome::Redraw
            }
            // `/` is §5's filter key. It scopes to a repository rather than
            // opening a text field — see the module docs and the PR: free
            // text cannot work until a surface can claim keys ahead of the
            // globals, which is a change to `app.rs` and not to this path.
            (KeyCode::Char('/'), false) => {
                self.query.repo = match self.query.repo {
                    Some(_) => None,
                    None => self.focused().map(|i| self.items()[i].repo.clone()),
                };
                self.cursor = 0;
                Outcome::Redraw
            }
            _ => Outcome::Ignored,
        }
    }

    /// Flush triage, then read.
    ///
    /// A read failure is **kept**, not propagated: every arm of §8 is a screen
    /// this surface is supposed to draw, and returning `Err` here would take
    /// down the whole app instead of drawing one of them. The previous page
    /// is kept too, which is what makes "offline, with cache" possible at all.
    async fn load(&mut self, ctx: &Ctx) -> Result<()> {
        self.flush_pending(ctx).await;
        match ctx.store.notifications(&self.query).await {
            Ok(page) => {
                self.error = None;
                self.page = Some(page);
            }
            Err(e) => self.error = Some(e),
        }
        self.clamp_cursor();
        Ok(())
    }

    fn cares_about(&self, ev: &StoreEvent) -> bool {
        ev.is_global()
            || matches!(
                ev.target(),
                Some(RefreshTarget::Notifications | RefreshTarget::NotificationDetails)
            )
    }

    fn keymap(&self) -> &[Binding] {
        BINDINGS
    }

    /// The header's note comes from here; without it `App` had nothing to
    /// ask and §8's stale indicator was unreachable.
    fn freshness(&self) -> Option<Freshness> {
        self.page.as_ref().map(Freshness::of)
    }

    /// What `r` means while this surface is on top.
    fn reconfigure(&mut self, ctx: &Ctx) {
        self.variants = ctx.inbox;
    }

    /// The focused row's subject on github.com.
    ///
    /// The enriched `html_url` when there is one — it is what GitHub itself
    /// says, and it addresses the kinds a URL cannot be derived for, like a
    /// release addressed by tag on the web and by id in the API. Falling back
    /// to `SubjectRef::browser_url` is what lets an *unenriched* row still be
    /// opened, which is most of the point: the list arrives before the detail
    /// does.
    fn browser_url(&self) -> Option<String> {
        let i = self.focused()?;
        let n = self.page.as_ref()?.value.items.get(i)?;
        if let omaghy_model::Enrichment::Ready(d) = &n.detail
            && !d.html_url.is_empty()
        {
            return Some(d.html_url.clone());
        }
        n.subject.as_ref()?.browser_url()
    }

    fn refresh_target(&self) -> Option<RefreshTarget> {
        Some(RefreshTarget::Notifications)
    }

    fn on_enter(&mut self, ctx: &Ctx) {
        ctx.store.refresh(RefreshTarget::Notifications);
    }

    fn on_leave(&mut self, ctx: &Ctx) {
        ctx.store.cancel(&RefreshTarget::Notifications);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        keys,
        widgets::test_support::{buffer, render},
    };
    use omaghy_model::{LimitKind, NotificationReason as R};
    use omaghy_store::{
        FakeStore,
        fake::{Behaviour, FIXTURE_NOW, corpus},
    };
    use std::{collections::BTreeSet, sync::Arc};
    use time::Duration;

    // ------------------------------------------------------------ harness

    fn ctx(store: FakeStore) -> Ctx {
        Ctx::new(Arc::new(store), FIXTURE_NOW, Icons::new(IconMode::Unicode))
    }

    /// The store, loaded once, exactly as the router would.
    async fn open(store: FakeStore) -> (Notifications, Ctx) {
        let ctx = ctx(store);
        let mut surface = Notifications::new();
        surface.on_enter(&ctx);
        surface.load(&ctx).await.expect("load never propagates");
        (surface, ctx)
    }

    async fn corpus_surface() -> (Notifications, Ctx) {
        open(FakeStore::with_corpus()).await
    }

    fn press(surface: &mut Notifications, ctx: &Ctx, c: char) -> Outcome {
        surface.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), ctx)
    }

    /// Press a key and let the router do what it does on `Redraw`: reload.
    async fn press_and_load(surface: &mut Notifications, ctx: &Ctx, c: char) {
        if press(surface, ctx, c) == Outcome::Redraw {
            surface.load(ctx).await.unwrap();
        }
    }

    async fn enter(surface: &mut Notifications, ctx: &Ctx) {
        if surface.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), ctx) == Outcome::Redraw
        {
            surface.load(ctx).await.unwrap();
        }
    }

    fn screen(surface: &mut Notifications, ctx: &Ctx, w: u16, h: u16) -> String {
        render(w, h, |f, a| surface.render(f, a, ctx))
    }

    // --------------------------------------------------------- the rows

    #[test]
    fn every_binding_is_dotted_named_uniquely_and_described() {
        // A binding on a key `app.rs` claims globally would never arrive, and
        // nothing would fail — the key would simply do something else.
        let surface = Notifications::new();
        let map = surface.keymap();
        for b in map {
            assert!(b.action.contains('.'), "{} should be dotted", b.action);
            assert!(!b.description.is_empty(), "{} has no description", b.action);
            // Only single-character specs: "Enter" is a key name, not the
            // letters E-n-t-e-r, and `r` inside it is not a binding.
            let mut chars = b.keys.chars();
            if let (Some(key), None) = (chars.next(), chars.next())
                && key.is_ascii_alphanumeric()
            {
                assert_eq!(
                    keys::resolve(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE), 1),
                    None,
                    "`{key}` ({}) is claimed globally and would never reach the surface",
                    b.action
                );
            }
        }
        let actions: BTreeSet<_> = map.iter().map(|b| b.action).collect();
        assert_eq!(actions.len(), map.len(), "action names must be unique");
    }

    #[test]
    fn the_surface_never_binds_a_key_the_shell_has_taken() {
        for b in Notifications::new().keymap() {
            for token in b.keys.split(" / ") {
                let Some(c) = (token.chars().count() == 1)
                    .then(|| token.chars().next().unwrap())
                    .filter(char::is_ascii_alphanumeric)
                else {
                    continue;
                };
                assert_eq!(
                    keys::resolve(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), 1),
                    None,
                    "`{c}` in `{}` is a global binding",
                    b.action
                );
            }
        }
    }

    #[tokio::test]
    async fn the_newest_notification_is_the_first_row() {
        let (mut s, ctx) = corpus_surface().await;
        let out = screen(&mut s, &ctx, 100, 12);
        // `group = by-repo` is the default, so the first line is a section
        // header. Groups are ordered by their newest member, so the newest
        // notification is still the first *row* — one line further down.
        let mut lines = out.lines();
        assert!(
            lines.next().unwrap().contains("quickshell"),
            "the newest row's repository heads the list: {out}"
        );
        assert!(lines.next().unwrap().contains("Add SocketServer"), "{out}");
    }

    #[test]
    fn every_reason_has_a_glyph_that_is_distinct_in_both_icon_modes() {
        // Colour cannot carry twelve reasons when a theme supplies one accent
        // (§9), so the glyph is the encoding — and an ASCII fallback shared
        // by two reasons would make a terminal without a Nerd Font strictly
        // worse than one with no glyph at all.
        let reasons = [
            R::ReviewRequested,
            R::Mention,
            R::TeamMention,
            R::Assign,
            R::Author,
            R::Comment,
            R::StateChange,
            R::CiActivity,
            R::Subscribed,
            R::Manual,
            R::Invitation,
            R::SecurityAlert,
            R::Other("something_new".into()),
        ];
        for icons in [Icons::UNICODE, Icons::ASCII] {
            let glyphs: BTreeSet<&str> =
                reasons.iter().map(|r| icons.get(reason_icon(r))).collect();
            assert_eq!(
                glyphs.len(),
                reasons.len(),
                "two reasons share a glyph in {:?} mode",
                icons.mode()
            );
        }
        // And every *known* reason's short label fits the state column's six
        // cells. `Other` carries whatever GitHub invented next and is elided
        // like any other over-long text.
        for r in &reasons[..12] {
            assert!(list::cells(reason_short(r)) <= 6, "{r:?} label is too wide");
        }
    }

    #[tokio::test]
    async fn no_variant_combination_ever_overflows_its_terminal() {
        // A row one cell too wide wraps, and a wrapped row silently halves
        // the list without failing anything.
        let (mut s, ctx) = corpus_surface().await;
        for reason in [ReasonMode::Glyph, ReasonMode::Text, ReasonMode::Hidden] {
            for repo in [
                RepoMode::Full,
                RepoMode::OwnerElided,
                RepoMode::HiddenWhenShared,
            ] {
                for rows in [RowMode::OneLine, RowMode::TwoLine] {
                    for group in [GroupMode::Flat, GroupMode::ByRepo] {
                        {
                            s.variants = Variants {
                                reason,
                                repo,
                                rows,
                                group,
                                ..Default::default()
                            };
                            for w in [40u16, 59, 60, 79, 80, 99, 100, 120] {
                                let out = screen(&mut s, &ctx, w, 14);
                                for line in out.lines() {
                                    assert!(
                                        list::cells(line) <= w as usize,
                                        "{line:?} at width {w} with {:?}",
                                        s.variants
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------- the cursor

    #[tokio::test]
    async fn the_cursor_moves_and_stops_at_both_ends() {
        let (mut s, ctx) = corpus_surface().await;
        assert_eq!(s.cursor, 0);
        press(&mut s, &ctx, 'k');
        assert_eq!(s.cursor, 0, "must not run off the top");
        for _ in 0..5 {
            press(&mut s, &ctx, 'j');
        }
        assert_eq!(s.cursor, 5);
        press(&mut s, &ctx, 'G');
        assert_eq!(s.cursor, 28, "G lands on the last of 29");
        press(&mut s, &ctx, 'j');
        assert_eq!(s.cursor, 28, "must not run off the bottom");
    }

    #[tokio::test]
    async fn grouping_inserts_headers_the_cursor_never_lands_on() {
        let (mut s, ctx) = corpus_surface().await;
        press(&mut s, &ctx, 'g');
        let entries = s.entries();
        let headers = entries
            .iter()
            .filter(|e| matches!(e, Entry::Header { .. }))
            .count();
        assert!(headers >= 5, "the corpus spans several repositories");
        // Every row still reachable, and only rows.
        assert_eq!(s.display_order().len(), 29);
        for i in 0..29 {
            s.cursor = i;
            assert!(s.focused().is_some(), "row {i} unreachable");
        }
        // Each group is contiguous: that is the whole point of grouping.
        let mut seen: Vec<String> = Vec::new();
        for e in &entries {
            if let Entry::Header { label, .. } = e {
                assert!(!seen.contains(label), "{label} appears twice");
                seen.push(label.clone());
            }
        }
    }

    // ------------------------------------------------------- the triage

    #[tokio::test]
    async fn marking_read_is_optimistic_and_then_agrees_with_the_store() {
        let (mut s, ctx) = corpus_surface().await;
        let id = s.items()[s.focused().unwrap()].id.clone();
        assert!(s.items()[0].unread, "the newest row starts unread");

        // The flip is on screen before the store has been told.
        s.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert!(!s.items()[0].unread, "the row flips before the write");
        assert_eq!(s.pending.len(), 1);

        s.load(&ctx).await.unwrap();
        assert!(s.pending.is_empty(), "the write was flushed");
        let unread = ctx
            .store
            .notifications(&NotificationQuery::unread())
            .await
            .unwrap();
        assert!(
            !unread.value.items.iter().any(|n| n.id == id),
            "the store agrees"
        );
        assert!(!s.items()[0].unread, "and so does the next read");
    }

    #[tokio::test]
    async fn a_failed_write_becomes_a_toast_rather_than_a_crash() {
        let store = FakeStore::with_corpus();
        store.set_behaviour(Behaviour {
            write_error: Some(StoreError::Forbidden),
            ..Default::default()
        });
        let (mut s, ctx) = open(store).await;
        enter(&mut s, &ctx).await;

        let toast = s.toast.as_ref().expect("a failure is reported");
        assert_eq!(toast.level, crate::widgets::Level::Error);
        // And the store's own value wins on the next read — the surface does
        // not keep insisting on an optimistic flip the store rejected.
        assert!(s.items()[0].unread, "rolled back to the store's truth");
        let out = screen(&mut s, &ctx, 100, 14);
        assert!(out.contains("access denied"), "{out}");
    }

    #[tokio::test]
    async fn triage_modes_differ_in_what_happens_to_the_row() {
        for mode in [TriageMode::Hide, TriageMode::Sink] {
            let (mut s, ctx) = corpus_surface().await;
            s.variants.triage = mode;
            let before = s.items()[s.focused().unwrap()].id.clone();
            enter(&mut s, &ctx).await;
            let after = s.items()[s.focused().unwrap()].id.clone();
            assert_ne!(
                before, after,
                "{mode:?} should move the cursor onto the next row"
            );
        }

        // The spec's answer keeps the row, so `Enter` is its own undo.
        let (mut s, ctx) = corpus_surface().await;
        assert_eq!(s.variants.triage, TriageMode::Grey);
        let before = s.items()[s.focused().unwrap()].id.clone();
        enter(&mut s, &ctx).await;
        assert_eq!(s.items()[s.focused().unwrap()].id, before);
        assert!(!s.items()[0].unread);
        enter(&mut s, &ctx).await;
        assert!(s.items()[0].unread, "pressing again undoes it");
    }

    #[tokio::test]
    async fn hiding_read_rows_is_named_as_a_filter_so_empty_is_not_a_lie() {
        let mut rows = corpus();
        for n in &mut rows {
            n.unread = false;
        }
        let (mut s, _ctx) = open(FakeStore::with_rows(rows)).await;
        s.variants.triage = TriageMode::Hide;
        assert_eq!(s.display_order().len(), 0);
        let state = s.state(s.filter_label().as_deref());
        assert_eq!(state.name(), "filtered-empty", "not an empty inbox");
    }

    // ------------------------------------------------------ the filters

    #[tokio::test]
    async fn unread_only_narrows_and_names_itself() {
        let (mut s, ctx) = corpus_surface().await;
        assert_eq!(s.items().len(), 29);
        press_and_load(&mut s, &ctx, 'u').await;
        assert_eq!(s.items().len(), 10);
        assert!(s.title().contains("unread only"), "{}", s.title());
        press_and_load(&mut s, &ctx, 'u').await;
        assert_eq!(s.items().len(), 29);
        assert!(s.filter_label().is_none());
    }

    #[tokio::test]
    async fn filtering_to_a_repository_and_then_to_nothing_is_not_an_empty_inbox() {
        let (mut s, ctx) = corpus_surface().await;
        // Flat, because the cursor indexes display entries: with group headers
        // in the list, an item index is not a cursor position. What is under
        // test is the filter, not the layout.
        s.variants.group = GroupMode::Flat;
        // The only row in this repository is read, so scoping to it and then
        // asking for unread gives the "filtered to empty" screen.
        let index = s
            .items()
            .iter()
            .position(|n| n.repo.name == "clipboard-sharing-mac-omarchy")
            .expect("the corpus has one");
        s.cursor = index;
        press_and_load(&mut s, &ctx, '/').await;
        assert_eq!(s.items().len(), 1);
        press_and_load(&mut s, &ctx, 'u').await;
        assert_eq!(s.items().len(), 0);

        let out = screen(&mut s, &ctx, 100, 14);
        assert!(out.contains("No matches"), "{out}");
        assert!(!out.contains("All caught up"), "{out}");
        assert!(
            out.contains("clipboard-sharing-mac-omarchy"),
            "names the filter: {out}"
        );
        assert!(out.contains("clear the repository filter"), "{out}");

        // And `/` again returns everything.
        press_and_load(&mut s, &ctx, '/').await;
        assert!(s.query.repo.is_none());
    }

    #[tokio::test]
    async fn an_empty_inbox_is_good_news_and_not_the_same_screen() {
        let (mut s, ctx) = open(FakeStore::empty()).await;
        let out = screen(&mut s, &ctx, 100, 14);
        assert!(out.contains("All caught up"), "{out}");
        assert!(
            out.contains("check for new ones"),
            "offers a next step: {out}"
        );
        assert!(!out.contains("No matches"), "{out}");
    }

    // ------------------------------------------------- the state matrix

    /// Every arm of §8, produced by the real surface against `FakeStore`.
    async fn state_matrix() -> Vec<(&'static str, (Notifications, Ctx))> {
        let mut out = Vec::new();

        let cold = FakeStore::with_corpus();
        cold.set_behaviour(Behaviour::offline_without_cache());
        out.push(("cold", open(cold).await));

        out.push(("populated", corpus_surface().await));

        let stale = FakeStore::with_corpus();
        stale.set_behaviour(Behaviour::offline_with_cache());
        out.push(("stale", open(stale).await));

        let refreshing = FakeStore::with_corpus();
        refreshing.set_behaviour(Behaviour {
            refreshing: true,
            ..Default::default()
        });
        out.push(("refreshing", open(refreshing).await));

        out.push(("empty", open(FakeStore::empty()).await));

        let mut rows = corpus();
        for n in &mut rows {
            n.unread = false;
        }
        let (mut filtered, fctx) = open(FakeStore::with_rows(rows)).await;
        filtered.query.read = ReadFilter::UnreadOnly;
        filtered.load(&fctx).await.unwrap();
        out.push(("filtered-empty", (filtered, fctx)));

        // Cache first, *then* the network goes away — which is what makes
        // this state different from having nothing, and what proves the
        // surface keeps the page it already read.
        let cached = Arc::new(FakeStore::with_corpus());
        let wctx = Ctx::new(cached.clone(), FIXTURE_NOW, Icons::new(IconMode::Unicode));
        let mut with_cache = Notifications::new();
        with_cache.load(&wctx).await.unwrap();
        cached.set_behaviour(Behaviour::failing(StoreError::Offline(
            "dns lookup failed".into(),
        )));
        with_cache.load(&wctx).await.unwrap();
        assert!(!with_cache.items().is_empty(), "the cached page survives");
        out.push(("offline-with-cache", (with_cache, wctx)));

        for (name, error) in [
            (
                "offline-without-cache",
                StoreError::Offline("dns lookup failed".into()),
            ),
            (
                "rate-limited",
                StoreError::RateLimited {
                    kind: LimitKind::Primary,
                    at: FIXTURE_NOW + Duration::hours(1),
                },
            ),
            ("forbidden", StoreError::Forbidden),
            (
                "error",
                StoreError::Upstream {
                    status: 502,
                    message: "bad gateway".into(),
                },
            ),
        ] {
            let store = FakeStore::with_corpus();
            store.set_behaviour(Behaviour::failing(error));
            out.push((name, open(store).await));
        }
        out
    }

    async fn state_matrix_screens() -> String {
        let mut rendered = String::new();
        for (name, (mut surface, ctx)) in state_matrix().await {
            let filter = surface.filter_label();
            assert_eq!(
                surface.state(filter.as_deref()).name(),
                name,
                "the scene must produce the state it claims"
            );
            rendered.push_str(&format!("── {name} ── {}\n", surface.title()));
            rendered.push_str(&screen(&mut surface, &ctx, 88, 12));
            rendered.push_str("\n\n");
        }
        rendered
    }

    #[tokio::test]
    async fn the_state_matrix() {
        insta::assert_snapshot!("state_matrix", state_matrix_screens().await);
    }

    #[tokio::test]
    async fn every_state_says_something_rather_than_nothing() {
        // Naming the way out is `App`'s footer now, and tested there. What is
        // still this surface's job is that no state renders a blank screen —
        // an error nobody can describe is an error nobody can report.
        for (name, (mut surface, ctx)) in state_matrix().await {
            let out = screen(&mut surface, &ctx, 88, 12);
            let ink: String = out.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(
                ink.chars().count() > 20,
                "{name} renders almost nothing: {out}"
            );
        }
    }

    // ---------------------------------------------- the variant matrix

    /// One screen per state of each switch, with everything else on its
    /// default. This is the snapshot the decision gets made from.
    async fn variant_matrix(width: u16) -> String {
        let (mut s, ctx) = corpus_surface().await;
        let mut out = String::new();
        for (key, label, steps) in [
            ('n', "reason", 3),
            ('p', "repo", 3),
            ('i', "icons", 2),
            ('w', "width", 3),
            ('t', "rows", 2),
            ('m', "triage", 3),
            ('g', "group", 2),
        ] {
            for step in 0..steps {
                out.push_str(&format!("── {label} #{step} ──\n"));
                out.push_str(&screen(&mut s, &ctx, width, 12));
                out.push_str("\n\n");
                press(&mut s, &ctx, key);
            }
            assert_eq!(
                s.variants,
                Variants::default(),
                "`{key}` must leave the others alone"
            );
        }
        out
    }

    #[tokio::test]
    async fn the_variant_matrix_at_120_columns() {
        insta::assert_snapshot!("variants_120", variant_matrix(120).await);
    }

    #[tokio::test]
    async fn the_variant_matrix_at_80_columns() {
        insta::assert_snapshot!("variants_80", variant_matrix(80).await);
    }

    /// The `m` variants are indistinguishable until something is marked
    /// read — which is the point of the switch, and what a still image of an
    /// untouched inbox cannot show.
    ///
    /// Each mode clears its top three rows using **its own** natural
    /// keystrokes, which is itself part of the answer: keeping the row means
    /// the cursor does not advance, so a sweep costs two keys an item rather
    /// than one.
    #[tokio::test]
    async fn triage_after_clearing_the_top_three_rows() {
        let mut out = String::new();
        for (label, steps, advance) in [("grey", 0, true), ("hide", 1, false), ("sink", 2, false)] {
            let (mut s, ctx) = corpus_surface().await;
            for _ in 0..steps {
                press(&mut s, &ctx, 'm');
            }
            for _ in 0..3 {
                enter(&mut s, &ctx).await;
                if advance {
                    press(&mut s, &ctx, 'j');
                }
            }
            let keys = if advance {
                "Enter j Enter j Enter j"
            } else {
                "Enter Enter Enter"
            };
            out.push_str(&format!("── {label} · {keys} ──\n"));
            out.push_str(&screen(&mut s, &ctx, 100, 12));
            out.push_str("\n\n");
        }
        insta::assert_snapshot!("triage_after_marking_read", out);
    }

    #[tokio::test]
    async fn grouped_and_two_line_together() {
        // The two variants that change the shape of the list rather than the
        // contents of a row, composed — which is the combination most likely
        // to be wrong.
        let (mut s, ctx) = corpus_surface().await;
        s.variants.group = GroupMode::ByRepo;
        s.variants.rows = RowMode::TwoLine;
        let mut out = String::from("── grouped · two-line ──\n");
        out.push_str(&screen(&mut s, &ctx, 100, 18));
        out.push_str("\n\n── grouped · two-line · ascii ──\n");
        out.push_str(&screen(&mut s, &ctx, 100, 18));
        insta::assert_snapshot!("grouped_two_line", out);
    }

    // ----------------------------------------- style, not merely text

    #[tokio::test]
    async fn unread_is_a_marker_and_weight_and_the_cursor_is_a_glyph() {
        // On a monochrome Omarchy theme neither survives on colour alone.
        let (mut s, ctx) = corpus_surface().await;
        // One line per row and no section headers: this asserts exact cells,
        // and what it is testing — that unread and the cursor are not carried
        // by colour — is independent of the row layout.
        s.variants.group = GroupMode::Flat;
        s.variants.rows = RowMode::OneLine;
        s.cursor = 2;
        let buf = buffer(100, 14, |f, a| s.render(f, a, &ctx));

        assert_eq!(buf[(0, 2)].symbol(), Icons::UNICODE.cursor());
        assert_eq!(buf[(0, 1)].symbol(), " ", "only one cursor glyph");
        assert!(
            buf[(4, 2)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED),
            "the cursor row is also reversed"
        );

        // Read and unread interleave by age, so the marker column is not a
        // block — it must track each row. Check against the data rather than
        // against a row number.
        let rows = s.display_order();
        let items = s.items();
        let mut seen_unread = false;
        let mut seen_read = false;
        for (y, &i) in rows.iter().enumerate().take(10) {
            let y = y as u16;
            let unread = items[i].unread;
            seen_unread |= unread;
            seen_read |= !unread;
            let marker = buf[(1, y)].symbol();
            let bold = buf[(6, y)].style().add_modifier.contains(Modifier::BOLD);
            if unread {
                assert_eq!(
                    marker,
                    Icons::UNICODE.get(Icon::Unread),
                    "row {y} unread, no marker"
                );
                assert!(bold, "row {y} is unread but not bold");
            } else {
                assert_ne!(
                    marker,
                    Icons::UNICODE.get(Icon::Unread),
                    "row {y} read, has a marker"
                );
                assert!(!bold, "row {y} is read but bold");
            }
        }
        assert!(
            seen_unread && seen_read,
            "the corpus must interleave read and unread, or this proves nothing"
        );
        // Row 10 used to be the first read row, back when the corpus put every
        // unread row above every read one. The loop above now covers it.
    }

    #[tokio::test]
    async fn the_reason_word_is_on_the_focused_row_even_though_no_column_fits_it() {
        // §9 asks for the full word on the focused row; `Column::State` has
        // six cells, and "review requested" is sixteen.
        let (mut s, ctx) = corpus_surface().await;
        let out = screen(&mut s, &ctx, 100, 14);
        assert!(out.contains("review requested"), "{out}");
        assert!(out.contains("quickshell/quickshell"), "{out}");
        press(&mut s, &ctx, 'j');
        let out = screen(&mut s, &ctx, 100, 14);
        assert!(
            out.contains("commented"),
            "the line follows the cursor: {out}"
        );
    }

    #[tokio::test]
    async fn stale_and_refreshing_say_so_without_hiding_the_rows() {
        let stale = FakeStore::with_corpus();
        stale.set_behaviour(Behaviour::offline_with_cache());
        let (mut s, ctx) = open(stale).await;
        let out = screen(&mut s, &ctx, 100, 14);
        assert!(out.contains("cached"), "{out}");
        assert!(out.contains("Add SocketServer"), "content survives: {out}");

        let refreshing = FakeStore::with_corpus();
        refreshing.set_behaviour(Behaviour {
            refreshing: true,
            ..Default::default()
        });
        let (mut s, ctx) = open(refreshing).await;
        let out = screen(&mut s, &ctx, 100, 14);
        assert!(out.contains("refreshing"), "{out}");
        assert!(out.contains("Add SocketServer"), "{out}");
    }

    #[tokio::test]
    async fn a_read_failure_is_a_screen_rather_than_a_torn_down_app() {
        // `load` returning `Err` would take the whole app with it instead of
        // drawing one of §8's designed states.
        let store = FakeStore::with_corpus();
        store.set_behaviour(Behaviour::failing(StoreError::Forbidden));
        let ctx = ctx(store);
        let mut s = Notifications::new();
        assert!(
            s.load(&ctx).await.is_ok(),
            "errors are kept, not propagated"
        );
        let out = screen(&mut s, &ctx, 100, 14);
        assert!(out.contains("No access"), "{out}");
        assert!(
            !out.contains("r  retry"),
            "a lost repo is not retried: {out}"
        );
    }

    #[tokio::test]
    async fn a_short_body_drops_the_readout_before_it_drops_a_row() {
        // The switcher is scaffolding; the notifications are the point.
        let (mut s, ctx) = corpus_surface().await;
        let out = screen(&mut s, &ctx, 80, 2);
        assert!(out.contains("Add SocketServer"), "{out}");
        assert!(!out.contains("n glyph"), "{out}");
    }
}

//! The list row, and the responsive columns it drops as the terminal narrows.
//!
//! Terminal width is not negotiable, so **columns drop by priority**
//! (`spec/30-ui.md` §4.1). The title always takes the remaining space and
//! elides right; the age never drops, because it is the only always-present
//! sort cue (§9).
//!
//! Two encodings, never one: the cursor is a glyph *and* reverse video, and
//! an unread row is a marker glyph *and* bold. Neither survives a monochrome
//! Omarchy theme on colour alone, and several of them are monochrome.

use crate::theme::{Icon, Icons, Role};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem, ListState},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Cells before the first column: the cursor, the unread marker, a space.
const GUTTER: u16 = 3;

/// The columns of `spec/30-ui.md` §4.1, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Column {
    Icon,
    Title,
    Repo,
    Number,
    State,
    Checks,
    Actor,
    Age,
}

impl Column {
    /// The fixed cell budget for this column. `Title` flexes and reports 0.
    pub fn width(self) -> u16 {
        match self {
            Self::Title => 0,
            Self::Icon => 1,
            // "14mo" is the widest age the model produces.
            Self::Age => 4,
            // "#12345" covers every repository anyone will scroll through.
            Self::Number => 6,
            // "✓ 12/14" without the slash: glyph, space, four digits.
            Self::Checks => 6,
            Self::State => 8,
            Self::Actor => 12,
            Self::Repo => 20,
        }
    }
}

/// The column set for a width, plus the space left for the title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Columns {
    cols: Vec<Column>,
    title: u16,
    width: u16,
}

impl Columns {
    /// The breakpoint table of `spec/30-ui.md` §4.1.
    ///
    /// One deliberate divergence, recorded in the spec in this PR: below 60
    /// the table says "title only", which contradicts §9's rule that age is
    /// never the column that drops. §9 wins — it is a measured finding, and
    /// four cells is a cheap price for the only sort cue on the screen.
    pub fn for_width(width: u16) -> Self {
        use Column::*;
        let cols: &[Column] = match width {
            0..60 => &[Title, Age],
            60..80 => &[Icon, Title, Age],
            80..100 => &[Icon, Title, Repo, State, Age],
            100..120 => &[Icon, Title, Repo, Number, State, Checks, Age],
            _ => &[Icon, Title, Repo, Number, State, Checks, Actor, Age],
        };
        Self::assemble(cols.to_vec(), width)
    }

    fn assemble(cols: Vec<Column>, width: u16) -> Self {
        let fixed: u16 = cols.iter().map(|c| c.width()).sum();
        let separators = cols.len().saturating_sub(1) as u16;
        let title = width.saturating_sub(GUTTER + fixed + separators).max(1);
        Self { cols, title, width }
    }

    /// Drop a column that has become noise.
    ///
    /// Used for the repo column when every visible row shares one repository:
    /// fourteen consecutive rows reading `ShaxP/shax` is measured noise
    /// (`spec/30-ui.md` §9), and the header names it instead.
    pub fn without(self, drop: Column) -> Self {
        let cols = self.cols.into_iter().filter(|c| *c != drop).collect();
        Self::assemble(cols, self.width)
    }

    pub fn contains(&self, c: Column) -> bool {
        self.cols.contains(&c)
    }

    /// How wide the title may be before it elides.
    pub fn title_width(&self) -> u16 {
        self.title
    }

    pub fn as_slice(&self) -> &[Column] {
        &self.cols
    }
}

/// A coloured cell that always carries a glyph.
///
/// The glyph is not optional by construction: a role like `Danger` that
/// resolves to red on one theme and to the foreground colour on another is
/// only legible because of the glyph beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub icon: Icon,
    pub text: String,
    pub role: Role,
}

impl Cell {
    pub fn new(icon: Icon, text: impl Into<String>, role: Role) -> Self {
        Self {
            icon,
            text: text.into(),
            role,
        }
    }
}

/// One line of a list, in the vocabulary of §4.1's columns.
///
/// Surfaces build these from domain types; the widget knows nothing about
/// notifications, pull requests or issues, which is what lets the dashboard
/// and the inbox render identically.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub icon: Option<Icon>,
    pub unread: bool,
    pub title: String,
    pub repo: Option<String>,
    pub number: Option<String>,
    pub state: Option<Cell>,
    pub checks: Option<Cell>,
    pub actor: Option<String>,
    pub age: String,
}

impl Row {
    pub fn new(title: impl Into<String>, age: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            age: age.into(),
            ..Default::default()
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn unread(mut self, unread: bool) -> Self {
        self.unread = unread;
        self
    }

    pub fn repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    pub fn number(mut self, number: u64) -> Self {
        self.number = Some(format!("#{number}"));
        self
    }

    pub fn state(mut self, cell: Cell) -> Self {
        self.state = Some(cell);
        self
    }

    pub fn checks(mut self, cell: Cell) -> Self {
        self.checks = Some(cell);
        self
    }

    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }
}

/// The repository every row shares, if they all share one.
///
/// `None` for a mixed list, and `None` for a single row — eliding the only
/// repository on screen tells the reader nothing and costs them the name.
pub fn shared_repo(rows: &[Row]) -> Option<&str> {
    let mut it = rows.iter().map(|r| r.repo.as_deref());
    let first = it.next().flatten()?;
    if rows.len() < 2 {
        return None;
    }
    it.all(|r| r == Some(first)).then_some(first)
}

/// Drop `owner/` when it is the viewer's own — measured noise, §9.
pub fn elide_owner<'a>(repo: &'a str, viewer: Option<&str>) -> &'a str {
    match (repo.split_once('/'), viewer) {
        (Some((owner, name)), Some(v)) if owner.eq_ignore_ascii_case(v) => name,
        _ => repo,
    }
}

/// Display width in terminal cells.
///
/// Not `chars().count()`: a CJK ideograph or a wide emoji occupies two cells,
/// a combining mark none. Counting characters misaligns every column to the
/// right of such a title — and GitHub titles contain both.
pub fn cells(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate to `width` cells, marking the cut with a one-cell ellipsis.
///
/// A wide character straddling the boundary is dropped rather than half-drawn:
/// there is no half cell to put it in.
pub fn elide(s: &str, width: usize, icons: Icons) -> String {
    if width == 0 {
        return String::new();
    }
    if cells(s) <= width {
        return s.to_owned();
    }
    let budget = width.saturating_sub(cells(icons.ellipsis()));
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push_str(icons.ellipsis());
    out
}

/// Break `s` into lines of at most `width` cells.
///
/// On whitespace where it can and mid-word where it must: a filesystem path or
/// a URL has no spaces to break at, and those are exactly what the messages
/// that need wrapping are made of. Found by a settings note reading
/// `applied, but not saved — /home/…/config.toml could ` — the rest of the
/// sentence, including *why*, was off the edge of the panel.
///
/// Measured in cells, not characters, for the same reason [`elide`] is.
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();

    for word in s.split_whitespace() {
        if cells(word) > width {
            // Longer than a whole line: it has to be cut somewhere, so cut it
            // where the line ends rather than letting it run off the edge.
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            let mut chunk = String::new();
            for c in word.chars() {
                let cw = UnicodeWidthChar::width(c).unwrap_or(0);
                if cells(&chunk) + cw > width {
                    out.push(std::mem::take(&mut chunk));
                }
                chunk.push(c);
            }
            // The tail stays open, so the next word can share its line.
            line = chunk;
            continue;
        }

        let would_be = if line.is_empty() {
            cells(word)
        } else {
            cells(&line) + 1 + cells(word)
        };
        if would_be > width {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }

    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// Pad or elide to exactly `width` cells, left-aligned.
fn fit(s: &str, width: usize, icons: Icons) -> String {
    let mut out = elide(s, width, icons);
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(cells(&out))));
    out
}

/// Pad or elide to exactly `width` cells, right-aligned. The age column is
/// right-aligned so the unit letters line up and scan as one column.
fn fit_right(s: &str, width: usize, icons: Icons) -> String {
    let cut = elide(s, width, icons);
    let mut out: String = std::iter::repeat_n(' ', width.saturating_sub(cells(&cut))).collect();
    out.push_str(&cut);
    out
}

/// `owner/name` in a narrow column: keep the name, lose the owner first.
fn fit_repo(repo: &str, width: usize, icons: Icons) -> String {
    if cells(repo) <= width {
        return fit(repo, width, icons);
    }
    // The name is what distinguishes two rows; the owner usually repeats, so
    // it is what gets spent first.
    if let Some((_, name)) = repo.split_once('/')
        && width > 3
    {
        let room = width - 2;
        return fit(
            &format!("{}/{}", icons.ellipsis(), elide(name, room, icons)),
            width,
            icons,
        );
    }
    fit(repo, width, icons)
}

/// Render one row as a styled line.
///
/// `repo` is passed separately because whether to show it, and how much of
/// it, is a decision about the whole list rather than about one row.
pub fn row_line(
    row: &Row,
    repo: Option<&str>,
    cols: &Columns,
    icons: Icons,
    cursor: bool,
) -> Line<'static> {
    let mut spans = Vec::new();

    // The gutter: cursor glyph, unread marker, space. Both are glyphs, so
    // both survive a theme with one colour and a terminal without bold.
    spans.push(Span::styled(
        if cursor { icons.cursor() } else { " " },
        Role::Accent.style(),
    ));
    spans.push(Span::styled(
        if row.unread {
            icons.get(Icon::Unread)
        } else {
            " "
        },
        Role::Accent.style(),
    ));
    spans.push(Span::raw(" "));

    let title_style = if row.unread {
        Role::Unread.style()
    } else {
        Role::Default.style()
    };

    for (i, col) in cols.as_slice().iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let w = if *col == Column::Title {
            cols.title_width() as usize
        } else {
            col.width() as usize
        };
        match col {
            Column::Icon => {
                let glyph = row.icon.map_or(" ", |i| icons.get(i));
                spans.push(Span::styled(glyph.to_owned(), Role::Accent.style()));
            }
            Column::Title => {
                spans.push(Span::styled(fit(&row.title, w, icons), title_style));
            }
            Column::Repo => {
                let text = repo.unwrap_or("");
                spans.push(Span::styled(fit_repo(text, w, icons), Role::Muted.style()));
            }
            Column::Number => {
                let text = row.number.as_deref().unwrap_or("");
                spans.push(Span::styled(fit_right(text, w, icons), Role::Muted.style()));
            }
            // Glyph, space, text: the glyph is what carries the meaning when
            // the theme resolves the role's colour to the foreground.
            Column::State => match &row.state {
                Some(cell) => {
                    spans.push(Span::styled(
                        icons.get(cell.icon).to_owned(),
                        cell.role.style(),
                    ));
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        fit(&cell.text, w.saturating_sub(2), icons),
                        cell.role.style(),
                    ));
                }
                None => spans.push(Span::raw(" ".repeat(w))),
            },
            Column::Checks => match &row.checks {
                Some(cell) => {
                    spans.push(Span::styled(
                        icons.get(cell.icon).to_owned(),
                        cell.role.style(),
                    ));
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        fit_right(&cell.text, w.saturating_sub(2), icons),
                        cell.role.style(),
                    ));
                }
                None => spans.push(Span::raw(" ".repeat(w))),
            },
            Column::Actor => {
                let text = row.actor.as_deref().unwrap_or("");
                spans.push(Span::styled(fit(text, w, icons), Role::Muted.style()));
            }
            Column::Age => {
                spans.push(Span::styled(
                    fit_right(&row.age, w, icons),
                    Role::Muted.style(),
                ));
            }
        }
    }

    Line::from(spans)
}

/// A list of [`Row`]s with responsive columns.
///
/// Scrolling and the selected index live in ratatui's [`ListState`], which
/// the surface owns — so returning to a list lands where you left it
/// (`spec/30-ui.md` §3.1).
#[derive(Debug, Clone)]
pub struct RowList<'a> {
    rows: &'a [Row],
    icons: Icons,
    viewer: Option<&'a str>,
    elide_shared_repo: bool,
}

impl<'a> RowList<'a> {
    pub fn new(rows: &'a [Row]) -> Self {
        Self {
            rows,
            icons: Icons::UNICODE,
            viewer: None,
            elide_shared_repo: true,
        }
    }

    pub fn icons(mut self, icons: Icons) -> Self {
        self.icons = icons;
        self
    }

    /// The viewer's login, so `ShaxP/omaghy` can render as `omaghy`.
    pub fn viewer(mut self, viewer: &'a str) -> Self {
        self.viewer = Some(viewer);
        self
    }

    /// Keep the repo column even when every row shares one repository.
    pub fn keep_shared_repo(mut self) -> Self {
        self.elide_shared_repo = false;
        self
    }

    /// The repository the header should name, because the rows no longer do.
    pub fn elided_repo(&self) -> Option<&'a str> {
        self.elide_shared_repo
            .then(|| shared_repo(self.rows))
            .flatten()
    }

    /// The columns this list will actually draw at `width`.
    pub fn columns(&self, width: u16) -> Columns {
        let cols = Columns::for_width(width);
        if self.elided_repo().is_some() {
            cols.without(Column::Repo)
        } else {
            cols
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect, state: &mut ListState) {
        let cols = self.columns(area.width);
        let selected = state.selected();
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let repo = row.repo.as_deref().map(|r| elide_owner(r, self.viewer));
                ListItem::new(row_line(row, repo, &cols, self.icons, selected == Some(i)))
            })
            .collect();

        f.render_stateful_widget(
            List::new(items).highlight_style(Role::Selected.style()),
            area,
            state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Row {
        Row::new("Add SocketServer reconnect backoff and idle timeout", "4m")
            .icon(Icon::PrOpen)
            .unread(true)
            .repo("quickshell/quickshell")
            .number(61)
            .state(Cell::new(Icon::PrOpen, "open", Role::Success))
            .checks(Cell::new(Icon::CheckFail, "2/14", Role::Danger))
            .actor("ShaxP")
    }

    fn rendered(width: u16, icons: Icons) -> String {
        let row = sample();
        let cols = Columns::for_width(width);
        row_line(&row, row.repo.as_deref(), &cols, icons, false)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn columns_drop_by_priority_as_the_terminal_narrows() {
        use Column::*;
        assert_eq!(
            Columns::for_width(120).as_slice(),
            [Icon, Title, Repo, Number, State, Checks, Actor, Age]
        );
        assert_eq!(
            Columns::for_width(100).as_slice(),
            [Icon, Title, Repo, Number, State, Checks, Age]
        );
        assert_eq!(
            Columns::for_width(80).as_slice(),
            [Icon, Title, Repo, State, Age]
        );
        assert_eq!(Columns::for_width(60).as_slice(), [Icon, Title, Age]);
        assert_eq!(Columns::for_width(59).as_slice(), [Title, Age]);
    }

    #[test]
    fn the_age_column_never_drops() {
        // It is the only always-present sort cue (spec/30-ui.md §9), which is
        // why §4.1's "title only" row is wrong below 60.
        for w in [40, 59, 60, 79, 80, 99, 100, 119, 120, 200] {
            assert!(
                Columns::for_width(w).contains(Column::Age),
                "age dropped at width {w}"
            );
            assert!(Columns::for_width(w).contains(Column::Title));
        }
    }

    #[test]
    fn a_row_occupies_exactly_the_width_it_was_given() {
        // A row one cell too wide wraps and silently halves the list.
        for w in [40, 59, 60, 79, 80, 99, 100, 119, 120, 200] {
            for icons in [Icons::UNICODE, Icons::ASCII] {
                let line = rendered(w, icons);
                assert_eq!(
                    line.chars().count(),
                    w as usize,
                    "width {w} rendered {} cells: {line:?}",
                    line.chars().count()
                );
            }
        }
    }

    #[test]
    fn both_icon_modes_produce_identical_layout() {
        // The columns must land in the same cells with and without a Nerd
        // Font, or switching modes reflows every row.
        for w in [60, 80, 100, 120] {
            let u = rendered(w, Icons::UNICODE);
            let a = rendered(w, Icons::ASCII);
            assert_eq!(u.chars().count(), a.chars().count(), "width {w}");
            // Not merely the same length: the same cells. Byte offsets would
            // differ between a three-byte glyph and a one-byte letter, so
            // this counts characters.
            let spaces = |s: &str| -> Vec<usize> {
                s.chars()
                    .enumerate()
                    .filter(|(_, c)| *c == ' ')
                    .map(|(i, _)| i)
                    .collect()
            };
            assert_eq!(spaces(&u), spaces(&a), "width {w}: {u:?} vs {a:?}");
        }
    }

    #[test]
    fn the_title_elides_right_and_takes_what_is_left() {
        let narrow = rendered(60, Icons::UNICODE);
        assert!(narrow.contains('…'), "long title must elide: {narrow:?}");
        assert!(
            narrow.contains("Add SocketServer"),
            "the front of the title survives: {narrow:?}"
        );
        // Wider terminals spend the extra space on the title, not on padding.
        assert!(Columns::for_width(200).title_width() > Columns::for_width(120).title_width());
    }

    #[test]
    fn unread_is_encoded_twice() {
        // A marker column and weight, so it survives both a monochrome theme
        // and a terminal without bold (spec/30-ui.md §9).
        let row = sample();
        let cols = Columns::for_width(100);
        let line = row_line(&row, None, &cols, Icons::UNICODE, false);
        assert_eq!(
            line.spans[1].content.as_ref(),
            Icons::UNICODE.get(Icon::Unread)
        );
        let title = line
            .spans
            .iter()
            .find(|s| s.content.contains("SocketServer"))
            .expect("title span");
        assert!(
            title
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );

        let read = row_line(&sample().unread(false), None, &cols, Icons::UNICODE, false);
        assert_eq!(read.spans[1].content.as_ref(), " ");
        assert!(
            !read
                .spans
                .iter()
                .find(|s| s.content.contains("SocketServer"))
                .unwrap()
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn the_cursor_is_a_glyph_as_well_as_a_highlight() {
        // Reverse video is invisible on a few terminals and in every
        // screenshot; the glyph is what makes the cursor findable.
        let cols = Columns::for_width(100);
        let on = row_line(&sample(), None, &cols, Icons::UNICODE, true);
        let off = row_line(&sample(), None, &cols, Icons::UNICODE, false);
        assert_eq!(on.spans[0].content.as_ref(), Icons::UNICODE.cursor());
        assert_eq!(off.spans[0].content.as_ref(), " ");
    }

    #[test]
    fn a_coloured_cell_cannot_be_built_without_a_glyph() {
        // Structural, not a convention: Cell::icon is not an Option, so
        // "red text alone" is unrepresentable.
        let cell = Cell::new(Icon::CheckFail, "2/14", Role::Danger);
        assert!(cell.role.pairs_with_a_glyph());
        let cols = Columns::for_width(120);
        let line = row_line(&sample(), None, &cols, Icons::UNICODE, false);
        assert!(
            line.spans
                .iter()
                .any(|s| s.content.as_ref() == Icons::UNICODE.get(Icon::CheckFail)),
            "the failing-check glyph must be on the row"
        );
    }

    #[test]
    fn a_shared_repository_is_elided_but_a_lone_one_is_not() {
        let rows = vec![
            Row::new("a", "1m").repo("ShaxP/shax"),
            Row::new("b", "2m").repo("ShaxP/shax"),
        ];
        assert_eq!(shared_repo(&rows), Some("ShaxP/shax"));
        let list = RowList::new(&rows);
        assert_eq!(list.elided_repo(), Some("ShaxP/shax"));
        assert!(!list.columns(120).contains(Column::Repo));

        // One row: eliding the only repository on screen costs the reader
        // the name and tells them nothing.
        let one = vec![Row::new("a", "1m").repo("ShaxP/shax")];
        assert_eq!(shared_repo(&one), None);

        let mixed = vec![
            Row::new("a", "1m").repo("ShaxP/shax"),
            Row::new("b", "2m").repo("rust-lang/rust"),
        ];
        assert_eq!(shared_repo(&mixed), None);
        assert!(RowList::new(&mixed).columns(120).contains(Column::Repo));
    }

    #[test]
    fn the_owner_is_elided_only_when_it_is_the_viewers_own() {
        assert_eq!(elide_owner("ShaxP/omaghy", Some("ShaxP")), "omaghy");
        assert_eq!(elide_owner("shaxp/omaghy", Some("ShaxP")), "omaghy");
        assert_eq!(
            elide_owner("rust-lang/rust", Some("ShaxP")),
            "rust-lang/rust"
        );
        assert_eq!(elide_owner("ShaxP/omaghy", None), "ShaxP/omaghy");
    }

    #[test]
    fn a_long_repository_keeps_its_name_and_loses_its_owner() {
        let long = "some-very-long-organization-name/an-equally-long-repository-name";
        let out = fit_repo(long, 20, Icons::UNICODE);
        assert_eq!(out.chars().count(), 20);
        assert!(out.starts_with('…'), "owner goes first: {out:?}");
        assert!(out.contains("an-equally"), "the name survives: {out:?}");
    }

    #[test]
    fn eliding_never_exceeds_its_budget_in_either_mode() {
        for icons in [Icons::UNICODE, Icons::ASCII] {
            for w in 0..12usize {
                assert!(elide("a-fairly-long-string", w, icons).chars().count() <= w);
                assert_eq!(fit("abc", w, icons).chars().count(), w);
                assert_eq!(fit_right("abc", w, icons).chars().count(), w);
            }
        }
    }

    #[test]
    fn the_age_is_right_aligned_so_the_units_line_up() {
        assert_eq!(fit_right("4m", 4, Icons::UNICODE), "  4m");
        assert_eq!(fit_right("14mo", 4, Icons::UNICODE), "14mo");
    }
    #[test]
    fn width_is_measured_in_cells_not_characters() {
        let icons = Icons::new(crate::theme::IconMode::Unicode);

        // A CJK ideograph is two cells wide. Counting characters would call
        // this string 4 wide when it draws 8, pushing every column right of
        // it out of alignment.
        let cjk = "修复中文标题";
        assert_eq!(cjk.chars().count(), 6);
        assert_eq!(cells(cjk), 12);

        // Fitting must produce exactly `width` cells, whatever the script.
        for w in [4usize, 7, 12, 20] {
            assert_eq!(cells(&fit(cjk, w, icons)), w, "fit to {w}");
            assert_eq!(cells(&fit_right(cjk, w, icons)), w, "fit_right to {w}");
        }
    }

    #[test]
    fn a_wide_character_straddling_the_cut_is_dropped_not_halved() {
        let icons = Icons::new(crate::theme::IconMode::Unicode);
        // Budget after the ellipsis is 2 cells: one ideograph fits, the second
        // would need a cell that does not exist.
        let out = elide("中文字", 3, icons);
        assert_eq!(cells(&out), 3, "never overflows the column");
        assert!(out.ends_with(icons.ellipsis()));
    }

    #[test]
    fn zero_width_marks_do_not_consume_a_cell() {
        let icons = Icons::new(crate::theme::IconMode::Unicode);
        // "e" + combining acute renders in one cell.
        let combining = "e\u{0301}fg";
        assert_eq!(combining.chars().count(), 4);
        assert_eq!(cells(combining), 3);
        assert_eq!(cells(&fit(combining, 6, icons)), 6);
    }
    #[test]
    fn wrapping_breaks_on_spaces_and_keeps_every_word() {
        let lines = wrap("the quick brown fox jumps over the lazy dog", 12);
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(cells(l) <= 12, "`{l}` is {} cells", cells(l));
        }
        assert_eq!(
            lines.join(" "),
            "the quick brown fox jumps over the lazy dog",
            "wrapping must not lose or reorder words"
        );
    }

    /// The case that mattered: a path has no spaces to break at, and a
    /// settings note is mostly path.
    #[test]
    fn a_word_longer_than_the_line_is_cut_rather_than_overflowing() {
        let path = "/home/shahram/.config/omaghy/config.toml";
        let lines = wrap(&format!("could not write {path} sorry"), 20);
        for l in &lines {
            assert!(cells(l) <= 20, "`{l}` overflowed");
        }
        let rejoined: String = lines.join("");
        assert!(
            rejoined.contains(".config/omaghy/config.toml"),
            "the path survives the break: {rejoined}"
        );
        assert!(lines.last().unwrap().ends_with("sorry"), "{lines:?}");
    }

    #[test]
    fn wrapping_measures_cells_not_characters() {
        // Four CJK ideographs are eight cells, so they cannot share a line of
        // six with anything.
        let lines = wrap("日本語テスト ok", 6);
        for l in &lines {
            assert!(cells(l) <= 6, "`{l}` is {} cells", cells(l));
        }
    }

    #[test]
    fn a_short_message_stays_one_line_and_an_empty_one_is_not_nothing() {
        assert_eq!(wrap("fits", 20), vec!["fits".to_owned()]);
        assert_eq!(wrap("", 20), vec![String::new()]);
        assert_eq!(wrap("anything", 0), vec![String::new()]);
    }
}

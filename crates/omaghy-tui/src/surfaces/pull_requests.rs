//! Pull requests — the list, and one opened.
//!
//! One surface, two views, chosen by the route (`spec/30-ui.md` §3.2):
//!
//! ```text
//! pr                      the default list: is:open review-requested:@me
//! pr?q=is:open author:@me any GitHub search — what a dashboard section opens
//! pr:ShaxP/shax           one repository's open PRs
//! pr:ShaxP/shax#61        one pull request, opened
//! ```
//!
//! `Enter` on a row **pushes** the detail route rather than swapping the view
//! in place, so the list underneath keeps its cursor and `q` lands back on
//! it (§3.1). The two views share nothing but this module and the domain
//! types; a reader of either can ignore the other.
//!
//! **The detail is one scrolling document, not tabs.** Header, checks, the
//! description, then the timeline oldest-first — the order a reviewer reads a
//! PR in. Files will be a tab when there are files (`90-plan.md` §4, M2.5);
//! a tab bar with one tab is furniture.
//!
//! **Markdown is the source, wrapped.** `Markdown::blocks` is empty until
//! M2.4 parses it, so bodies render as their source text, word-wrapped to
//! the column. Honest, readable, and a smaller change to replace than a
//! renderer written against blocks nothing fills.
//!
//! **Bold means "needs you".** The list widget's unread marker and weight
//! carry `ReviewSummary::awaits_me` here: a review requested of you that you
//! have not yet given is the row this list exists for, and it is the one
//! thing worth two encodings.
//!
//! Timestamps in the detail are absolute (§9); the list keeps relative ages.

use crate::{
    keys::Binding,
    route::{Route, SurfaceId},
    surface::{Ctx, Outcome, Surface},
    theme::{Icon, Icons, Role},
    widgets::{
        Cell, Conditions, EmptyCopy, Row, RowList, StateView, SurfaceState,
        chrome::Freshness,
        classify, elide,
        list::{cells, wrap},
    },
};
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use omaghy_model::{
    Actor, CheckConclusion, CheckRollup, CheckRun, CheckStatus, Label, Mergeable, PrDetail,
    PrDisplayStatus, PullRequest, RepoRef, Result, ReviewDecision, ReviewState, ReviewThread,
    RollupState, StoreError, SubjectKind, SubjectRef, TimelineEntry, TimelineEvent, TimelineKind,
    age, fold,
};
use omaghy_store::{Fresh, Page, PrQuery, RefreshTarget, StoreEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{ListState, Paragraph},
};
use time::{OffsetDateTime, macros::format_description};

/// What `pr` with no argument lists. The dashboard's first section, because
/// it is the question a client is for.
pub const DEFAULT_QUERY: &str = "is:open review-requested:@me";

/// Runs of low-signal events shorter than this stay expanded
/// (`10-domain-model.md` §3.4: folding one event behind "1 more" saves nothing).
const FOLD_MIN_RUN: usize = 3;

const LIST_BINDINGS: &[Binding] = &[
    Binding::new("pr.next", "j / ↓", "next").on(KeyCode::Char('j')),
    Binding::new("pr.prev", "k / ↑", "previous").on(KeyCode::Char('k')),
    Binding::new("pr.open", "Enter", "open").on(KeyCode::Enter),
    Binding::new("pr.first", "g", "first").on(KeyCode::Char('g')),
    Binding::new("pr.last", "G", "last").on(KeyCode::Char('G')),
    Binding::new("pr.page-down", "Ctrl-d", "half page down").on_ctrl(KeyCode::Char('d')),
    Binding::new("pr.page-up", "Ctrl-u", "half page up").on_ctrl(KeyCode::Char('u')),
];

const DETAIL_BINDINGS: &[Binding] = &[
    Binding::new("pr.scroll-down", "j / ↓", "scroll down").on(KeyCode::Char('j')),
    Binding::new("pr.scroll-up", "k / ↑", "scroll up").on(KeyCode::Char('k')),
    Binding::new("pr.page-down", "Ctrl-d", "half page down").on_ctrl(KeyCode::Char('d')),
    Binding::new("pr.page-up", "Ctrl-u", "half page up").on_ctrl(KeyCode::Char('u')),
    Binding::new("pr.top", "g", "top").on(KeyCode::Char('g')),
    Binding::new("pr.bottom", "G", "bottom").on(KeyCode::Char('G')),
    Binding::new("pr.expand", "x", "expand folded events").on(KeyCode::Char('x')),
];

// ------------------------------------------------------------------ routing

/// What a list is a list of, kept apart from the [`PrQuery`] it becomes so
/// the header can say `ShaxP/shax` rather than `repo:ShaxP/shax is:open`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    Repo(RepoRef),
    Query(String),
}

impl Scope {
    fn query(&self) -> PrQuery {
        match self {
            Self::Repo(r) => PrQuery::repo(r),
            Self::Query(q) => PrQuery::search(q.clone()),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Repo(r) => r.to_string(),
            Self::Query(q) => q.clone(),
        }
    }
}

/// What the route asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    List(Scope),
    Detail(SubjectRef),
}

/// Read the route's argument, per the table in the module docs.
///
/// Nothing here can fail: a string that is not a coordinate and not a
/// repository is a search, because GitHub search accepts anything and a
/// route that refused to open would have to draw an error screen for a
/// typo it could instead just search for.
fn parse_arg(arg: Option<&str>) -> Target {
    let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
        return Target::List(Scope::Query(DEFAULT_QUERY.to_owned()));
    };
    if let Some(q) = arg.strip_prefix("q=") {
        return Target::List(Scope::Query(q.trim().to_owned()));
    }
    if let Some(r) = SubjectRef::parse_numbered(arg, SubjectKind::PullRequest) {
        return Target::Detail(r);
    }
    if let Some(repo) = RepoRef::parse(arg).filter(|_| !arg.contains(char::is_whitespace)) {
        return Target::List(Scope::Repo(repo));
    }
    Target::List(Scope::Query(arg.to_owned()))
}

// ------------------------------------------------------------------ the surface

#[derive(Debug)]
pub struct PullRequests {
    view: View,
}

#[derive(Debug)]
/// Both boxed: each carries a page or a whole `PrDetail`, and an enum is
/// sized to its largest variant.
enum View {
    List(Box<ListView>),
    Detail(Box<DetailView>),
}

impl PullRequests {
    /// Build for a route argument. `None` is `pr`.
    pub fn from_arg(arg: Option<&str>) -> Self {
        let view = match parse_arg(arg) {
            Target::List(scope) => View::List(Box::new(ListView::new(scope))),
            Target::Detail(subject) => View::Detail(Box::new(DetailView::new(subject))),
        };
        Self { view }
    }

    fn target(&self) -> RefreshTarget {
        match &self.view {
            View::List(l) => RefreshTarget::PullRequests(l.query.clone()),
            View::Detail(d) => RefreshTarget::PullRequest(d.subject.clone()),
        }
    }
}

#[async_trait]
impl Surface for PullRequests {
    fn title(&self) -> String {
        match &self.view {
            View::List(l) => l.title(),
            View::Detail(d) => d.title(),
        }
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        match &mut self.view {
            View::List(l) => l.render(f, area, ctx),
            View::Detail(d) => d.render(f, area, ctx),
        }
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Outcome {
        match &mut self.view {
            View::List(l) => l.on_key(key),
            View::Detail(d) => d.on_key(key),
        }
    }

    /// A failed read is kept and drawn, never propagated: every arm of §8 is
    /// a screen this surface owns, and `App` applies `?` to this.
    async fn load(&mut self, ctx: &Ctx) -> Result<()> {
        match &mut self.view {
            View::List(l) => l.load(ctx).await,
            View::Detail(d) => d.load(ctx).await,
        }
        Ok(())
    }

    fn browser_url(&self) -> Option<String> {
        match &self.view {
            View::List(l) => l.focused().and_then(|pr| pr.subject_ref().browser_url()),
            View::Detail(d) => d.subject.browser_url(),
        }
    }

    fn cares_about(&self, ev: &StoreEvent) -> bool {
        ev.is_global() || ev.target() == Some(&self.target())
    }

    fn keymap(&self) -> &[Binding] {
        match &self.view {
            View::List(_) => LIST_BINDINGS,
            View::Detail(_) => DETAIL_BINDINGS,
        }
    }

    fn freshness(&self) -> Option<Freshness> {
        match &self.view {
            View::List(l) => l.page.as_ref().map(Freshness::of),
            View::Detail(d) => d.detail.as_ref().map(Freshness::of),
        }
    }

    fn refresh_target(&self) -> Option<RefreshTarget> {
        Some(self.target())
    }

    fn on_enter(&mut self, ctx: &Ctx) {
        ctx.store.refresh(self.target());
    }

    fn on_leave(&mut self, ctx: &Ctx) {
        ctx.store.cancel(&self.target());
    }
}

// ------------------------------------------------------------------ the list

#[derive(Debug)]
struct ListView {
    scope: Scope,
    query: PrQuery,
    /// The last successful read, kept across a failed one so offline shows a
    /// banner over rows rather than replacing them.
    page: Option<Fresh<Page<PullRequest>>>,
    error: Option<StoreError>,
    cursor: usize,
    list: ListState,
    /// Rows visible at the last draw, for half-page moves.
    viewport: u16,
}

impl ListView {
    fn new(scope: Scope) -> Self {
        Self {
            query: scope.query(),
            scope,
            page: None,
            error: None,
            cursor: 0,
            list: ListState::default(),
            viewport: 0,
        }
    }

    fn items(&self) -> &[PullRequest] {
        self.page
            .as_ref()
            .map(|p| p.value.items.as_slice())
            .unwrap_or_default()
    }

    fn focused(&self) -> Option<&PullRequest> {
        self.items().get(self.cursor)
    }

    fn state(&self) -> SurfaceState {
        match &self.page {
            Some(page) => {
                classify(&Conditions::from_fresh(page, page.value.len()).error(self.error.as_ref()))
            }
            None => match &self.error {
                Some(err) => classify(&Conditions::default().error(Some(err))),
                None => SurfaceState::Cold,
            },
        }
    }

    /// The one repository every row shares, when the widget will have
    /// elided it from the rows (`RowList`) — the header names it instead.
    fn shared_repo(&self) -> Option<String> {
        let items = self.items();
        if items.len() < 2 || matches!(self.scope, Scope::Repo(_)) {
            return None;
        }
        let first = &items[0].repo;
        items
            .iter()
            .all(|p| &p.repo == first)
            .then(|| first.to_string())
    }

    fn title(&self) -> String {
        let mut t = String::from("Pull requests");
        if let Some(page) = &self.page
            && page.fetched_at.is_some()
        {
            let n = page.value.len();
            match page.value.total {
                Some(total) if total as usize > n => t.push_str(&format!("  {n} of {total}")),
                _ => t.push_str(&format!("  {n}")),
            }
        }
        t.push_str(&format!(" · {}", self.scope.label()));
        if let Some(repo) = self.shared_repo() {
            t.push_str(&format!(" · {repo}"));
        }
        t
    }

    fn move_cursor(&mut self, delta: isize) {
        let n = self.items().len();
        if n == 0 {
            self.cursor = 0;
            return;
        }
        self.cursor = (self.cursor as isize + delta).clamp(0, n as isize - 1) as usize;
    }

    fn half_page(&self) -> isize {
        (self.viewport / 2).max(1) as isize
    }

    fn row_of(pr: &PullRequest, now: OffsetDateTime) -> Row {
        let (icon, cell) = status_cell(pr.display_status());
        let mut row = Row::new(pr.title.clone(), age::relative(pr.updated_at, now))
            .icon(icon)
            .unread(pr.review.awaits_me())
            .repo(pr.repo.to_string())
            .number(pr.number)
            .state(cell)
            .actor(Actor::display(pr.author.as_ref()).to_owned());
        if let Some(cell) = checks_cell(&pr.checks) {
            row = row.checks(cell);
        }
        row
    }

    /// One line about the focused row: what the columns could not hold.
    ///
    /// The review phrase and a conflict come first, right after the number:
    /// the line elides from the right, and a long repository name must not
    /// be what pushes "needs your review" off the screen.
    fn focus_line(&self, width: usize, icons: Icons, now: OffsetDateTime) -> Line<'static> {
        let Some(pr) = self.focused() else {
            return Line::raw("");
        };
        let mut parts = vec![format!("#{}", pr.number), review_phrase(pr).to_owned()];
        if pr.mergeable == Mergeable::Conflicting {
            parts.push("conflicts".to_owned());
        }
        parts.extend([
            pr.repo.to_string(),
            Actor::display(pr.author.as_ref()).to_owned(),
            churn(pr, icons),
            format!("{} {}", pr.changed_files, plural(pr.changed_files, "file")),
        ]);
        if pr.comment_count > 0 {
            parts.push(format!(
                "{} {}",
                pr.comment_count,
                plural(pr.comment_count, "comment")
            ));
        }
        parts.push(age::relative(pr.updated_at, now));
        Line::from(vec![
            Span::raw(" "),
            Span::styled(icons.cursor().to_owned(), Role::Accent.style()),
            Span::raw(" "),
            Span::styled(
                elide(
                    &parts.join(&format!(" {} ", icons.dot())),
                    width.saturating_sub(3),
                    icons,
                ),
                Role::Muted.style(),
            ),
        ])
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let focus_h = u16::from(area.height >= 6);
        let body_h = area.height.saturating_sub(focus_h);
        let body = Rect {
            height: body_h,
            ..area
        };
        let focus = Rect {
            y: area.y + body_h,
            height: focus_h,
            ..area
        };
        self.viewport = body_h;

        let icons = ctx.icons;
        let state = self.state();
        let detail = format!("Nothing matches {}.", self.query.effective());
        let view = StateView::new(&state)
            .icons(icons)
            .retry("r", "try again")
            .empty(EmptyCopy {
                headline: "No pull requests",
                detail: &detail,
                action: Some(("r", "check again")),
            });
        if let Some(rows_area) = view.render(f, body).rows() {
            let rows: Vec<Row> = self
                .items()
                .iter()
                .map(|pr| Self::row_of(pr, ctx.now))
                .collect();
            self.list.select(Some(self.cursor));
            // The widget drops the repository column when every row shares
            // one (§9); the header names it instead — in the scope for a
            // repository list, and via `shared_repo` for a search whose
            // results happen to agree.
            RowList::new(&rows)
                .icons(icons)
                .viewer(&ctx.viewer().login)
                .render(f, rows_area, &mut self.list);
        }
        if focus_h > 0 && state.has_content() {
            f.render_widget(
                Paragraph::new(self.focus_line(area.width as usize, icons, ctx.now)),
                focus,
            );
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Char('d'), true) => {
                self.move_cursor(self.half_page());
                Outcome::Redraw
            }
            (KeyCode::Char('u'), true) => {
                self.move_cursor(-self.half_page());
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
            (KeyCode::Char('g'), false) | (KeyCode::Home, _) => {
                self.move_cursor(isize::MIN / 2);
                Outcome::Redraw
            }
            (KeyCode::Char('G'), false) | (KeyCode::End, _) => {
                self.move_cursor(isize::MAX / 2);
                Outcome::Redraw
            }
            (KeyCode::Enter, _) => match self.focused() {
                Some(pr) => Outcome::Push(Route {
                    surface: SurfaceId::PullRequests,
                    arg: Some(pr.subject_ref().to_string()),
                }),
                None => Outcome::Ignored,
            },
            _ => Outcome::Ignored,
        }
    }

    async fn load(&mut self, ctx: &Ctx) {
        match ctx.store.pull_requests(&self.query).await {
            Ok(page) => {
                self.page = Some(page);
                self.error = None;
            }
            Err(err) => self.error = Some(err),
        }
        self.move_cursor(0);
    }
}

// ------------------------------------------------------------------ the detail

#[derive(Debug)]
struct DetailView {
    subject: SubjectRef,
    detail: Option<Fresh<Option<PrDetail>>>,
    error: Option<StoreError>,
    /// First document line on screen.
    scroll: usize,
    /// Lines the body had at the last draw, for clamping.
    viewport: u16,
    /// Document lines at the last draw, for `G` and for clamping.
    lines: usize,
    /// Whether folded runs of low-signal events are shown in full.
    expanded: bool,
}

impl DetailView {
    fn new(subject: SubjectRef) -> Self {
        Self {
            subject,
            detail: None,
            error: None,
            scroll: 0,
            viewport: 0,
            lines: 0,
            expanded: false,
        }
    }

    fn opened(&self) -> Option<&PrDetail> {
        self.detail.as_ref().and_then(|d| d.value.as_ref())
    }

    fn state(&self) -> SurfaceState {
        match &self.detail {
            Some(fresh) => classify(
                &Conditions::from_fresh(fresh, usize::from(fresh.value.is_some()))
                    .error(self.error.as_ref()),
            ),
            None => match &self.error {
                Some(err) => classify(&Conditions::default().error(Some(err))),
                None => SurfaceState::Cold,
            },
        }
    }

    fn title(&self) -> String {
        let mut t = format!("Pull request  {}", self.subject);
        if let Some(d) = self.opened() {
            t.push_str(&format!(" · {}", d.pr.display_status().label()));
        }
        t
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.lines.saturating_sub(usize::from(self.viewport));
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    fn half_page(&self) -> isize {
        (self.viewport / 2).max(1) as isize
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let icons = ctx.icons;
        let state = self.state();
        let missing = format!(
            "GitHub has no pull request at {}, or this token cannot see it.",
            self.subject
        );
        let view = StateView::new(&state)
            .icons(icons)
            .retry("r", "try again")
            .empty(EmptyCopy {
                headline: "Not here",
                detail: &missing,
                action: Some(("r", "try again")),
            });
        let Some(body) = view.render(f, area).rows() else {
            return;
        };
        let Some(d) = self.opened() else {
            return;
        };
        let lines = document(d, body.width as usize, icons, self.expanded);
        self.lines = lines.len();
        self.viewport = body.height;
        self.scroll_by(0);
        f.render_widget(Paragraph::new(lines).scroll((self.scroll as u16, 0)), body);
    }

    fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Char('d'), true) => {
                self.scroll_by(self.half_page());
                Outcome::Redraw
            }
            (KeyCode::Char('u'), true) => {
                self.scroll_by(-self.half_page());
                Outcome::Redraw
            }
            (KeyCode::Char('j') | KeyCode::Down, false) => {
                self.scroll_by(1);
                Outcome::Redraw
            }
            (KeyCode::Char('k') | KeyCode::Up, false) => {
                self.scroll_by(-1);
                Outcome::Redraw
            }
            (KeyCode::Char('g'), false) | (KeyCode::Home, _) => {
                self.scroll = 0;
                Outcome::Redraw
            }
            (KeyCode::Char('G'), false) | (KeyCode::End, _) => {
                self.scroll_by(isize::MAX / 2);
                Outcome::Redraw
            }
            (KeyCode::Char('x'), false) => {
                self.expanded = !self.expanded;
                Outcome::Redraw
            }
            _ => Outcome::Ignored,
        }
    }

    async fn load(&mut self, ctx: &Ctx) {
        match ctx.store.pull_request(&self.subject).await {
            Ok(detail) => {
                self.detail = Some(detail);
                self.error = None;
            }
            Err(err) => self.error = Some(err),
        }
    }
}

// ------------------------------------------------------------------ cells

/// The status column, and the row icon that repeats it.
///
/// Duplicated from the inbox's `status_cell` rather than imported: a
/// surface never imports another (`30-ui.md` §3). That two surfaces now
/// carry it is the argument for moving it to `widgets` — filed as a request
/// to that path's owner rather than done from here.
fn status_cell(status: PrDisplayStatus) -> (Icon, Cell) {
    let (icon, role) = match status {
        PrDisplayStatus::Draft => (Icon::PrDraft, Role::Muted),
        PrDisplayStatus::Open => (Icon::PrOpen, Role::Success),
        PrDisplayStatus::Merged => (Icon::PrMerged, Role::Accent),
        PrDisplayStatus::Closed => (Icon::PrClosed, Role::Danger),
    };
    (icon, Cell::new(icon, status.label(), role))
}

/// `None` where there is no CI at all — different from everything skipped.
fn checks_cell(rollup: &CheckRollup) -> Option<Cell> {
    let total = rollup.passed + rollup.failed + rollup.pending + rollup.skipped;
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
            format!("{}/{total}", rollup.failed),
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

/// What the review state says to the viewer, in the viewer's terms.
fn review_phrase(pr: &PullRequest) -> &'static str {
    if pr.review.awaits_me() {
        return "needs your review";
    }
    match (pr.review.decision, pr.review.my_review) {
        (_, Some(ReviewState::ChangesRequested)) => "you requested changes",
        (_, Some(ReviewState::Approved)) => "you approved",
        (Some(ReviewDecision::Approved), _) => "approved",
        (Some(ReviewDecision::ChangesRequested), _) => "changes requested",
        (Some(ReviewDecision::ReviewRequired), _) => "review required",
        (None, _) => "no review",
    }
}

/// `+312 −48`, with a real minus where the font has one.
fn churn(pr: &PullRequest, icons: Icons) -> String {
    let minus = if icons.unicode() { "−" } else { "-" };
    format!("+{} {minus}{}", pr.additions, pr.deletions)
}

/// `head → base`, degrading with the icon set like every other glyph.
fn branches(d: &PrDetail, icons: Icons) -> String {
    let arrow = if icons.unicode() { "→" } else { "->" };
    format!("{} {arrow} {}", d.head_ref, d.base_ref)
}

fn plural(n: u32, word: &str) -> String {
    if n == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

// ------------------------------------------------------------------ the document

/// Absolute, per §9: a detail is read, not scanned.
fn stamp(at: OffsetDateTime) -> String {
    at.format(format_description!("[year]-[month]-[day] [hour]:[minute]"))
        .unwrap_or_default()
}

fn short(oid: &str) -> &str {
    &oid[..oid.len().min(7)]
}

/// `AddedToProjectEvent` → `added to project`.
///
/// For the sixty-odd kinds `TimelineKind` does not model: a line that says
/// what happened in the schema's own words, lower-cased, beats a line that
/// vanishes (`10-domain-model.md` §3.4).
fn humanize(kind: &str) -> String {
    let base = kind.strip_suffix("Event").unwrap_or(kind);
    let mut out = String::with_capacity(base.len() + 4);
    for (i, c) in base.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// Build the scrolling document. Pure: the same detail and width give the
/// same lines, which is what makes it snapshot-testable at every width.
fn document(d: &PrDetail, width: usize, icons: Icons, expanded: bool) -> Vec<Line<'static>> {
    let mut doc = Doc {
        lines: Vec::new(),
        width: width.max(1),
        icons,
    };
    doc.header(d);
    doc.blank();
    doc.checks(&d.pr.checks);
    doc.body(d);
    doc.timeline(&d.timeline, expanded);
    doc.lines
}

struct Doc {
    lines: Vec<Line<'static>>,
    width: usize,
    icons: Icons,
}

impl Doc {
    fn blank(&mut self) {
        self.lines.push(Line::raw(""));
    }

    fn push(&mut self, spans: Vec<Span<'static>>) {
        self.lines.push(Line::from(spans));
    }

    /// Word-wrap `text` into the column, each line under `indent` cells.
    fn wrapped(&mut self, text: &str, indent: usize, style: Style) {
        let pad = " ".repeat(indent);
        for line in wrap(text, self.width.saturating_sub(indent).max(1)) {
            self.push(vec![Span::raw(pad.clone()), Span::styled(line, style)]);
        }
    }

    /// A one-line row with a leading glyph, elided to the column.
    fn glyph_line(&mut self, icon: Icon, role: Role, text: String, style: Style) {
        let g = self.icons.get(icon).to_owned();
        let room = self.width.saturating_sub(cells(&g) + 1);
        self.push(vec![
            Span::styled(g, role.style()),
            Span::raw(" "),
            Span::styled(elide(&text, room, self.icons), style),
        ]);
    }

    fn header(&mut self, d: &PrDetail) {
        let pr = &d.pr;
        let bold = Role::Default.style().add_modifier(Modifier::BOLD);
        self.wrapped(&pr.title, 0, bold);

        let (icon, cell) = status_cell(pr.display_status());
        let meta = [
            format!("{}#{}", pr.repo, pr.number),
            Actor::display(pr.author.as_ref()).to_owned(),
            branches(d, self.icons),
            churn(pr, self.icons),
            format!("{} {}", pr.changed_files, plural(pr.changed_files, "file")),
        ]
        .join(&format!(" {} ", self.icons.dot()));
        let g = self.icons.get(icon).to_owned();
        let room = self
            .width
            .saturating_sub(cells(&g) + 1 + cells(&cell.text) + 1);
        self.push(vec![
            Span::styled(g, cell.role.style()),
            Span::raw(" "),
            Span::styled(cell.text.clone(), cell.role.style()),
            Span::raw(" "),
            Span::styled(elide(&meta, room, self.icons), Role::Muted.style()),
        ]);

        // Review, in the viewer's terms, then who has said what.
        let mut review = vec![Span::styled(
            review_phrase(pr).to_owned(),
            if pr.review.awaits_me() {
                Role::Accent.style().add_modifier(Modifier::BOLD)
            } else {
                Role::Muted.style()
            },
        )];
        for (who, state) in &pr.review.reviewers {
            review.push(Span::styled(
                format!(" {} ", self.icons.dot()),
                Role::Muted.style(),
            ));
            review.push(Span::styled(
                format!("{} {}", who.login, review_word(*state)),
                review_role(*state).style(),
            ));
        }
        if pr.mergeable == Mergeable::Conflicting {
            review.push(Span::styled(
                format!(" {} ", self.icons.dot()),
                Role::Muted.style(),
            ));
            review.push(Span::styled(
                "conflicts with base".to_owned(),
                Role::Danger.style(),
            ));
        }
        self.push(review);

        if !pr.labels.is_empty() {
            let mut spans = Vec::with_capacity(pr.labels.len() * 2);
            for l in &pr.labels {
                spans.push(label_span(l));
                spans.push(Span::raw(" "));
            }
            self.push(spans);
        }
    }

    fn checks(&mut self, rollup: &CheckRollup) {
        if rollup.state == RollupState::None {
            return;
        }
        let mut summary = Vec::new();
        for (n, word) in [
            (rollup.failed, "failed"),
            (rollup.pending, "pending"),
            (rollup.passed, "passed"),
            (rollup.skipped, "skipped"),
        ] {
            if n > 0 {
                summary.push(format!("{n} {word}"));
            }
        }
        let head = format!(
            "Checks  {}",
            summary.join(&format!(" {} ", self.icons.dot()))
        );
        self.push(vec![Span::styled(
            head,
            Role::Default.style().add_modifier(Modifier::BOLD),
        )]);
        for run in &rollup.runs {
            let (icon, role, word) = run_verdict(run);
            let text = format!("{}  {word}", run.name);
            let g = self.icons.get(icon).to_owned();
            let room = self.width.saturating_sub(2 + cells(&g) + 1);
            self.push(vec![
                Span::raw("  "),
                Span::styled(g, role.style()),
                Span::raw(" "),
                Span::styled(elide(&text, room, self.icons), Role::Default.style()),
            ]);
        }
        // The rollup merges check runs with legacy commit statuses
        // (`10-domain-model.md` §3.3), but only the runs travel as a list.
        // A summary of "2 failed" over three runs with one failure would
        // otherwise read as a bug in the arithmetic.
        let total = usize::from(rollup.failed + rollup.pending + rollup.passed + rollup.skipped);
        let unlisted = total.saturating_sub(rollup.runs.len());
        if unlisted > 0 {
            self.push(vec![Span::styled(
                format!(
                    "  and {unlisted} commit {} not listed here",
                    if unlisted == 1 { "status" } else { "statuses" }
                ),
                Role::Muted.style(),
            )]);
        }
        self.blank();
    }

    fn body(&mut self, d: &PrDetail) {
        if d.body.is_empty() {
            self.push(vec![Span::styled(
                "No description.".to_owned(),
                Role::Muted.style(),
            )]);
        } else {
            self.markdown(&d.body.source, 0);
        }
        self.blank();
    }

    /// The source, wrapped. Paragraph breaks survive; nothing else is
    /// interpreted until M2.4.
    fn markdown(&mut self, source: &str, indent: usize) {
        for line in source.lines() {
            if line.trim().is_empty() {
                self.blank();
            } else {
                self.wrapped(line, indent, Role::Default.style());
            }
        }
    }

    fn timeline(&mut self, events: &[TimelineEvent], expanded: bool) {
        if events.is_empty() {
            self.push(vec![Span::styled(
                "Nothing has happened here yet.".to_owned(),
                Role::Muted.style(),
            )]);
            return;
        }
        let entries = if expanded {
            events.iter().map(TimelineEntry::Event).collect()
        } else {
            fold(events, FOLD_MIN_RUN)
        };
        for entry in entries {
            match entry {
                TimelineEntry::Event(e) => self.event(e),
                TimelineEntry::Folded { events } => {
                    let n = events.len();
                    self.glyph_line(
                        Icon::Info,
                        Role::Muted,
                        format!("{n} more {} — x to expand", plural(n as u32, "event")),
                        Role::Muted.style(),
                    );
                }
            }
        }
    }

    /// `who did what · when`, then whatever prose the event carries.
    fn event(&mut self, e: &TimelineEvent) {
        let who = Actor::display(e.actor.as_ref()).to_owned();
        let when = stamp(e.at);
        let dot = format!(" {} ", self.icons.dot());
        let headline = |verb: String| format!("{who} {verb}{dot}{when}");
        let muted = Role::Muted.style();

        match &e.kind {
            TimelineKind::Comment {
                body,
                reactions,
                edited,
            } => {
                let mut verb = String::from("commented");
                if *edited {
                    verb.push_str(" (edited)");
                }
                let n = reactions.total();
                if n > 0 {
                    verb.push_str(&format!("{dot}{n} {}", plural(u32::from(n), "reaction")));
                }
                self.glyph_line(Icon::Comment, Role::Accent, headline(verb), muted);
                self.markdown(&body.source, 2);
                self.blank();
            }
            TimelineKind::Review {
                state,
                body,
                threads,
            } => {
                self.glyph_line(
                    Icon::Review,
                    review_role(*state),
                    headline(review_word(*state).to_owned()),
                    muted,
                );
                if let Some(b) = body.as_ref().filter(|b| !b.is_empty()) {
                    self.markdown(&b.source, 2);
                }
                for t in threads {
                    self.thread(t);
                }
                self.blank();
            }
            TimelineKind::ReviewThread(t) => {
                self.thread(t);
                self.blank();
            }
            TimelineKind::Commit {
                oid,
                message_headline,
                authored_by,
            } => {
                let by = Actor::display(authored_by.as_ref());
                self.glyph_line(
                    Icon::Commit,
                    Role::Muted,
                    format!("{} {message_headline}{dot}{by}", short(oid)),
                    muted,
                );
            }
            TimelineKind::Merged { commit, base } => {
                let verb = match commit {
                    Some(c) => format!("merged {} into {base}", short(c)),
                    None => format!("merged into {base}"),
                };
                self.glyph_line(Icon::PrMerged, Role::Accent, headline(verb), muted);
                self.blank();
            }
            TimelineKind::Closed { by_commit } => {
                let verb = match by_commit {
                    Some(c) => format!("closed this via {}", short(c)),
                    None => "closed this".to_owned(),
                };
                self.glyph_line(Icon::PrClosed, Role::Danger, headline(verb), muted);
                self.blank();
            }
            TimelineKind::Reopened => {
                self.glyph_line(
                    Icon::PrOpen,
                    Role::Success,
                    headline("reopened this".into()),
                    muted,
                );
                self.blank();
            }
            TimelineKind::ReadyForReview => {
                self.glyph_line(
                    Icon::PrOpen,
                    Role::Success,
                    headline("marked this ready for review".into()),
                    muted,
                );
                self.blank();
            }
            TimelineKind::ConvertedToDraft => {
                self.glyph_line(
                    Icon::PrDraft,
                    Role::Muted,
                    headline("converted this to a draft".into()),
                    muted,
                );
                self.blank();
            }
            TimelineKind::Renamed { from, to } => {
                self.glyph_line(
                    Icon::Info,
                    Role::Muted,
                    headline(format!("renamed this from “{from}” to “{to}”")),
                    muted,
                );
            }
            TimelineKind::Labeled { label } => {
                self.glyph_line(
                    Icon::Info,
                    Role::Muted,
                    headline(format!("added the {} label", label.name)),
                    muted,
                );
            }
            TimelineKind::Unlabeled { label } => {
                self.glyph_line(
                    Icon::Info,
                    Role::Muted,
                    headline(format!("removed the {} label", label.name)),
                    muted,
                );
            }
            TimelineKind::Assigned { who: w } => {
                self.glyph_line(
                    Icon::Actor,
                    Role::Muted,
                    headline(format!("assigned {}", w.login)),
                    muted,
                );
            }
            TimelineKind::Unassigned { who: w } => {
                self.glyph_line(
                    Icon::Actor,
                    Role::Muted,
                    headline(format!("unassigned {}", w.login)),
                    muted,
                );
            }
            TimelineKind::ReviewRequested { who: w } => {
                self.glyph_line(
                    Icon::Review,
                    Role::Muted,
                    headline(format!("requested a review from {}", w.login)),
                    muted,
                );
            }
            TimelineKind::ReviewRequestRemoved { who: w } => {
                self.glyph_line(
                    Icon::Review,
                    Role::Muted,
                    headline(format!("removed the review request for {}", w.login)),
                    muted,
                );
            }
            TimelineKind::CrossReferenced { source, will_close } => {
                let verb = if *will_close {
                    format!("referenced this from {source}, which will close it")
                } else {
                    format!("referenced this from {source}")
                };
                self.glyph_line(Icon::Info, Role::Muted, headline(verb), muted);
            }
            TimelineKind::HeadRefForcePushed { before, after } => {
                self.glyph_line(
                    Icon::Commit,
                    Role::Warning,
                    headline(format!(
                        "force-pushed {} {} {}",
                        short(before),
                        if self.icons.unicode() { "→" } else { "->" },
                        short(after)
                    )),
                    muted,
                );
            }
            TimelineKind::Other { kind } => {
                self.glyph_line(Icon::Info, Role::Muted, headline(humanize(kind)), muted);
            }
        }
    }

    fn thread(&mut self, t: &ReviewThread) {
        let mut place = t.path.clone();
        if let Some(l) = t.line {
            place.push_str(&format!(":{l}"));
        }
        let mut notes = Vec::new();
        if t.is_resolved {
            notes.push("resolved");
        }
        if t.is_outdated {
            notes.push("outdated");
        }
        if !notes.is_empty() {
            place.push_str(&format!(" ({})", notes.join(", ")));
        }
        self.wrapped(
            &place,
            2,
            if t.is_resolved {
                Role::Muted.style()
            } else {
                Role::Default.style().add_modifier(Modifier::BOLD)
            },
        );
        for c in &t.comments {
            let who = Actor::display(c.author.as_ref());
            self.wrapped(
                &format!("{who}: {}", c.body.source),
                4,
                if t.is_resolved {
                    Role::Muted.style()
                } else {
                    Role::Default.style()
                },
            );
        }
    }
}

fn review_word(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Pending => "has a pending review",
        ReviewState::Commented => "commented",
        ReviewState::Approved => "approved",
        ReviewState::ChangesRequested => "requested changes",
        ReviewState::Dismissed => "review dismissed",
    }
}

fn review_role(state: ReviewState) -> Role {
    match state {
        ReviewState::Approved => Role::Success,
        ReviewState::ChangesRequested => Role::Danger,
        ReviewState::Pending | ReviewState::Commented | ReviewState::Dismissed => Role::Muted,
    }
}

/// Glyph, colour and word for one run: three encodings, as everywhere.
fn run_verdict(run: &CheckRun) -> (Icon, Role, &'static str) {
    match (run.status, run.conclusion) {
        (CheckStatus::Completed, Some(c)) => match c {
            CheckConclusion::Success => (Icon::CheckPass, Role::Success, "success"),
            CheckConclusion::Failure => (Icon::CheckFail, Role::Danger, "failure"),
            CheckConclusion::Cancelled => (Icon::CheckFail, Role::Danger, "cancelled"),
            CheckConclusion::TimedOut => (Icon::CheckFail, Role::Danger, "timed out"),
            CheckConclusion::StartupFailure => (Icon::CheckFail, Role::Danger, "startup failure"),
            CheckConclusion::ActionRequired => (Icon::CheckFail, Role::Danger, "action required"),
            CheckConclusion::Neutral => (Icon::CheckPending, Role::Muted, "neutral"),
            CheckConclusion::Skipped => (Icon::CheckPending, Role::Muted, "skipped"),
            CheckConclusion::Stale => (Icon::CheckPending, Role::Muted, "stale"),
        },
        (CheckStatus::InProgress, _) => (Icon::CheckPending, Role::Warning, "in progress"),
        (CheckStatus::Queued | CheckStatus::Requested, _) => {
            (Icon::CheckPending, Role::Warning, "queued")
        }
        (CheckStatus::Waiting | CheckStatus::Pending, _) => {
            (Icon::CheckPending, Role::Warning, "waiting")
        }
        // Completed with no conclusion is malformed; the rollup treats it as
        // pending and so does the line.
        (CheckStatus::Completed, None) => (Icon::CheckPending, Role::Warning, "pending"),
    }
}

/// A label in its own colour — the one place GitHub dictates one
/// (`10-domain-model.md` §3). The text colour is the model's decision, so a
/// dark label never carries dark text whatever the terminal theme.
fn label_span(l: &Label) -> Span<'static> {
    let bg = Color::Rgb(l.color.r, l.color.g, l.color.b);
    let fg = if l.color.prefers_light_text() {
        Color::White
    } else {
        Color::Black
    };
    Span::styled(format!(" {} ", l.name), Style::default().bg(bg).fg(fg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::test_support::{buffer, render};
    use omaghy_model::LimitKind;
    use omaghy_store::{
        FakeStore, Store,
        fake::{Behaviour, FIXTURE_NOW, pr_corpus},
    };
    use std::sync::Arc;
    use time::Duration;

    fn ctx(store: Arc<dyn Store>, icons: Icons) -> Ctx {
        Ctx::new(store, FIXTURE_NOW, icons)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// A surface for a route argument, entered and loaded as the router
    /// would.
    async fn opened(store: Arc<FakeStore>, arg: Option<&str>) -> (PullRequests, Ctx) {
        let c = ctx(store, Icons::UNICODE);
        let mut s = PullRequests::from_arg(arg);
        s.on_enter(&c);
        s.load(&c).await.unwrap();
        (s, c)
    }

    fn screen(s: &mut PullRequests, c: &Ctx, w: u16, h: u16) -> String {
        render(w, h, |f, a| s.render(f, a, c))
    }

    /// The coordinate of the corpus PR that needs the viewer's review and has
    /// the long timeline.
    fn needs_me() -> String {
        pr_corpus()[0].pr.subject_ref().to_string()
    }

    // ------------------------------------------------------------ routing

    #[test]
    fn the_route_argument_decides_the_view() {
        assert_eq!(
            parse_arg(None),
            Target::List(Scope::Query(DEFAULT_QUERY.into()))
        );
        assert_eq!(
            parse_arg(Some("q=is:open author:@me")),
            Target::List(Scope::Query("is:open author:@me".into()))
        );
        assert_eq!(
            parse_arg(Some("ShaxP/shax")),
            Target::List(Scope::Repo(RepoRef::new("ShaxP", "shax")))
        );
        assert_eq!(
            parse_arg(Some("ShaxP/shax#61")),
            Target::Detail(
                SubjectRef::parse_numbered("ShaxP/shax#61", SubjectKind::PullRequest).unwrap()
            )
        );
        // Not a coordinate, not a repository: a search, because GitHub search
        // takes anything and a typo is better searched for than refused.
        assert_eq!(
            parse_arg(Some("socket reconnect")),
            Target::List(Scope::Query("socket reconnect".into()))
        );
        assert_eq!(
            parse_arg(Some("  ")),
            Target::List(Scope::Query(DEFAULT_QUERY.into())),
            "blank is the default"
        );
    }

    #[test]
    fn a_repository_scope_is_named_plainly_in_the_title() {
        let s = PullRequests::from_arg(Some("ShaxP/shax"));
        assert!(s.title().ends_with("· ShaxP/shax"), "{}", s.title());
        let s = PullRequests::from_arg(Some("q=is:open author:@me"));
        assert!(s.title().ends_with("· is:open author:@me"), "{}", s.title());
    }

    // ------------------------------------------------------------ the list

    #[tokio::test]
    async fn the_default_list_is_what_needs_your_review_newest_first() {
        let (mut s, c) = opened(Arc::new(FakeStore::with_corpus()), None).await;
        let View::List(l) = &s.view else {
            panic!("a list")
        };
        assert_eq!(l.items().len(), 3);
        assert!(l.items().iter().all(|p| p.review.i_am_requested));
        let ages: Vec<_> = l.items().iter().map(|p| p.updated_at).collect();
        assert!(ages.windows(2).all(|w| w[0] >= w[1]));
        assert_eq!(s.title(), "Pull requests  3 · is:open review-requested:@me");

        let out = screen(&mut s, &c, 120, 12);
        assert!(out.contains("Add SocketServer reconnect backoff"), "{out}");
        assert!(out.contains("outfoxxed"), "the author column: {out}");
        assert!(out.contains("#1"), "the number column: {out}");
        // The newest row is requested from the viewer but already answered;
        // the focus line says so in the viewer's terms, and the next row is
        // the one still owed.
        assert!(
            out.contains("you requested changes"),
            "the focus line: {out}"
        );
        s.on_key(key(KeyCode::Char('j')), &c);
        let out = screen(&mut s, &c, 120, 12);
        assert!(out.contains("needs your review"), "the focus line: {out}");
    }

    #[tokio::test]
    async fn rows_that_need_you_are_bold_and_marked() {
        use ratatui::style::Modifier;
        // `pr:some-very-long…#7` is requested from the viewer but already
        // answered; `#1` is requested and unanswered. Same list, one bold.
        let (mut s, c) = opened(Arc::new(FakeStore::with_corpus()), None).await;
        let buf = buffer(120, 12, |f, a| s.render(f, a, &c));
        let View::List(l) = &s.view else {
            panic!("a list")
        };
        for (i, pr) in l.items().iter().enumerate() {
            let y = i as u16;
            let title_cell = (0..buf.area.width)
                .map(|x| &buf[(x, y)])
                .find(|cell| cell.symbol() == pr.title.chars().next().unwrap().to_string())
                .expect("the title is on its row");
            assert_eq!(
                title_cell.modifier.contains(Modifier::BOLD),
                pr.review.awaits_me(),
                "row {i} ({}) bold should follow awaits_me",
                pr.number
            );
        }
    }

    #[tokio::test]
    async fn the_cursor_moves_and_enter_opens_the_row_under_it() {
        let (mut s, c) = opened(Arc::new(FakeStore::with_corpus()), None).await;
        assert_eq!(s.on_key(key(KeyCode::Char('k')), &c), Outcome::Redraw);
        assert_eq!(s.on_key(key(KeyCode::Char('j')), &c), Outcome::Redraw);
        let View::List(l) = &s.view else {
            panic!("a list")
        };
        assert_eq!(l.cursor, 1);
        let second = l.items()[1].subject_ref().to_string();

        let out = s.on_key(key(KeyCode::Enter), &c);
        assert_eq!(
            out,
            Outcome::Push(Route {
                surface: SurfaceId::PullRequests,
                arg: Some(second.clone()),
            })
        );
        assert_eq!(
            s.browser_url().as_deref(),
            SubjectRef::parse_numbered(&second, SubjectKind::PullRequest)
                .unwrap()
                .browser_url()
                .as_deref(),
            "`o` opens the same row Enter would"
        );

        s.on_key(key(KeyCode::Char('G')), &c);
        s.on_key(key(KeyCode::Char('j')), &c);
        let View::List(l) = &s.view else {
            panic!("a list")
        };
        assert_eq!(l.cursor, 2, "G stops at the last row and j stays there");
        s.on_key(key(KeyCode::Char('g')), &c);
        let View::List(l) = &s.view else {
            panic!("a list")
        };
        assert_eq!(l.cursor, 0);
    }

    #[tokio::test]
    async fn half_page_moves_follow_the_viewport() {
        let (mut s, c) = opened(Arc::new(FakeStore::with_corpus()), Some("q=is:pr")).await;
        // Twelve rows in a body eight high (ten minus header/focus lines):
        // a half page is four.
        screen(&mut s, &c, 100, 10);
        s.on_key(ctrl('d'), &c);
        let View::List(l) = &s.view else {
            panic!("a list")
        };
        assert_eq!(l.cursor, l.half_page() as usize);
        s.on_key(ctrl('u'), &c);
        let View::List(l) = &s.view else {
            panic!("a list")
        };
        assert_eq!(l.cursor, 0);
    }

    #[tokio::test]
    async fn a_repository_list_keeps_its_rows_to_that_repository() {
        let (mut s, c) = opened(Arc::new(FakeStore::with_corpus()), Some("ShaxP/shax")).await;
        let View::List(l) = &s.view else {
            panic!("a list")
        };
        assert_eq!(l.items().len(), 2);
        assert!(l.items().iter().all(|p| p.repo.name == "shax"));
        let out = screen(&mut s, &c, 100, 10);
        assert!(out.contains("M7 slice 1"), "{out}");
        let rows: Vec<_> = out.lines().take(2).collect();
        assert!(
            rows.iter().all(|l| !l.contains("shax")),
            "the repository is in the header, not on every row: {out}"
        );
    }

    #[tokio::test]
    async fn entering_schedules_the_list_refresh_and_leaving_cancels_it() {
        let store = Arc::new(FakeStore::with_corpus());
        let (mut s, c) = opened(store.clone(), Some("q=is:open author:@me")).await;
        let target = RefreshTarget::PullRequests(PrQuery::search("is:open author:@me"));
        assert_eq!(store.scheduled(), vec![target.clone()]);
        assert_eq!(s.refresh_target(), Some(target.clone()));
        assert!(s.cares_about(&StoreEvent::Updated(target.clone())));
        assert!(!s.cares_about(&StoreEvent::Updated(RefreshTarget::Notifications)));
        s.on_leave(&c);
        assert!(store.scheduled().is_empty());
    }

    #[tokio::test]
    async fn an_empty_list_says_what_it_searched_for() {
        let (mut s, c) = opened(
            Arc::new(FakeStore::with_corpus()),
            Some("q=is:open label:nope"),
        )
        .await;
        let out = screen(&mut s, &c, 80, 10);
        assert!(out.contains("No pull requests"), "{out}");
        assert!(out.contains("label:nope"), "{out}");
        assert_eq!(s.browser_url(), None, "nothing under the cursor");
        assert_eq!(s.on_key(key(KeyCode::Enter), &c), Outcome::Ignored);
    }

    // ------------------------------------------------------------ the detail

    #[tokio::test]
    async fn a_detail_reads_top_to_bottom_in_review_order() {
        let (mut s, c) = opened(Arc::new(FakeStore::with_corpus()), Some(&needs_me())).await;
        assert_eq!(s.title(), "Pull request  quickshell/quickshell#1 · open");
        let out = screen(&mut s, &c, 100, 60);
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("`{needle}` missing from:\n{out}"))
        };
        // Title, meta, review line, labels, checks, body, timeline — in that
        // order, because that is the order a reviewer reads a PR in.
        let title = at("Add SocketServer reconnect backoff");
        let meta = at("socket-reconnect → master");
        let review = at("needs your review");
        let labels = at("area:core");
        let checks = at("Checks  2 failed");
        at("and 1 commit status not listed here");
        let body = at("Reconnects with exponential backoff");
        let first_event = at("outfoxxed");
        assert!(title < meta && meta < review && review < labels);
        assert!(labels < checks && checks < body);
        assert!(
            body < at("4 more events"),
            "the timeline follows the body, folded"
        );
        assert!(
            at("4 more events") < at("Cap the backoff at 30s"),
            "oldest first"
        );
        assert!(
            first_event < body,
            "the author is named in the meta line first"
        );
        assert!(
            out.contains("2026-09-10 08:00"),
            "absolute timestamps: {out}"
        );
        assert!(out.contains("+312 −48"), "{out}");
        assert!(out.contains("nixie commented"), "{out}");
        assert!(
            out.contains("src/io/SocketServer.cpp:142"),
            "a review thread names its place: {out}"
        );
    }

    #[tokio::test]
    async fn a_noisy_timeline_folds_and_x_unfolds_it() {
        let (mut s, c) = opened(Arc::new(FakeStore::with_corpus()), Some("rust-lang/rust#6")).await;
        let out = screen(&mut s, &c, 100, 80);
        assert!(out.contains("more events — x to expand"), "{out}");
        assert!(
            !out.contains("added the T-lang label"),
            "folded events are not drawn: {out}"
        );
        assert!(
            out.contains("proposed to merge"),
            "an unmodelled kind outside a fold still renders, humanised: {out}"
        );

        assert_eq!(s.on_key(key(KeyCode::Char('x')), &c), Outcome::Redraw);
        let out = screen(&mut s, &c, 100, 80);
        assert!(!out.contains("x to expand"), "{out}");
        assert!(out.contains("added the T-lang label"), "{out}");
        assert!(out.contains("added to project"), "{out}");
    }

    #[tokio::test]
    async fn the_detail_scrolls_and_stops_at_the_ends() {
        let (mut s, c) = opened(Arc::new(FakeStore::with_corpus()), Some(&needs_me())).await;
        let top = screen(&mut s, &c, 80, 8);
        assert!(top.contains("Add SocketServer"), "{top}");

        s.on_key(key(KeyCode::Char('j')), &c);
        let View::Detail(d) = &s.view else {
            panic!("a detail")
        };
        assert_eq!(d.scroll, 1);
        s.on_key(key(KeyCode::Char('G')), &c);
        let View::Detail(d) = &s.view else {
            panic!("a detail")
        };
        assert_eq!(
            d.scroll,
            d.lines - usize::from(d.viewport),
            "G lands on the last page"
        );
        s.on_key(key(KeyCode::Char('j')), &c);
        let View::Detail(d) = &s.view else {
            panic!("a detail")
        };
        assert_eq!(
            d.scroll,
            d.lines - usize::from(d.viewport),
            "and j stays there"
        );
        let bottom = screen(&mut s, &c, 80, 8);
        assert!(bottom.contains("CI is red on the new test"), "{bottom}");

        s.on_key(ctrl('u'), &c);
        s.on_key(key(KeyCode::Char('g')), &c);
        let View::Detail(d) = &s.view else {
            panic!("a detail")
        };
        assert_eq!(d.scroll, 0);
    }

    #[tokio::test]
    async fn a_detail_never_held_is_cold_and_one_gone_is_not_here() {
        let store = Arc::new(FakeStore::with_corpus());
        let (mut s, c) = opened(store.clone(), Some("ShaxP/shax#9999")).await;
        let View::Detail(d) = &s.view else {
            panic!("a detail")
        };
        // The fake answers `None` with a fetch stamp for a coordinate it does
        // not hold, which is the "GitHub has nothing here" screen.
        assert_eq!(d.state().name(), "empty");
        let out = screen(&mut s, &c, 80, 10);
        assert!(out.contains("Not here"), "{out}");
        assert!(out.contains("ShaxP/shax#9999"), "{out}");
        assert_eq!(
            s.browser_url().as_deref(),
            Some("https://github.com/ShaxP/shax/pull/9999"),
            "`o` still works: the coordinate is enough"
        );

        store.set_behaviour(Behaviour::offline_without_cache());
        s.load(&c).await.unwrap();
        let View::Detail(d) = &s.view else {
            panic!("a detail")
        };
        assert_eq!(d.state().name(), "cold");
    }

    #[tokio::test]
    async fn every_corpus_pull_request_opens_at_every_width() {
        // No panic, no empty frame, at widths from a phone to a wide monitor.
        let store = Arc::new(FakeStore::with_corpus());
        for d in pr_corpus() {
            let arg = d.pr.subject_ref().to_string();
            let (mut s, c) = opened(store.clone(), Some(&arg)).await;
            for w in [20, 40, 60, 80, 120, 200] {
                let out = screen(&mut s, &c, w, 30);
                assert!(!out.trim().is_empty(), "{arg} at {w} drew nothing");
                assert!(
                    out.lines().all(|l| cells(l) <= w as usize),
                    "{arg} at {w} overflows:\n{out}"
                );
            }
        }
    }

    #[test]
    fn unmodelled_kinds_are_humanised_not_dropped() {
        assert_eq!(humanize("AddedToProjectEvent"), "added to project");
        assert_eq!(humanize("MilestonedEvent"), "milestoned");
        assert_eq!(humanize("Weird"), "weird");
    }

    #[test]
    fn the_review_phrase_is_in_the_viewers_terms() {
        let mut pr = pr_corpus()[0].pr.clone();
        assert_eq!(review_phrase(&pr), "needs your review");
        pr.review.my_review = Some(ReviewState::Approved);
        assert_eq!(review_phrase(&pr), "you approved");
        pr.review.i_am_requested = false;
        pr.review.my_review = None;
        pr.review.decision = Some(ReviewDecision::ChangesRequested);
        assert_eq!(review_phrase(&pr), "changes requested");
        pr.review.decision = None;
        assert_eq!(review_phrase(&pr), "no review");
    }

    // ------------------------------------------------------------ snapshots

    struct Scene {
        name: &'static str,
        behaviour: Behaviour,
        warm: bool,
    }

    fn scene(name: &'static str, behaviour: Behaviour) -> Scene {
        Scene {
            name,
            behaviour,
            warm: false,
        }
    }

    fn failing(name: &'static str, err: StoreError) -> Scene {
        scene(name, Behaviour::failing(err))
    }

    fn scenes() -> Vec<Scene> {
        vec![
            scene("cold", Behaviour::offline_without_cache()),
            scene("populated", Behaviour::default()),
            scene(
                "empty",
                Behaviour {
                    empty: true,
                    ..Default::default()
                },
            ),
            scene("stale", Behaviour::offline_with_cache()),
            scene(
                "refreshing",
                Behaviour {
                    refreshing: true,
                    ..Default::default()
                },
            ),
            Scene {
                warm: true,
                ..failing(
                    "offline-with-cache",
                    StoreError::Offline("dns lookup failed".into()),
                )
            },
            failing(
                "offline-without-cache",
                StoreError::Offline("dns lookup failed".into()),
            ),
            failing(
                "rate-limited",
                StoreError::RateLimited {
                    kind: LimitKind::Primary,
                    at: FIXTURE_NOW + Duration::hours(1),
                },
            ),
            failing("forbidden", StoreError::Forbidden),
            failing(
                "error",
                StoreError::Upstream {
                    status: 502,
                    message: "bad gateway".into(),
                },
            ),
        ]
    }

    /// Every arm of §8 both views can be in, each produced by `FakeStore`.
    ///
    /// `filtered-empty` does not apply: neither view has a filter. `empty`
    /// on the detail is "GitHub has nothing at this coordinate".
    async fn state_matrix(icons: Icons, arg: Option<&str>, h: u16) -> String {
        let mut out = String::new();
        for s in scenes() {
            let store = Arc::new(FakeStore::with_corpus());
            let c = ctx(store.clone(), icons);
            let mut surface = PullRequests::from_arg(arg);
            if s.warm {
                surface.load(&c).await.unwrap();
            }
            store.set_behaviour(s.behaviour);
            surface.load(&c).await.unwrap();
            let state = match &surface.view {
                View::List(l) => l.state(),
                View::Detail(d) => d.state(),
            };
            assert!(
                s.name.starts_with(state.name()),
                "the scene `{}` actually produced `{}`",
                s.name,
                state.name()
            );
            out.push_str(&format!("── {} · {} ──\n", s.name, surface.title()));
            out.push_str(&screen(&mut surface, &c, 100, h));
            out.push_str("\n\n");
        }
        out
    }

    #[tokio::test]
    async fn the_list_state_matrix_in_unicode() {
        insta::assert_snapshot!(
            "pr_list_state_matrix_unicode",
            state_matrix(Icons::UNICODE, None, 8).await
        );
    }

    #[tokio::test]
    async fn the_list_state_matrix_in_ascii() {
        insta::assert_snapshot!(
            "pr_list_state_matrix_ascii",
            state_matrix(Icons::ASCII, None, 8).await
        );
    }

    #[tokio::test]
    async fn the_detail_state_matrix_in_unicode() {
        let arg = needs_me();
        insta::assert_snapshot!(
            "pr_detail_state_matrix_unicode",
            state_matrix(Icons::UNICODE, Some(&arg), 12).await
        );
    }

    #[tokio::test]
    async fn the_detail_state_matrix_in_ascii() {
        let arg = needs_me();
        insta::assert_snapshot!(
            "pr_detail_state_matrix_ascii",
            state_matrix(Icons::ASCII, Some(&arg), 12).await
        );
    }

    /// The whole corpus as a list, at each breakpoint of §4.1.
    #[tokio::test]
    async fn the_list_breakpoints() {
        const WIDTHS: [u16; 8] = [40, 59, 60, 79, 80, 99, 100, 120];
        let mut out = String::new();
        for icons in [Icons::UNICODE, Icons::ASCII] {
            for w in WIDTHS {
                let store = Arc::new(FakeStore::with_corpus());
                let c = ctx(store, icons);
                let mut s = PullRequests::from_arg(Some("q=is:pr"));
                s.load(&c).await.unwrap();
                out.push_str(&format!(
                    "── {w} cols · {} ──\n",
                    if icons.unicode() { "unicode" } else { "ascii" }
                ));
                out.push_str(&screen(&mut s, &c, w, 16));
                out.push_str("\n\n");
            }
        }
        insta::assert_snapshot!("pr_list_breakpoints", out);
    }

    /// The two details that exercise the most: the one that needs the
    /// viewer, and the long noisy one, folded and unfolded.
    #[tokio::test]
    async fn the_detail_documents() {
        let mut out = String::new();
        let store = Arc::new(FakeStore::with_corpus());
        for (arg, expanded) in [
            (needs_me(), false),
            ("rust-lang/rust#6".to_owned(), false),
            ("rust-lang/rust#6".to_owned(), true),
            ("basecamp/omarchy#2210".to_owned(), false),
            (
                "some-very-long-organization-name/an-equally-long-repository-name#7".to_owned(),
                false,
            ),
        ] {
            let c = ctx(store.clone(), Icons::UNICODE);
            let mut s = PullRequests::from_arg(Some(&arg));
            s.load(&c).await.unwrap();
            if expanded {
                s.on_key(key(KeyCode::Char('x')), &c);
            }
            out.push_str(&format!(
                "── {arg}{} ──\n",
                if expanded { " · expanded" } else { "" }
            ));
            out.push_str(&screen(&mut s, &c, 90, 60));
            out.push_str("\n\n");
        }
        insta::assert_snapshot!("pr_detail_documents", out);
    }
}

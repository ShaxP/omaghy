# omaghy — domain model

The vocabulary every other crate speaks. `omaghy-model` depends on nothing and
is the contract that lets crates be built in parallel.

Rust below is **illustrative**, not final — it fixes shape and intent, not
field-by-field syntax.

---

## 1. Principles

**1. We own the vocabulary; GitHub does not.** API shapes are a wire format to
be translated at the `omaghy-api` boundary. No GraphQL-generated type and no
`serde_json::Value` crosses into `omaghy-model`. If GitHub renames a field, one
crate changes.

**2. Flatten every union to a curated enum with an escape hatch.** Verified
against the live schema: `PullRequestTimelineItems` has **78** members,
`IssueTimelineItems` **51**. We will model perhaps fifteen. The rest collapse
into `Other { kind }` and render as a generic line. Never enumerate all 78 —
but never silently drop the remainder either, or the timeline lies about what
happened.

**3. `Option<T>` means "meaningfully absent", not "GraphQL said nullable".**
Almost every GraphQL field is nullable; propagating that gives a model where
everything is optional and nothing can be rendered without unwrapping. Policy:
absence that the UI must *represent* is `Option`; absence that indicates a
malformed response is a parse error at the boundary.

**4. Computed display state belongs in the model, not the view.** Per
`00-overview.md` §4.3, `omaghy-tui` renders and does not decide. Rollups,
merged draft/state, relative age bucketing, and review-state precedence are all
model concerns.

**5. Timestamps are UTC and typed.** `OffsetDateTime` throughout. Formatting is
a view concern; bucketing ("today", "this week") is a model concern.

---

## 2. Identity

Two id systems, and conflating them is a bug:

- **GraphQL node id** (`String`, e.g. `PR_kwDOA...`) — the primary key for
  every entity that has one. Stable, global, the cache key.
- **REST notification id** (`String`, e.g. `"24555446034"`) — notifications
  are a REST-only surface with their own id space, unrelated to node ids.

Plus a human coordinate that must be *derivable without a network call*, since
notification payloads carry only an API URL:

```rust
pub struct SubjectRef {           // parsed from e.g.
    pub owner: String,            //   https://api.github.com/repos/ShaxP/shax/pulls/61
    pub repo: String,
    pub number: u64,
    pub kind: SubjectKind,
}
```

`SubjectRef` is what `o` (open in browser) uses, and it is how an unenriched
notification still offers a useful action.

---

## 3. Core entities

```rust
pub struct Actor {                      // user, org, bot — one type
    pub login: String,
    pub node_id: Option<String>,        // absent for some bot/ghost actors
    pub avatar_url: Option<Url>,
    pub is_bot: bool,
}

pub struct Repo {
    pub node_id: String,
    pub owner: String,
    pub name: String,
    pub is_private: bool,
    pub description: Option<String>,
    pub default_branch: Option<String>,
}
impl Repo { pub fn full_name(&self) -> String { … } }

pub struct Label { pub name: String, pub color: Rgb, pub description: Option<String> }
```

`Label.color` is a real `Rgb`, not a hex string: labels are the one place
GitHub dictates colour, and rendering them against 22 Omarchy themes needs a
contrast decision the model should make once.

### 3.1 Pull requests and issues

```rust
pub enum PrState { Open, Closed, Merged }        // verified: no Draft variant

pub struct PullRequest {
    pub node_id: String,
    pub number: u64,
    pub repo: RepoRef,
    pub title: String,
    pub author: Option<Actor>,          // None = ghost (deleted account)
    pub state: PrState,
    pub is_draft: bool,
    pub mergeable: Mergeable,           // Mergeable | Conflicting | Unknown
    pub labels: Vec<Label>,
    pub review: ReviewSummary,
    pub checks: CheckRollup,
    pub comment_count: u32,
    pub additions: u32, pub deletions: u32, pub changed_files: u32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}
```

> **Trap:** draft is *not* a state. `PullRequestState` is `OPEN | CLOSED |
> MERGED` and `isDraft` is a separate boolean, so a draft PR is `Open` +
> `is_draft`. Every list renders the pair, never the state alone. The model
> therefore exposes a computed display status:

```rust
pub enum PrDisplayStatus { Draft, Open, Merged, Closed }
impl PullRequest { pub fn display_status(&self) -> PrDisplayStatus { … } }
```

`Issue` mirrors this with `IssueState { Open, Closed }` plus
`state_reason: Option<Completed | NotPlanned | Reopened | Duplicate>` — because
"closed as not planned" reads very differently from "closed".

### 3.2 Review state

```rust
pub enum ReviewState { Pending, Commented, Approved, ChangesRequested, Dismissed }

pub struct ReviewSummary {
    pub decision: Option<ReviewDecision>,   // Approved | ChangesRequested | ReviewRequired
    pub reviewers: Vec<(Actor, ReviewState)>,
    pub i_am_requested: bool,               // "does this need me" — the dashboard's core question
    pub my_review: Option<ReviewState>,
}
```

`i_am_requested` and `my_review` are viewer-relative and therefore cache keys
must include the viewer. A cache shared across accounts would silently answer
the wrong question.

### 3.3 CI status — two systems, one rollup

GitHub has **two independent CI systems** and a real client must merge them:

- legacy commit statuses — `StatusState`: `EXPECTED, ERROR, FAILURE, PENDING, SUCCESS`
- check runs — `CheckStatusState`: `REQUESTED, QUEUED, IN_PROGRESS, COMPLETED, WAITING, PENDING`
  with `CheckConclusionState`: `ACTION_REQUIRED, TIMED_OUT, CANCELLED, FAILURE, SUCCESS, NEUTRAL, SKIPPED, STARTUP_FAILURE, STALE`

A PR can carry both at once. The model resolves this once:

```rust
pub enum RollupState { Success, Failure, Pending, Neutral, None }

pub struct CheckRollup {
    pub state: RollupState,
    pub passed: u16, pub failed: u16, pub pending: u16, pub skipped: u16,
    pub runs: Vec<CheckRun>,        // empty in list contexts; populated in detail
}
```

Precedence, fixed here so every surface agrees: **any failure → `Failure`;
else any pending/in-progress → `Pending`; else any success → `Success`; else
neutral/skipped only → `Neutral`; else `None`.** `CANCELLED`, `TIMED_OUT`,
`STARTUP_FAILURE`, and `ACTION_REQUIRED` all count as failure — a cancelled
build is not a green one.

### 3.4 Timeline — the flattening

The heart of the model, and the reason for Rust.

```rust
pub struct TimelineEvent {
    pub node_id: Option<String>,
    pub actor: Option<Actor>,
    pub at: OffsetDateTime,
    pub kind: TimelineKind,
}

pub enum TimelineKind {
    Comment { body: Markdown, reactions: Reactions, edited: bool },
    Review { state: ReviewState, body: Option<Markdown>, threads: Vec<ReviewThread> },
    ReviewThread(ReviewThread),
    Commit { oid: String, message_headline: String, authored_by: Option<Actor> },
    Merged { commit: Option<String>, base: String },
    Closed { by_commit: Option<String> },
    Reopened,
    ReadyForReview,
    ConvertedToDraft,
    Renamed { from: String, to: String },
    Labeled { label: Label }, Unlabeled { label: Label },
    Assigned { who: Actor }, Unassigned { who: Actor },
    ReviewRequested { who: Actor }, ReviewRequestRemoved { who: Actor },
    CrossReferenced { source: SubjectRef, will_close: bool },
    HeadRefForcePushed { before: String, after: String },
    Other { kind: String },          // the other ~60
}
```

Adding a variant makes the compiler enumerate every rendering site. `Other`
carries the schema type name so an unmodelled event still renders as
"octocat added this to a project" rather than vanishing.

**Rendering rule:** consecutive `Other` and low-signal events (labels,
assignments, references) collapse into a single fold line — "*7 more events*",
expandable. Timelines on active PRs are dominated by noise, and a client that
renders all 78 kinds equally is unusable.

### 3.5 Notifications, and the enrichment problem

The REST notifications payload — verified against the live API — carries only:
`id`, `unread`, `reason`, `updated_at`, `last_read_at`, `subject{title, type,
url, latest_comment_url}`, `repository{…}`.

It does **not** carry: the number, the state, who acted, the comment body, CI
status, or a browser URL. Everything a good row wants needs a second pass. The
type system models that directly rather than hiding it:

```rust
pub enum Enrichment<T> {
    Absent,                    // never requested
    Pending,                   // in flight
    Failed { reason: String }, // 404 from a repo you lost access to, etc.
    Ready(T),
}

pub struct Notification {
    pub id: String,                   // REST id space
    pub unread: bool,
    pub reason: NotificationReason,
    pub updated_at: OffsetDateTime,
    pub title: String,
    pub kind: SubjectKind,
    pub repo: RepoRef,
    pub subject: Option<SubjectRef>,  // parsed from the API url; None if unparseable
    pub detail: Enrichment<SubjectDetail>,
}

pub struct SubjectDetail {          // one batched GraphQL query fills a whole page
    pub number: u64,
    pub status: PrDisplayStatus,
    pub checks: CheckRollup,
    pub last_actor: Option<Actor>,
    pub html_url: Url,
}
```

`Enrichment` is what makes the two-phase paint honest: a row renders from
`Absent`/`Pending` on the first frame and re-renders on `Ready` **without
changing height**. Reserving that space is a layout requirement, specified in
`30-ui.md`.

`NotificationReason` is a closed enum of the twelve documented values —
`ReviewRequested, Mention, TeamMention, Assign, Author, Comment, StateChange,
CiActivity, Subscribed, Manual, Invitation, SecurityAlert` — plus
`Other(String)`, since GitHub adds them without warning.

`SubjectKind`: `PullRequest, Issue, Discussion, Release, CheckSuite, Commit,
VulnerabilityAlert, Other(String)`.

### 3.6 Content

```rust
pub struct Markdown { pub source: String, pub blocks: Vec<Block> }
```

Parsed in `omaghy-model` (pulldown-cmark), never in the view. Blocks carry
already-resolved styling, including syntax-highlighted code spans, because the
view's job is to draw. GitHub-flavoured extensions — task lists, `@mentions`,
`#123` references, emoji shortcodes — are resolved here too, since none of them
survive a generic CommonMark parse.

Diffs (M2+):

```rust
pub struct FileDiff { pub path: String, pub change: ChangeKind, pub hunks: Vec<Hunk>,
                      pub additions: u32, pub deletions: u32, pub is_binary: bool }
pub struct Hunk { pub header: String, pub lines: Vec<DiffLine> }
pub struct DiffLine { pub kind: Added | Removed | Context, pub old_no: Option<u32>,
                      pub new_no: Option<u32>, pub spans: Vec<StyledSpan> }
```

Syntax highlighting resolves to `StyledSpan` in the model. The view never sees
a language name.

---

## 4. What the model deliberately omits

Projects/ProjectsV2, milestones beyond a title, Discussions beyond the
notification subject, wikis, packages, releases beyond a name, gists, and
anything Enterprise-only. Each would earn its place only alongside a surface
that renders it.

---

## 5. Cache implications

For `omaghy-cache` (specified separately):

- Node id is the primary key; notifications key on their REST id.
- **Every viewer-relative field taints its row** — `i_am_requested`,
  `my_review`, `unread` — so the cache is keyed by viewer login.
- `Enrichment` is persisted, including `Failed`, so a repo you lost access to
  is not re-fetched on every open.
- List membership (which PRs matched a query) is cached separately from the
  entities, so one PR updating does not invalidate a whole list.

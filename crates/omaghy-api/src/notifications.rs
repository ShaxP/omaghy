//! Notifications: the poll, the translation, and the second pass.
//!
//! This is the first module in omaghy that turns GitHub's JSON into
//! `omaghy-model` types, so it is where the boundary in
//! `spec/10-domain-model.md` §1 is actually enforced: the wire structs below
//! are private, and nothing but [`omaghy_model::Notification`] leaves.
//!
//! Three things make notifications their own module rather than a call site:
//!
//! - **Polling is only affordable because of the 304.** [`list`] always sends
//!   whatever validators the caller holds, and a `NotModified` answer costs no
//!   rate limit (`spec/20-store.md` §4). `X-Poll-Interval` lands in the
//!   governor on the way past, so the caller never has to parse a header.
//! - **The REST payload is half a row.** It carries no number, no state, no
//!   actor and no browser URL (`spec/10-domain-model.md` §3.5), so a useful
//!   inbox needs a second pass — [`enrich`], one GraphQL query for a whole
//!   page.
//! - **Enrichment must not loop.** `Enrichment::Failed` is terminal and
//!   `Pending` is in-flight, so only `Absent` rows are ever fetched. A repo
//!   you lost access to is asked about once, ever.
//!
//! [`list`]: Notifications::list
//! [`enrich`]: Notifications::enrich

use crate::client::{GitHubClient, RestRequest};
use crate::conditional::{Conditional, Validators};
use crate::graphql::GraphQlRequest;
use omaghy_model::{
    Enrichment, Notification, NotificationId, RepoRef, StoreError, SubjectDetail, SubjectId,
    SubjectKind,
};
use serde_json::{Map, Value};
use time::{Duration, OffsetDateTime};

/// GitHub's own ceiling on `per_page` for this endpoint. Asking for more is
/// silently clamped, which would make our paging arithmetic wrong.
pub const MAX_PER_PAGE: u8 = 50;

/// How many subjects one enrichment query resolves.
///
/// Equal to [`MAX_PER_PAGE`] on purpose: a page arrives from one REST request
/// and leaves in one GraphQL request, so enrichment never fans out.
pub const ENRICHMENT_BATCH: usize = MAX_PER_PAGE as usize;

// ---------------------------------------------------------------------------
// what a caller asks for
// ---------------------------------------------------------------------------

/// The REST parameters of a notifications poll.
///
/// Deliberately *not* `omaghy_store::NotificationQuery`: that one describes
/// what the viewer wants to see (reasons, kinds, free text) and is applied to
/// the cache. This one describes what GitHub is asked for, and GitHub offers
/// exactly these four knobs plus paging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationFilter {
    /// Include threads already read. GitHub's default is unread-only.
    pub all: bool,
    /// Only threads the viewer is directly participating in.
    pub participating: bool,
    /// Only threads updated after this instant.
    pub since: Option<OffsetDateTime>,
    /// Only threads updated before this instant.
    pub before: Option<OffsetDateTime>,
    /// Restrict to one repository — a different endpoint, not a query
    /// parameter.
    pub repo: Option<RepoRef>,
    pub per_page: u8,
    /// 1-based, as GitHub counts.
    pub page: u32,
}

impl Default for NotificationFilter {
    fn default() -> Self {
        Self {
            all: true,
            participating: false,
            since: None,
            before: None,
            repo: None,
            per_page: MAX_PER_PAGE,
            page: 1,
        }
    }
}

impl NotificationFilter {
    /// Unread only — GitHub's own default, spelled out.
    pub fn unread() -> Self {
        Self {
            all: false,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn in_repo(mut self, repo: RepoRef) -> Self {
        self.repo = Some(repo);
        self
    }

    #[must_use]
    pub fn per_page(mut self, per_page: u8) -> Self {
        self.per_page = per_page.clamp(1, MAX_PER_PAGE);
        self
    }

    #[must_use]
    pub fn page(mut self, page: u32) -> Self {
        self.page = page.max(1);
        self
    }

    #[must_use]
    pub fn since(mut self, since: OffsetDateTime) -> Self {
        self.since = Some(since);
        self
    }

    /// The path and query this filter fetches.
    ///
    /// Parameter order is fixed rather than incidental: a cassette matches on
    /// the exact path, so a reordering would silently stop replaying.
    pub fn path(&self) -> String {
        let base = match &self.repo {
            Some(r) => format!("/repos/{}/{}/notifications", r.owner, r.name),
            None => "/notifications".to_owned(),
        };
        let mut query = vec![
            format!("all={}", self.all),
            format!("per_page={}", self.per_page.clamp(1, MAX_PER_PAGE)),
        ];
        if self.participating {
            query.push("participating=true".to_owned());
        }
        if let Some(since) = self.since.and_then(format_rfc3339) {
            query.push(format!("since={since}"));
        }
        if let Some(before) = self.before.and_then(format_rfc3339) {
            query.push(format!("before={before}"));
        }
        if self.page > 1 {
            query.push(format!("page={}", self.page));
        }
        format!("{base}?{}", query.join("&"))
    }
}

fn format_rfc3339(t: OffsetDateTime) -> Option<String> {
    t.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

/// One page of the inbox, plus what the next poll needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPage {
    pub items: Vec<Notification>,
    /// Store these beside the page; the next poll sends them and hopes for a
    /// 304.
    pub validators: Validators,
    /// The `rel="next"` URL, when the inbox continues past this page.
    pub next_page: Option<String>,
    /// What GitHub asked for, floored by our own minimum. Already folded into
    /// the governor; repeated here so a caller scheduling the next poll does
    /// not have to reach for it.
    pub poll_interval: Duration,
}

// ---------------------------------------------------------------------------
// the handle
// ---------------------------------------------------------------------------

/// Notification operations on a [`GitHubClient`].
#[derive(Debug, Clone, Copy)]
pub struct Notifications<'a> {
    client: &'a GitHubClient,
}

impl GitHubClient {
    /// The notifications surface of the API.
    pub fn notifications(&self) -> Notifications<'_> {
        Notifications { client: self }
    }
}

impl<'a> Notifications<'a> {
    pub fn new(client: &'a GitHubClient) -> Self {
        Self { client }
    }

    /// Fetch a page of the inbox.
    ///
    /// `validators` is whatever was stored beside the last copy of this page.
    /// `Ok(NotModified)` means the cache is still correct and the request cost
    /// no rate limit — it is not an empty inbox, and a caller that confuses
    /// the two writes an empty list over good data.
    pub async fn list(
        &self,
        filter: &NotificationFilter,
        validators: Validators,
    ) -> Result<Conditional<NotificationPage>, StoreError> {
        let response = self
            .client
            .rest(RestRequest::get(filter.path()).conditional(validators))
            .await?;

        // Read after the request: `observe` has already folded
        // `X-Poll-Interval` into the governor, on a 304 as much as a 200.
        let poll_interval = self.client.rate_limits().effective_poll_interval();

        let response = match response {
            Conditional::NotModified { validators } => {
                return Ok(Conditional::NotModified { validators });
            }
            Conditional::Modified(r) => r,
        };

        let wire: Vec<wire::Notification> = response.json()?;
        Ok(Conditional::Modified(NotificationPage {
            items: wire
                .into_iter()
                .map(wire::Notification::into_model)
                .collect(),
            validators: response.validators.clone(),
            next_page: response.next_page(),
            poll_interval,
        }))
    }

    /// Fill in everything the REST payload left out, for a whole page at once.
    ///
    /// Only rows whose detail is `Absent` are fetched: `Pending` is already in
    /// flight and `Failed` is terminal (`spec/10-domain-model.md` §3.5). Those
    /// two rules are what stop a repository you lost access to from being
    /// re-asked on every open.
    ///
    /// Every selected row is left `Ready` or `Failed` when this returns
    /// `Ok`. When the request itself fails — offline, rate limited — the rows
    /// go back to `Absent` so the next open can try again, and the error is
    /// returned rather than written into each row.
    pub async fn enrich(&self, notifications: &mut [Notification]) -> Result<(), StoreError> {
        let wanted: Vec<usize> = notifications
            .iter()
            .enumerate()
            .filter(|(_, n)| n.detail.wants_fetch())
            .map(|(i, _)| i)
            .collect();
        if wanted.is_empty() {
            return Ok(());
        }

        // A subject with no number is not addressable by the query below, and
        // `SubjectDetail.number` could not hold its identity anyway. Recording
        // that as `Failed` rather than leaving it `Absent` is deliberate:
        // `Absent` means "ask again", and asking again for a commit will fail
        // in exactly the same way forever.
        let mut addressable: Vec<(usize, Target)> = Vec::new();
        for i in wanted {
            match Target::of(&notifications[i]) {
                Some(t) => addressable.push((i, t)),
                None => {
                    // Not a failure: a check suite has no number and GitHub
                    // sends it with `subject.url` null, so there is nothing to
                    // fetch and never will be. Recording it as `Failed` made an
                    // ordinary row render as broken — observed live before this
                    // was fixed.
                    notifications[i].detail = Enrichment::NotApplicable;
                }
            }
        }
        if addressable.is_empty() {
            return Ok(());
        }

        for chunk in addressable.chunks(ENRICHMENT_BATCH) {
            // Marked before the request, not after: a second caller reaching
            // this page while the query is in flight sees `Pending` and does
            // not schedule a duplicate.
            for (i, _) in chunk {
                notifications[*i].detail = Enrichment::Pending;
            }

            if let Err(e) = self.enrich_batch(notifications, chunk).await {
                // The failure is the request's, not the rows'. Leaving them
                // `Pending` would strand them: nothing would ever fetch them
                // again, because `Pending` does not want a fetch.
                for (i, _) in chunk {
                    notifications[*i].detail = Enrichment::Absent;
                }
                return Err(e);
            }
        }
        Ok(())
    }

    async fn enrich_batch(
        &self,
        notifications: &mut [Notification],
        chunk: &[(usize, Target)],
    ) -> Result<(), StoreError> {
        // Two notifications can name the same subject — a PR you are both the
        // author of and were mentioned in. One alias answers both.
        let mut aliases: Vec<Target> = Vec::new();
        let mut alias_of: Vec<usize> = Vec::with_capacity(chunk.len());
        for (_, target) in chunk {
            let at = aliases.iter().position(|t| t == target).unwrap_or_else(|| {
                aliases.push(target.clone());
                aliases.len() - 1
            });
            alias_of.push(at);
        }

        let request = build_query(&aliases);
        let mut answer = self
            .client
            .graphql_partial::<Map<String, Value>>(&request)
            .await?;

        let data = answer.data.take().unwrap_or_default();
        let resolved: Vec<Result<SubjectDetail, String>> = aliases
            .iter()
            .enumerate()
            .map(|(slot, target)| {
                let alias = alias_name(slot);
                if let Some(message) = answer.message_for(&alias) {
                    return Err(message);
                }
                match data.get(&alias) {
                    Some(Value::Null) | None => Err(MISSING.to_owned()),
                    Some(node) => wire::detail_from(node.clone(), target),
                }
            })
            .collect();

        for ((i, _), slot) in chunk.iter().zip(alias_of) {
            notifications[*i].detail = match &resolved[slot] {
                Ok(detail) => Enrichment::Ready(detail.clone()),
                Err(reason) => Enrichment::Failed {
                    reason: reason.clone(),
                },
            };
        }
        Ok(())
    }

    /// Mark threads read.
    ///
    /// Idempotent, as `spec/20-store.md` §5 requires: GitHub answers `205` for
    /// a thread that was already read, which the client reads as success. A
    /// thread that has since been deleted answers `404`, and that is treated
    /// as success too — a thread that does not exist cannot be unread, and
    /// failing the whole batch for it would leave the rest unmarked.
    ///
    /// One request per thread: GitHub offers no batch form short of
    /// `PUT /notifications`, which marks the entire inbox read.
    pub async fn mark_read(&self, ids: &[NotificationId]) -> Result<(), StoreError> {
        for id in ids {
            let path = format!("/notifications/threads/{}", id.0);
            match self.client.rest(RestRequest::patch(path)).await {
                Ok(_) | Err(StoreError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Mark threads unread — locally, because GitHub cannot.
    ///
    /// **This sends no request and always succeeds.** GitHub's REST API has no
    /// mark-as-unread: the documented thread verbs are `PATCH` (read) and
    /// `DELETE` (done). Verified against the live API rather than inferred —
    /// `PATCH /notifications/threads/{id}` with a body of `{"unread": true}`
    /// answers `205 Reset Content` and leaves the thread read.
    ///
    /// So the caller's optimistic cache write is the whole of the effect, and
    /// it will be overwritten by the next poll. The method exists so that
    /// `Store::mark_unread` has something honest to call, and so that this
    /// finding lives somewhere a reader will hit it; see the contract note in
    /// `spec/20-store.md` §5.
    pub async fn mark_unread(&self, ids: &[NotificationId]) -> Result<(), StoreError> {
        tracing::debug!(
            count = ids.len(),
            "marking unread is local only; GitHub's REST API offers no such verb"
        );
        Ok(())
    }
}

/// What is recorded when GitHub returned neither a node nor an error for an
/// alias. Should not happen; recorded rather than retried if it does.
const MISSING: &str = "GitHub returned no node for this subject";

// ---------------------------------------------------------------------------
// the batched query
// ---------------------------------------------------------------------------

/// A subject the enrichment query can address.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    owner: String,
    repo: String,
    number: u64,
    discussion: bool,
}

impl Target {
    fn of(n: &Notification) -> Option<Self> {
        let subject = n.subject.as_ref()?;
        let SubjectId::Number(number) = subject.id else {
            return None;
        };
        let discussion = match subject.kind {
            SubjectKind::PullRequest | SubjectKind::Issue => false,
            SubjectKind::Discussion => true,
            _ => return None,
        };
        Some(Self {
            owner: subject.owner.clone(),
            repo: subject.repo.clone(),
            number,
            discussion,
        })
    }
}

fn alias_name(slot: usize) -> String {
    format!("s{slot}")
}

/// Build one query for a batch of subjects.
///
/// Owner, name and number travel as **variables**, never interpolated: a
/// repository name is attacker-controlled as far as this process is
/// concerned, and a query string built by `format!` is an injection waiting to
/// happen. Only the aliases and the field name are generated.
fn build_query(targets: &[Target]) -> GraphQlRequest {
    let mut declarations = Vec::with_capacity(targets.len());
    let mut selections = Vec::with_capacity(targets.len());
    let mut variables = Map::new();

    for (slot, target) in targets.iter().enumerate() {
        let (o, r, n) = (format!("o{slot}"), format!("r{slot}"), format!("n{slot}"));
        declarations.push(format!("${o}: String!, ${r}: String!, ${n}: Int!"));

        let field = if target.discussion {
            format!("discussion(number: ${n}) {{ __typename ...DiscussionFields }}")
        } else {
            format!("issueOrPullRequest(number: ${n}) {{ __typename ...PrFields ...IssueFields }}")
        };
        selections.push(format!(
            "  {}: repository(owner: ${o}, name: ${r}) {{ {field} }}",
            alias_name(slot)
        ));

        variables.insert(o, Value::String(target.owner.clone()));
        variables.insert(r, Value::String(target.repo.clone()));
        variables.insert(n, Value::Number(target.number.into()));
    }

    // Only the fragments the selections actually reference: GraphQL rejects a
    // query that defines one it does not use, so the set follows the batch.
    let mut fragments = vec![COMMON_FRAGMENTS];
    if targets.iter().any(|t| !t.discussion) {
        fragments.push(ISSUE_OR_PR_FRAGMENTS);
    }
    if targets.iter().any(|t| t.discussion) {
        fragments.push(DISCUSSION_FRAGMENT);
    }

    let query = format!(
        "query Enrich({}) {{\n{}\n}}\n{}",
        declarations.join(", "),
        selections.join("\n"),
        fragments.join("\n"),
    );
    GraphQlRequest::query(query).variables(Value::Object(variables))
}

/// The per-subject selection, factored out so the query grows by one line per
/// notification rather than by fifteen.
///
/// `comments(last: 1)` and `commits(last: 1)` are the only connections here,
/// and both ask for a single node. GraphQL's cost is denominated in requested
/// nodes, so a full page of fifty stays at one point — measured against the
/// live API while recording `notifications_enrichment.json`.
const COMMON_FRAGMENTS: &str = "\
fragment Who on Actor { login avatarUrl __typename }
fragment Latest on Comment { author { ...Who } }";

const ISSUE_OR_PR_FRAGMENTS: &str = "\
fragment PrFields on PullRequest {
  number url state isDraft
  author { ...Who }
  comments(last: 1) { nodes { ...Latest } }
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
}
fragment IssueFields on Issue {
  number url state
  author { ...Who }
  comments(last: 1) { nodes { ...Latest } }
}";

const DISCUSSION_FRAGMENT: &str = "\
fragment DiscussionFields on Discussion {
  number url
  author { ...Who }
  comments(last: 1) { nodes { ...Latest } }
}";

// ---------------------------------------------------------------------------
// the wire, and the translation
// ---------------------------------------------------------------------------

/// GitHub's shapes. Private, and they stay that way: nothing in here crosses
/// into `omaghy-model` (`spec/10-domain-model.md` §1).
mod wire {
    use super::{Target, alias_missing};
    use omaghy_model::{
        Actor, CheckRollup, Enrichment, IssueDisplayStatus, IssueState, NotificationId,
        NotificationReason, PrDisplayStatus, PrState, RepoRef, RollupState, StatusState,
        SubjectDetail, SubjectKind, SubjectRef, SubjectStatus,
    };
    use serde::Deserialize;
    use serde_json::Value;
    use time::OffsetDateTime;

    #[derive(Debug, Deserialize)]
    pub(super) struct Notification {
        id: String,
        #[serde(default)]
        unread: bool,
        #[serde(default)]
        reason: String,
        #[serde(with = "time::serde::rfc3339")]
        updated_at: OffsetDateTime,
        subject: Subject,
        #[serde(default)]
        repository: Option<Repository>,
    }

    #[derive(Debug, Deserialize)]
    struct Subject {
        #[serde(default)]
        title: String,
        /// Null for a check suite, and for anything else GitHub cannot address
        /// by REST. The row still renders; it just cannot be opened.
        #[serde(default)]
        url: Option<String>,
        #[serde(rename = "type", default)]
        kind: String,
    }

    #[derive(Debug, Deserialize)]
    struct Repository {
        #[serde(default)]
        full_name: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        owner: Option<Owner>,
    }

    #[derive(Debug, Deserialize)]
    struct Owner {
        login: String,
    }

    impl Notification {
        pub(super) fn into_model(self) -> omaghy_model::Notification {
            let subject = self
                .subject
                .url
                .as_deref()
                .and_then(SubjectRef::from_api_url);

            omaghy_model::Notification {
                id: NotificationId(self.id),
                unread: self.unread,
                reason: reason(&self.reason),
                updated_at: self.updated_at,
                title: self.subject.title,
                kind: kind(&self.subject.kind),
                repo: self
                    .repository
                    .and_then(Repository::into_ref)
                    .unwrap_or_else(
                        // A notification with no repository is not a shape GitHub
                        // has ever produced, but a row that renders as `?/?` beats
                        // a page that fails to parse.
                        || RepoRef::new("?", "?"),
                    ),
                subject,
                detail: Enrichment::Absent,
            }
        }
    }

    impl Repository {
        fn into_ref(self) -> Option<RepoRef> {
            if let Some(parsed) = self.full_name.as_deref().and_then(RepoRef::parse) {
                return Some(parsed);
            }
            Some(RepoRef::new(self.owner?.login, self.name?))
        }
    }

    /// GitHub's twelve documented reasons, plus whatever it adds next.
    ///
    /// Written out rather than derived. `NotificationReason` carries
    /// `Other(String)`, and serde's default enum representation deserializes a
    /// newtype variant from a *map*, not a bare string — so a derived
    /// `Deserialize` would reject `"new_reason"` outright rather than keeping
    /// it. Verified against the live payload: `reason` is `"author"`,
    /// lower_snake_case.
    fn reason(raw: &str) -> NotificationReason {
        use NotificationReason as R;
        match raw {
            "review_requested" => R::ReviewRequested,
            "mention" => R::Mention,
            "team_mention" => R::TeamMention,
            "assign" => R::Assign,
            "author" => R::Author,
            "comment" => R::Comment,
            "state_change" => R::StateChange,
            "ci_activity" => R::CiActivity,
            "subscribed" => R::Subscribed,
            "manual" => R::Manual,
            "invitation" => R::Invitation,
            "security_alert" => R::SecurityAlert,
            other => R::Other(other.to_owned()),
        }
    }

    /// `subject.type`, which is **PascalCase** on the wire.
    ///
    /// Verified against the live payload: `"PullRequest"`, not
    /// `"pull_request"`. `SubjectKind`'s own
    /// `#[serde(rename_all = "snake_case")]` is the cache's representation and
    /// does not match GitHub — see the contract note in the PR. Translating by
    /// hand here is what keeps the two from being confused.
    fn kind(raw: &str) -> SubjectKind {
        use SubjectKind as K;
        match raw {
            "PullRequest" => K::PullRequest,
            "Issue" => K::Issue,
            "Discussion" => K::Discussion,
            "Release" => K::Release,
            "CheckSuite" => K::CheckSuite,
            "Commit" => K::Commit,
            // GitHub's own name is longer than ours.
            "RepositoryVulnerabilityAlert" => K::VulnerabilityAlert,
            other => K::Other(other.to_owned()),
        }
    }

    // ---- the enrichment response ------------------------------------------

    #[derive(Debug, Deserialize)]
    struct RepositoryNode {
        #[serde(default, rename = "issueOrPullRequest")]
        issue_or_pull_request: Option<SubjectNode>,
        #[serde(default)]
        discussion: Option<SubjectNode>,
    }

    /// The three subject shapes the query asks for.
    ///
    /// `state` is deserialized straight into `PrState` / `IssueState`, whose
    /// `SCREAMING_SNAKE_CASE` attributes this is the first code to put in
    /// front of a live response. They are correct: GraphQL answers `"MERGED"`
    /// and `"CLOSED"`.
    #[derive(Debug, Deserialize)]
    #[serde(tag = "__typename")]
    enum SubjectNode {
        PullRequest {
            number: u64,
            url: String,
            state: PrState,
            #[serde(rename = "isDraft", default)]
            is_draft: bool,
            #[serde(default)]
            author: Option<WireActor>,
            #[serde(default)]
            comments: Connection<Comment>,
            #[serde(default)]
            commits: Connection<CommitNode>,
        },
        Issue {
            number: u64,
            url: String,
            state: IssueState,
            #[serde(default)]
            author: Option<WireActor>,
            #[serde(default)]
            comments: Connection<Comment>,
        },
        Discussion {
            number: u64,
            url: String,
            #[serde(default)]
            author: Option<WireActor>,
            #[serde(default)]
            comments: Connection<Comment>,
        },
        /// A subject type we did not ask about. Keeps an unexpected
        /// `__typename` from failing the whole batch.
        #[serde(other)]
        Unknown,
    }

    #[derive(Debug, Deserialize)]
    struct Connection<T> {
        #[serde(default = "Vec::new")]
        nodes: Vec<T>,
    }

    // Written out rather than derived: `#[derive(Default)]` on a generic
    // struct demands `T: Default`, which a wire node has no business
    // implementing. An absent connection is an empty one.
    impl<T> Default for Connection<T> {
        fn default() -> Self {
            Self { nodes: Vec::new() }
        }
    }

    #[derive(Debug, Deserialize)]
    struct Comment {
        #[serde(default)]
        author: Option<WireActor>,
    }

    #[derive(Debug, Deserialize)]
    struct CommitNode {
        commit: Commit,
    }

    #[derive(Debug, Deserialize)]
    struct Commit {
        #[serde(rename = "statusCheckRollup", default)]
        status_check_rollup: Option<Rollup>,
    }

    #[derive(Debug, Deserialize)]
    struct Rollup {
        state: StatusState,
    }

    #[derive(Debug, Deserialize)]
    struct WireActor {
        login: String,
        #[serde(rename = "avatarUrl", default)]
        avatar_url: Option<String>,
        #[serde(rename = "__typename", default)]
        typename: String,
    }

    impl WireActor {
        fn into_model(self) -> Actor {
            Actor {
                is_bot: self.typename == "Bot",
                login: self.login,
                // `Actor.node_id` stays `None`: the query does not ask for
                // `id`, because nothing in an inbox row needs it and every
                // field costs query size across fifty aliases.
                node_id: None,
                avatar_url: self.avatar_url,
            }
        }
    }

    /// Translate one alias's node into a [`SubjectDetail`].
    pub(super) fn detail_from(node: Value, target: &Target) -> Result<SubjectDetail, String> {
        let node: RepositoryNode =
            serde_json::from_value(node).map_err(|e| format!("unexpected shape: {e}"))?;

        let subject = node
            .issue_or_pull_request
            .or(node.discussion)
            .ok_or_else(|| alias_missing(target))?;

        Ok(match subject {
            SubjectNode::PullRequest {
                number,
                url,
                state,
                is_draft,
                author,
                comments,
                commits,
            } => SubjectDetail {
                number: Some(number),
                status: SubjectStatus::PullRequest(display_status(state, is_draft)),
                checks: rollup(&commits),
                last_actor: last_actor(comments, author),
                html_url: url,
            },
            SubjectNode::Issue {
                number,
                url,
                state,
                author,
                comments,
            } => SubjectDetail {
                number: Some(number),
                status: SubjectStatus::Issue(match state {
                    IssueState::Open => IssueDisplayStatus::Open,
                    // The enrichment query does not ask for `stateReason`, so
                    // "closed" is all we know — not that it was completed.
                    IssueState::Closed => IssueDisplayStatus::Completed,
                }),
                checks: CheckRollup::empty(),
                last_actor: last_actor(comments, author),
                html_url: url,
            },
            SubjectNode::Discussion {
                number,
                url,
                author,
                comments,
            } => SubjectDetail {
                number: Some(number),
                // A discussion has no state this query fetches, and
                // `SubjectStatus::None` says so instead of picking the least
                // wrong of four wrong answers.
                status: SubjectStatus::None,
                checks: CheckRollup::empty(),
                last_actor: last_actor(comments, author),
                html_url: url,
            },
            SubjectNode::Unknown => return Err(alias_missing(target)),
        })
    }

    /// The same rule `PullRequest::display_status` applies, on the wire
    /// fields: draft only means anything while open.
    fn display_status(state: PrState, is_draft: bool) -> PrDisplayStatus {
        match state {
            PrState::Open if is_draft => PrDisplayStatus::Draft,
            PrState::Open => PrDisplayStatus::Open,
            PrState::Merged => PrDisplayStatus::Merged,
            PrState::Closed => PrDisplayStatus::Closed,
        }
    }

    /// Who acted last: the latest comment's author, falling back to whoever
    /// opened the subject.
    ///
    /// Not the whole truth — a label change is an action and has an actor —
    /// but it is the honest part that costs one node per subject. Reading the
    /// real last actor means a `timelineItems` connection and an inline
    /// fragment per event type, which is the timeline query M2 will write, not
    /// something an inbox row can afford.
    fn last_actor(comments: Connection<Comment>, author: Option<WireActor>) -> Option<Actor> {
        comments
            .nodes
            .into_iter()
            .next_back()
            .and_then(|c| c.author)
            .or(author)
            .map(WireActor::into_model)
    }

    /// GitHub's aggregate verdict for the head commit, as a [`CheckRollup`].
    ///
    /// The counts are all zero and `runs` is empty, deliberately.
    /// `statusCheckRollup.state` is a single merged verdict — GitHub has
    /// already applied its own precedence across both CI systems — and the
    /// per-run breakdown costs a `contexts(first: 100)` connection on every
    /// one of fifty aliases, which is fifty times the point cost for a number
    /// no inbox row renders. `spec/10-domain-model.md` §3.3 already says
    /// `runs` is empty in list contexts; the counts follow it.
    fn rollup(commits: &Connection<CommitNode>) -> CheckRollup {
        let Some(state) = commits
            .nodes
            .last()
            .and_then(|c| c.commit.status_check_rollup.as_ref())
            .map(|r| r.state)
        else {
            return CheckRollup::empty();
        };

        CheckRollup {
            state: match state {
                StatusState::Success => RollupState::Success,
                StatusState::Failure | StatusState::Error => RollupState::Failure,
                StatusState::Pending | StatusState::Expected => RollupState::Pending,
            },
            ..CheckRollup::empty()
        }
    }
}

fn alias_missing(target: &Target) -> String {
    format!(
        "{}/{}#{} could not be resolved",
        target.owner, target.repo, target.number
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use omaghy_model::{
        NotificationReason, PrDisplayStatus, RollupState, SubjectRef, SubjectStatus,
    };

    // ---- the path a filter fetches ---------------------------------------

    #[test]
    fn the_default_filter_asks_for_the_whole_inbox_one_page_at_a_time() {
        assert_eq!(
            NotificationFilter::default().path(),
            "/notifications?all=true&per_page=50"
        );
        assert_eq!(
            NotificationFilter::unread().path(),
            "/notifications?all=false&per_page=50"
        );
    }

    #[test]
    fn a_repository_filter_is_a_different_endpoint_not_a_parameter() {
        let f = NotificationFilter::default()
            .in_repo(RepoRef::new("ShaxP", "shax"))
            .per_page(3);
        assert_eq!(
            f.path(),
            "/repos/ShaxP/shax/notifications?all=true&per_page=3"
        );
    }

    #[test]
    fn per_page_is_clamped_to_what_github_will_actually_return() {
        // Asking for more is silently clamped by GitHub, which would make our
        // paging arithmetic wrong rather than merely optimistic.
        assert!(
            NotificationFilter::default()
                .per_page(200)
                .path()
                .contains("per_page=50")
        );
        assert!(
            NotificationFilter::default()
                .per_page(0)
                .path()
                .contains("per_page=1")
        );
    }

    #[test]
    fn since_and_page_appear_only_when_they_were_asked_for() {
        let f = NotificationFilter::default()
            .since(time::macros::datetime!(2026-09-12 08:00 UTC))
            .page(3);
        assert_eq!(
            f.path(),
            "/notifications?all=true&per_page=50&since=2026-09-12T08:00:00Z&page=3"
        );
    }

    // ---- which rows get fetched ------------------------------------------

    fn notification(kind: SubjectKind, url: Option<&str>) -> Notification {
        Notification {
            id: NotificationId("1".into()),
            unread: true,
            reason: NotificationReason::Author,
            updated_at: time::macros::datetime!(2026-07-10 16:04:40 UTC),
            title: "t".into(),
            kind: kind.clone(),
            repo: RepoRef::new("ShaxP", "shax"),
            subject: url.and_then(SubjectRef::from_api_url),
            detail: Enrichment::Absent,
        }
    }

    #[tokio::test]
    async fn a_subject_with_nothing_to_fetch_is_not_a_failure() {
        // GitHub sends a CheckSuite notification with `subject.url` null, so
        // there is no number, no URL, and nothing that will ever be
        // enrichable. Recording that as `Failed` made an ordinary row render
        // as broken, and `is_a_problem()` could not tell the two apart —
        // observed against the live API before this was fixed.
        use crate::{auth::Token, cassette::StubTransport, client::GitHubClient};
        use std::sync::Arc;

        let mut rows = vec![notification(SubjectKind::CheckSuite, None)];
        // No responses queued: reaching the transport at all would be the bug.
        let stub = Arc::new(StubTransport::sequence(vec![]));
        let client = GitHubClient::new(Token::new("t").expect("a token"), stub.clone());

        Notifications::new(&client).enrich(&mut rows).await.unwrap();

        assert_eq!(rows[0].detail, Enrichment::NotApplicable);
        assert!(
            !rows[0].detail.is_a_problem(),
            "nothing to fetch is not a problem"
        );
        assert!(
            !rows[0].detail.wants_fetch(),
            "and must not be asked for again"
        );
        assert!(
            stub.requests().is_empty(),
            "a subject with no address must not cost a request"
        );
    }

    #[test]
    fn only_subjects_addressed_by_a_number_are_enrichable() {
        let pr = notification(
            SubjectKind::PullRequest,
            Some("https://api.github.com/repos/ShaxP/shax/pulls/61"),
        );
        assert_eq!(
            Target::of(&pr),
            Some(Target {
                owner: "ShaxP".into(),
                repo: "shax".into(),
                number: 61,
                discussion: false,
            })
        );

        // A commit is addressed by SHA, so `SubjectDetail.number` has nothing
        // to hold and the query has nothing to ask.
        let commit = notification(
            SubjectKind::Commit,
            Some(
                "https://api.github.com/repos/ShaxP/shax/commits/9e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f",
            ),
        );
        assert_eq!(Target::of(&commit), None);

        // A check suite carries no subject URL at all.
        assert_eq!(
            Target::of(&notification(SubjectKind::CheckSuite, None)),
            None
        );
    }

    #[test]
    fn a_discussion_asks_a_different_field_than_a_pull_request() {
        let d = notification(
            SubjectKind::Discussion,
            Some("https://api.github.com/repos/ShaxP/shax/discussions/9"),
        );
        let target = Target::of(&d).expect("a discussion is addressable");
        assert!(target.discussion);

        let q = build_query(&[target]);
        assert!(q.query.contains("discussion(number: $n0)"), "{}", q.query);
        assert!(!q.query.contains("issueOrPullRequest"), "{}", q.query);
    }

    // ---- the query -------------------------------------------------------

    #[test]
    fn repository_names_travel_as_variables_not_as_string_interpolation() {
        // A repository name is attacker-controlled as far as this process is
        // concerned. Interpolating one would be an injection.
        let target = Target {
            owner: "o\") { viewer { login } } #".into(),
            repo: "r".into(),
            number: 1,
            discussion: false,
        };
        let q = build_query(&[target]);
        assert!(
            !q.query.contains("viewer"),
            "the name reached the query text: {}",
            q.query
        );
        assert_eq!(
            q.variables.as_ref().unwrap()["o0"],
            "o\") { viewer { login } } #"
        );
    }

    #[test]
    fn one_query_addresses_a_whole_page() {
        let targets: Vec<Target> = (1..=50)
            .map(|n| Target {
                owner: "ShaxP".into(),
                repo: "shax".into(),
                number: n,
                discussion: false,
            })
            .collect();
        let q = build_query(&targets);
        assert_eq!(q.query.matches("repository(owner:").count(), 50);
        // The selection lives in fragments, so fifty subjects cost fifty lines
        // rather than fifty copies of it — and every connection asks for a
        // single node, which is what keeps a full page at one GraphQL point.
        assert_eq!(q.query.matches("fragment ").count(), 4);
        assert!(
            !q.query.contains("first: "),
            "no connection is paged: {}",
            q.query
        );
        assert!(q.query.contains("fragment PrFields"));
        assert!(!q.mutation, "enrichment reads");
    }

    // ---- the rollup ------------------------------------------------------

    #[test]
    fn githubs_aggregate_verdict_maps_onto_the_rollup() {
        let cases = [
            ("SUCCESS", RollupState::Success),
            ("FAILURE", RollupState::Failure),
            ("ERROR", RollupState::Failure),
            ("PENDING", RollupState::Pending),
            ("EXPECTED", RollupState::Pending),
        ];
        for (raw, expected) in cases {
            let node = serde_json::json!({
                "issueOrPullRequest": {
                    "__typename": "PullRequest",
                    "number": 1, "url": "https://github.com/o/r/pull/1",
                    "state": "OPEN", "isDraft": false, "author": null,
                    "comments": {"nodes": []},
                    "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": raw}}}]}
                }
            });
            let target = Target {
                owner: "o".into(),
                repo: "r".into(),
                number: 1,
                discussion: false,
            };
            let d = wire::detail_from(node, &target).expect("a well-formed node");
            assert_eq!(d.checks.state, expected, "{raw}");
        }
    }

    #[test]
    fn a_repository_with_no_ci_is_none_not_a_verdict() {
        let node = serde_json::json!({
            "issueOrPullRequest": {
                "__typename": "PullRequest",
                "number": 1, "url": "https://github.com/o/r/pull/1",
                "state": "OPEN", "isDraft": true, "author": {"login": "a", "__typename": "User"},
                "comments": {"nodes": []},
                "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]}
            }
        });
        let target = Target {
            owner: "o".into(),
            repo: "r".into(),
            number: 1,
            discussion: false,
        };
        let d = wire::detail_from(node, &target).unwrap();
        assert_eq!(d.checks.state, RollupState::None);
        assert_eq!(
            d.status,
            SubjectStatus::PullRequest(PrDisplayStatus::Draft),
            "draft is not a state"
        );
    }
}

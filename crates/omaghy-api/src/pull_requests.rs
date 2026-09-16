//! Pull requests — a page of rows for a search, and one opened.
//!
//! Two queries, both hand-written (`spec/00-overview.md` §4.1) and both
//! validated against the live schema while this module was written:
//!
//! - **The list** is `search(type: ISSUE)` with a `PullRequest` fragment on
//!   each node. Search is the only vocabulary that says both "this
//!   repository's open PRs" and "everything awaiting my review anywhere",
//!   which is why `PrQuery` (`20-store.md` §1.1) is a search string. Measured
//!   live: thirty rows with labels and latest reviews cost **1 point**.
//! - **The detail** is `repository.pullRequest` with the same row fragment,
//!   the body and branches, the head commit's check contexts, every review
//!   thread, and the first hundred timeline items. Measured live on a merged
//!   PR: **2 points**. A longer timeline is followed by cursor, up to
//!   [`TIMELINE_CAP`].
//!
//! # What the schema settled
//!
//! **Viewer-relative review state comes from two fields, not from matching
//! logins.** `viewerLatestReviewRequest` is non-null exactly while a review
//! is requested of the token's user (a submitted review consumes the
//! request), and `viewerLatestReview` is the last one they gave. Together
//! they fill `ReviewSummary::{i_am_requested, my_review}` without this crate
//! knowing who the viewer is.
//!
//! **Four timeline types are gated behind `read:project`.** Selecting even
//! `createdAt` on `AddedToProjectV2Event`, `RemovedFromProjectV2Event`,
//! `ProjectV2ItemStatusChangedEvent` or `ConvertedFromDraftEvent` fails the
//! whole query with `INSUFFICIENT_SCOPES` under the scopes `PREREQUISITES.md`
//! asks for. They are left unselected, arrive as a bare `__typename`, and
//! render as an [`TimelineKind::Other`] with no actor and the previous
//! event's time.
//!
//! **A merged PR carries both a `MergedEvent` and a `ClosedEvent`**, one
//! second apart. Both are kept: the model mirrors the timeline, and which
//! of the two to show is the surface's decision.
//!
//! **`requestedReviewer` can be null** — a team the token cannot see. Such
//! an event becomes an `Other` rather than a request from a ghost.
//!
//! **Review threads are not timeline items of their review.** They hang off
//! the PR as `reviewThreads`, and each comment names the review it belongs
//! to. So threads are fetched once, attached to their review by that id, and
//! any left over — a thread whose review is not on the page — becomes a
//! [`TimelineKind::ReviewThread`] at its first comment's time. The
//! `PullRequestReviewThread` timeline items are then dropped, or every thread
//! would appear twice.

use crate::{client::GitHubClient, graphql::GraphQlRequest};
use omaghy_model::{
    Actor, CheckConclusion, CheckRollup, CheckRun, CheckStatus, CommitStatus, Label, Markdown,
    Mergeable, NodeId, PrDetail, PrState, PullRequest, Reactions, RepoRef, ReviewDecision,
    ReviewState, ReviewSummary, ReviewThread, Rgb, RollupState, StatusState, StoreError, SubjectId,
    SubjectKind, SubjectRef, ThreadComment, TimelineEvent, TimelineKind,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use time::OffsetDateTime;

/// Rows per list fetch. A page the surface scrolls, not the whole result.
pub const LIST_PAGE: u8 = 30;

/// Timeline items per request. GitHub's maximum for a connection.
pub const TIMELINE_PAGE: u8 = 100;

/// The most timeline items a detail fetch will follow. Beyond this a PR is
/// a document the web renders better (`00-overview.md` §1). Pages run oldest
/// first, so a capped timeline holds the oldest events and logs that it
/// stopped; `o` is the way to the rest.
pub const TIMELINE_CAP: usize = 500;

/// The sort applied when the query names none. GitHub's default is "best
/// match", which is not an order a list can be scrolled in.
const DEFAULT_SORT: &str = "sort:updated-desc";

/// One page of a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrPage {
    pub items: Vec<PullRequest>,
    /// How many match the whole query, per GitHub. An estimate for large
    /// results; never use it to size a scrollbar.
    pub total: u32,
    /// Pass back as `after` for the next page. `None` on the last one.
    pub next_cursor: Option<String>,
}

/// Pull-request operations on a [`GitHubClient`].
#[derive(Debug, Clone, Copy)]
pub struct PullRequests<'a> {
    client: &'a GitHubClient,
}

impl GitHubClient {
    pub fn pull_requests(&self) -> PullRequests<'_> {
        PullRequests { client: self }
    }
}

impl<'a> PullRequests<'a> {
    pub fn new(client: &'a GitHubClient) -> Self {
        Self { client }
    }

    /// One page of rows for a GitHub search.
    ///
    /// The query is sent as given — `PrQuery::effective` has already made
    /// it a pull-request search — except that a sort is added when it has
    /// none, so the rows arrive newest-activity first. Nodes that are not
    /// pull requests (a query without `is:pr`) are skipped, not errors.
    pub async fn list(&self, query: &str, after: Option<&str>) -> Result<PrPage, StoreError> {
        let request = list_query(&with_sort(query), after);
        let data: wire::ListData = self.client.graphql(&request).await?;
        let search = data.search;
        Ok(PrPage {
            items: search
                .nodes
                .into_iter()
                .flatten()
                .filter_map(|n| n.into_row())
                .collect(),
            total: search.issue_count,
            next_cursor: search.page_info.next_cursor(),
        })
    }

    /// One pull request, opened.
    ///
    /// `StoreError::NotFound` when the repository or the number does not
    /// exist for this token — GitHub answers that as HTTP 200 with a
    /// `NOT_FOUND` error, which [`crate::graphql`] maps.
    pub async fn detail(&self, r: &SubjectRef) -> Result<PrDetail, StoreError> {
        let number = match (&r.kind, &r.id) {
            (SubjectKind::PullRequest, SubjectId::Number(n)) => *n,
            _ => {
                return Err(StoreError::Upstream {
                    status: 0,
                    message: format!("{r} is not a pull request coordinate"),
                });
            }
        };

        let data: wire::DetailData = self
            .client
            .graphql(&detail_query(&r.owner, &r.repo, number, None))
            .await?;
        let mut node = data
            .repository
            .and_then(|repo| repo.pull_request)
            .ok_or(StoreError::NotFound)?;

        // Follow the timeline while there is more and we are under the cap.
        // Each page is its own request against the same coordinate.
        let mut page = node.timeline_items.take().unwrap_or_default();
        let mut items = std::mem::take(&mut page.nodes);
        let mut truncated = false;
        while page.page_info.has_next_page {
            if items.len() >= TIMELINE_CAP {
                truncated = true;
                break;
            }
            let after = page.page_info.next_cursor();
            let more: wire::DetailData = self
                .client
                .graphql(&detail_query(&r.owner, &r.repo, number, after.as_deref()))
                .await?;
            page = more
                .repository
                .and_then(|repo| repo.pull_request)
                .and_then(|pr| pr.timeline_items)
                .unwrap_or_default();
            items.append(&mut page.nodes);
        }
        if truncated {
            tracing::info!(%r, items = items.len(), "timeline capped; the oldest events are held");
        }

        node.into_detail(items)
    }
}

/// Add [`DEFAULT_SORT`] unless the query already says how to sort.
fn with_sort(query: &str) -> String {
    let q = query.trim();
    if q.split_whitespace().any(|t| t.starts_with("sort:")) {
        q.to_owned()
    } else {
        format!("{q} {DEFAULT_SORT}")
    }
}

// ---------------------------------------------------------------------------
// the queries
// ---------------------------------------------------------------------------

/// The query string travels as a variable, never interpolated: it comes from
/// `config.toml` and from routes, neither of which this process writes.
fn list_query(query: &str, after: Option<&str>) -> GraphQlRequest {
    let text = format!(
        "query PullRequestList($q: String!, $first: Int!, $after: String) {{\n\
         \x20 search(query: $q, type: ISSUE, first: $first, after: $after) {{\n\
         \x20   issueCount\n\
         \x20   pageInfo {{ hasNextPage endCursor }}\n\
         \x20   nodes {{ __typename ...PrRow }}\n\
         \x20 }}\n\
         }}\n{FRAGMENTS}"
    );
    GraphQlRequest::query(text).variables(json!({
        "q": query,
        "first": LIST_PAGE,
        "after": after,
    }))
}

/// The first page asks for everything; a continuation (`after` set) asks
/// only for more timeline. Same document, so a cassette holds one shape.
fn detail_query(owner: &str, name: &str, number: u64, after: Option<&str>) -> GraphQlRequest {
    let text = format!(
        "query PullRequestDetail($owner: String!, $name: String!, $number: Int!, \
         $timeline: Int!, $after: String, $full: Boolean!) {{\n\
         \x20 repository(owner: $owner, name: $name) {{\n\
         \x20   pullRequest(number: $number) {{\n\
         \x20     __typename\n\
         \x20     ...PrRow @include(if: $full)\n\
         \x20     body @include(if: $full)\n\
         \x20     baseRefName @include(if: $full)\n\
         \x20     headRefName @include(if: $full)\n\
         \x20     checks: commits(last: 1) @include(if: $full) {{ nodes {{ commit {{ \
         statusCheckRollup {{ state contexts(first: 100) {{ nodes {{\n\
         \x20       __typename\n\
         \x20       ... on CheckRun {{ name status conclusion detailsUrl }}\n\
         \x20       ... on StatusContext {{ context state targetUrl }}\n\
         \x20     }} }} }} }} }} }}\n\
         \x20     reviewThreads(first: 100) @include(if: $full) {{ nodes {{\n\
         \x20       id isResolved isOutdated path line\n\
         \x20       comments(first: 50) {{ nodes {{ author {{ ...Who }} body createdAt \
         pullRequestReview {{ id }} }} }}\n\
         \x20     }} }}\n\
         \x20     timelineItems(first: $timeline, after: $after) {{\n\
         \x20       pageInfo {{ hasNextPage endCursor }}\n\
         \x20       nodes {{\n{TIMELINE_SELECTIONS}\
         \x20       }}\n\
         \x20     }}\n\
         \x20   }}\n\
         \x20 }}\n\
         }}\n{FRAGMENTS}"
    );
    GraphQlRequest::query(text).variables(json!({
        "owner": owner,
        "name": name,
        "number": number,
        "timeline": TIMELINE_PAGE,
        "after": after,
        "full": after.is_none(),
    }))
}

/// The row: everything a list needs, and the base of the detail.
///
/// `latestReviews(first: 20)` rather than `reviews`: one entry per reviewer,
/// their latest state, which is what `ReviewSummary::reviewers` is. The
/// rollup asks for `state` only — the per-run breakdown is the detail's,
/// where it costs one PR's worth of contexts rather than thirty.
const FRAGMENTS: &str = "\
fragment Who on Actor { login avatarUrl __typename }
fragment Tag on Label { name color description }
fragment PrRow on PullRequest {
  id number title url state isDraft mergeable createdAt updatedAt
  additions deletions changedFiles totalCommentsCount
  repository { owner { login } name }
  author { ...Who }
  labels(first: 20) { nodes { ...Tag } }
  reviewDecision
  viewerLatestReviewRequest { id }
  viewerLatestReview { state }
  latestReviews(first: 20) { nodes { author { ...Who } state } }
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
}";

/// One inline fragment per modelled kind, then `actor`/`createdAt` for every
/// other event type the schema lets an ordinary token read. Anything absent
/// from this list still arrives — GraphQL returns the `__typename` of any
/// union member — it just carries nothing else.
const TIMELINE_SELECTIONS: &str = "\
          __typename
          ... on IssueComment { id author { ...Who } body createdAt lastEditedAt reactionGroups { content reactors { totalCount } } }
          ... on PullRequestReview { id author { ...Who } state body createdAt }
          ... on PullRequestCommit { commit { oid messageHeadline committedDate author { name user { ...Who } } } }
          ... on MergedEvent { actor { ...Who } createdAt commit { oid } mergeRefName }
          ... on ClosedEvent { actor { ...Who } createdAt closer { __typename ... on Commit { oid } } }
          ... on ReopenedEvent { actor { ...Who } createdAt }
          ... on ReadyForReviewEvent { actor { ...Who } createdAt }
          ... on ConvertToDraftEvent { actor { ...Who } createdAt }
          ... on RenamedTitleEvent { actor { ...Who } createdAt previousTitle currentTitle }
          ... on LabeledEvent { actor { ...Who } createdAt label { ...Tag } }
          ... on UnlabeledEvent { actor { ...Who } createdAt label { ...Tag } }
          ... on AssignedEvent { actor { ...Who } createdAt assignee { __typename ... on User { login } ... on Bot { login } ... on Mannequin { login } ... on Organization { login } } }
          ... on UnassignedEvent { actor { ...Who } createdAt assignee { __typename ... on User { login } ... on Bot { login } ... on Mannequin { login } ... on Organization { login } } }
          ... on ReviewRequestedEvent { actor { ...Who } createdAt requestedReviewer { __typename ... on User { login } ... on Bot { login } ... on Mannequin { login } ... on Team { name } } }
          ... on ReviewRequestRemovedEvent { actor { ...Who } createdAt requestedReviewer { __typename ... on User { login } ... on Bot { login } ... on Mannequin { login } ... on Team { name } } }
          ... on CrossReferencedEvent { actor { ...Who } createdAt willCloseTarget source { __typename ... on Issue { number repository { nameWithOwner } } ... on PullRequest { number repository { nameWithOwner } } } }
          ... on HeadRefForcePushedEvent { actor { ...Who } createdAt beforeCommit { oid } afterCommit { oid } }
          ... on MilestonedEvent { actor { ...Who } createdAt }
          ... on DemilestonedEvent { actor { ...Who } createdAt }
          ... on ReviewDismissedEvent { actor { ...Who } createdAt }
          ... on HeadRefDeletedEvent { actor { ...Who } createdAt }
          ... on HeadRefRestoredEvent { actor { ...Who } createdAt }
          ... on BaseRefChangedEvent { actor { ...Who } createdAt }
          ... on BaseRefForcePushedEvent { actor { ...Who } createdAt }
          ... on BaseRefDeletedEvent { actor { ...Who } createdAt }
          ... on MentionedEvent { actor { ...Who } createdAt }
          ... on ReferencedEvent { actor { ...Who } createdAt }
          ... on SubscribedEvent { actor { ...Who } createdAt }
          ... on UnsubscribedEvent { actor { ...Who } createdAt }
          ... on ConnectedEvent { actor { ...Who } createdAt }
          ... on DisconnectedEvent { actor { ...Who } createdAt }
          ... on PinnedEvent { actor { ...Who } createdAt }
          ... on UnpinnedEvent { actor { ...Who } createdAt }
          ... on LockedEvent { actor { ...Who } createdAt }
          ... on UnlockedEvent { actor { ...Who } createdAt }
          ... on AutoMergeEnabledEvent { actor { ...Who } createdAt }
          ... on AutoMergeDisabledEvent { actor { ...Who } createdAt }
          ... on AutoRebaseEnabledEvent { actor { ...Who } createdAt }
          ... on AutoSquashEnabledEvent { actor { ...Who } createdAt }
          ... on AddedToMergeQueueEvent { actor { ...Who } createdAt }
          ... on RemovedFromMergeQueueEvent { actor { ...Who } createdAt }
          ... on AddedToProjectEvent { actor { ...Who } createdAt }
          ... on RemovedFromProjectEvent { actor { ...Who } createdAt }
          ... on MovedColumnsInProjectEvent { actor { ...Who } createdAt }
          ... on DeployedEvent { actor { ...Who } createdAt }
          ... on DeploymentEnvironmentChangedEvent { actor { ...Who } createdAt }
          ... on CommentDeletedEvent { actor { ...Who } createdAt }
          ... on TransferredEvent { actor { ...Who } createdAt }
          ... on MarkedAsDuplicateEvent { actor { ...Who } createdAt }
          ... on UnmarkedAsDuplicateEvent { actor { ...Who } createdAt }
          ... on UserBlockedEvent { actor { ...Who } createdAt }
";

// ---------------------------------------------------------------------------
// the wire, and the translation
// ---------------------------------------------------------------------------

/// GitHub's shapes. Private; nothing here crosses into `omaghy-model`
/// (`spec/10-domain-model.md` §1). The model's closed enums are used as
/// wire enums where the spelling is GitHub's — `PrState`, `Mergeable`,
/// `ReviewState`, the check enums — because they were verified against the
/// schema when the model was written and a second copy would only drift.
mod wire {
    use super::*;

    #[derive(Debug, Deserialize)]
    pub(super) struct ListData {
        pub(super) search: SearchNode,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct SearchNode {
        #[serde(rename = "issueCount")]
        pub(super) issue_count: u32,
        #[serde(rename = "pageInfo")]
        pub(super) page_info: PageInfo,
        #[serde(default)]
        pub(super) nodes: Vec<Option<PrNode>>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct DetailData {
        pub(super) repository: Option<RepositoryNode>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct RepositoryNode {
        #[serde(rename = "pullRequest")]
        pub(super) pull_request: Option<PrNode>,
    }

    #[derive(Debug, Default, Deserialize)]
    pub(super) struct PageInfo {
        #[serde(rename = "hasNextPage", default)]
        pub(super) has_next_page: bool,
        #[serde(rename = "endCursor", default)]
        end_cursor: Option<String>,
    }

    impl PageInfo {
        pub(super) fn next_cursor(&self) -> Option<String> {
            self.has_next_page
                .then(|| self.end_cursor.clone())
                .flatten()
        }
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Connection<T> {
        #[serde(default = "Vec::new")]
        pub(super) nodes: Vec<T>,
    }

    impl<T> Default for Connection<T> {
        fn default() -> Self {
            Self { nodes: Vec::new() }
        }
    }

    #[derive(Debug, Default, Deserialize)]
    pub(super) struct TimelinePage {
        #[serde(rename = "pageInfo", default)]
        pub(super) page_info: PageInfo,
        #[serde(default)]
        pub(super) nodes: Vec<Value>,
    }

    /// A search node or a `pullRequest`. Every field the row fragment asks
    /// for is optional on the wire — a search node that is an `Issue` has
    /// none of them, and a continuation page asks for none — and
    /// [`PrNode::into_row`] is where "not a pull request" is decided.
    #[derive(Debug, Deserialize)]
    pub(super) struct PrNode {
        #[serde(rename = "__typename", default)]
        typename: String,
        id: Option<String>,
        number: Option<u64>,
        title: Option<String>,
        state: Option<PrState>,
        #[serde(rename = "isDraft", default)]
        is_draft: bool,
        mergeable: Option<Mergeable>,
        #[serde(rename = "createdAt", default, with = "time::serde::rfc3339::option")]
        created_at: Option<OffsetDateTime>,
        #[serde(rename = "updatedAt", default, with = "time::serde::rfc3339::option")]
        updated_at: Option<OffsetDateTime>,
        #[serde(default)]
        additions: u32,
        #[serde(default)]
        deletions: u32,
        #[serde(rename = "changedFiles", default)]
        changed_files: u32,
        #[serde(rename = "totalCommentsCount", default)]
        total_comments_count: u32,
        repository: Option<RepoNode>,
        author: Option<WireActor>,
        #[serde(default)]
        labels: Connection<WireLabel>,
        #[serde(rename = "reviewDecision")]
        review_decision: Option<ReviewDecision>,
        #[serde(rename = "viewerLatestReviewRequest")]
        viewer_latest_review_request: Option<Value>,
        #[serde(rename = "viewerLatestReview")]
        viewer_latest_review: Option<ReviewStateNode>,
        #[serde(rename = "latestReviews", default)]
        latest_reviews: Connection<LatestReview>,
        #[serde(default)]
        commits: Connection<CommitNode>,
        // ---- detail only ----
        body: Option<String>,
        #[serde(rename = "baseRefName")]
        base_ref_name: Option<String>,
        #[serde(rename = "headRefName")]
        head_ref_name: Option<String>,
        /// The head commit again, with contexts. Aliased `checks` so the
        /// same `commits(last: 1)` field can be asked for twice.
        checks: Option<Connection<CommitNode>>,
        #[serde(rename = "reviewThreads")]
        review_threads: Option<Connection<WireThread>>,
        #[serde(rename = "timelineItems")]
        pub(super) timeline_items: Option<TimelinePage>,
    }

    #[derive(Debug, Deserialize)]
    struct RepoNode {
        owner: OwnerNode,
        name: String,
    }

    #[derive(Debug, Deserialize)]
    struct OwnerNode {
        login: String,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct WireActor {
        login: String,
        #[serde(rename = "avatarUrl", default)]
        avatar_url: Option<String>,
        #[serde(rename = "__typename", default)]
        typename: String,
    }

    impl WireActor {
        pub(super) fn into_model(self) -> Actor {
            Actor {
                is_bot: self.typename == "Bot",
                login: self.login,
                // Not asked for: nothing a row or a timeline line renders
                // needs an actor's node id.
                node_id: None,
                avatar_url: self.avatar_url,
            }
        }
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct WireLabel {
        name: String,
        #[serde(default)]
        color: String,
        #[serde(default)]
        description: Option<String>,
    }

    impl WireLabel {
        pub(super) fn into_model(self) -> Label {
            Label {
                name: self.name,
                // GitHub always sends six hex digits; a label with a colour
                // this cannot parse is still a label, in the grey GitHub
                // itself uses for one with no colour.
                color: Rgb::parse_hex(&self.color).unwrap_or(Rgb {
                    r: 0xed,
                    g: 0xed,
                    b: 0xed,
                }),
                description: self.description.filter(|d| !d.is_empty()),
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct ReviewStateNode {
        state: ReviewState,
    }

    #[derive(Debug, Deserialize)]
    struct LatestReview {
        author: Option<WireActor>,
        state: ReviewState,
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
        #[serde(default)]
        contexts: Option<Connection<RollupNode>>,
    }

    /// One node of `statusCheckRollup.contexts` — the two CI systems side
    /// by side (`10-domain-model.md` §3.3).
    #[derive(Debug, Deserialize)]
    #[serde(tag = "__typename")]
    enum RollupNode {
        CheckRun {
            name: String,
            status: CheckStatus,
            conclusion: Option<CheckConclusion>,
            #[serde(rename = "detailsUrl", default)]
            details_url: Option<String>,
        },
        StatusContext {
            context: String,
            state: StatusState,
            #[serde(rename = "targetUrl", default)]
            target_url: Option<String>,
        },
        #[serde(other)]
        Unknown,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct WireThread {
        #[serde(rename = "isResolved", default)]
        is_resolved: bool,
        #[serde(rename = "isOutdated", default)]
        is_outdated: bool,
        #[serde(default)]
        path: String,
        line: Option<u32>,
        #[serde(default)]
        comments: Connection<WireThreadComment>,
    }

    #[derive(Debug, Deserialize)]
    struct WireThreadComment {
        author: Option<WireActor>,
        #[serde(default)]
        body: String,
        #[serde(rename = "createdAt", with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
        #[serde(rename = "pullRequestReview")]
        pull_request_review: Option<IdNode>,
    }

    #[derive(Debug, Deserialize)]
    struct IdNode {
        id: String,
    }

    impl PrNode {
        /// The list row. `None` for a search node that is not a pull request.
        pub(super) fn into_row(self) -> Option<PullRequest> {
            if self.typename != "PullRequest" {
                return None;
            }
            let repo = self.repository.as_ref()?;
            let (Some(id), Some(number), Some(title), Some(state)) = (
                self.id.as_ref(),
                self.number,
                self.title.as_ref(),
                self.state,
            ) else {
                return None;
            };
            let head = self.commits.nodes.last();
            Some(PullRequest {
                node_id: NodeId(id.clone()),
                number,
                repo: RepoRef::new(repo.owner.login.clone(), repo.name.clone()),
                title: title.clone(),
                author: self.author.map(WireActor::into_model),
                state,
                is_draft: self.is_draft,
                mergeable: self.mergeable.unwrap_or(Mergeable::Unknown),
                labels: self
                    .labels
                    .nodes
                    .into_iter()
                    .map(WireLabel::into_model)
                    .collect(),
                review: ReviewSummary {
                    decision: self.review_decision,
                    reviewers: self
                        .latest_reviews
                        .nodes
                        .into_iter()
                        .filter_map(|r| Some((r.author?.into_model(), r.state)))
                        .collect(),
                    i_am_requested: self.viewer_latest_review_request.is_some(),
                    my_review: self.viewer_latest_review.map(|r| r.state),
                },
                checks: head
                    .and_then(|c| c.commit.status_check_rollup.as_ref())
                    .map(Rollup::summary)
                    .unwrap_or_else(CheckRollup::empty),
                comment_count: self.total_comments_count,
                additions: self.additions,
                deletions: self.deletions,
                changed_files: self.changed_files,
                created_at: self.created_at.unwrap_or(OffsetDateTime::UNIX_EPOCH),
                updated_at: self.updated_at.unwrap_or(OffsetDateTime::UNIX_EPOCH),
            })
        }

        /// The detail: the row, plus what only the detail query asked for,
        /// plus a timeline assembled from `items`.
        pub(super) fn into_detail(mut self, items: Vec<Value>) -> Result<PrDetail, StoreError> {
            let body = self.body.take().unwrap_or_default();
            let base_ref = self.base_ref_name.take().unwrap_or_default();
            let head_ref = self.head_ref_name.take().unwrap_or_default();
            let checks = self.checks.take();
            let threads = self.review_threads.take().unwrap_or_default();
            let created_at = self.created_at.unwrap_or(OffsetDateTime::UNIX_EPOCH);

            let mut pr = self.into_row().ok_or_else(|| StoreError::Upstream {
                status: 200,
                message: "pull request detail was missing its row fields".to_owned(),
            })?;
            if let Some(rollup) = checks
                .as_ref()
                .and_then(|c| c.nodes.last())
                .and_then(|c| c.commit.status_check_rollup.as_ref())
            {
                pr.checks = rollup.full();
            }

            Ok(PrDetail {
                pr,
                body: Markdown::from_source(body),
                base_ref,
                head_ref,
                timeline: timeline::assemble(items, threads.nodes, created_at),
            })
        }
    }

    impl Rollup {
        /// State only: a list row's rollup, counts zero and runs empty.
        fn summary(&self) -> CheckRollup {
            CheckRollup {
                state: match self.state {
                    StatusState::Success => RollupState::Success,
                    StatusState::Failure | StatusState::Error => RollupState::Failure,
                    StatusState::Pending | StatusState::Expected => RollupState::Pending,
                },
                ..CheckRollup::empty()
            }
        }

        /// Every context, merged by the model's own precedence.
        ///
        /// GitHub's aggregate `state` is deliberately not used here: the
        /// model resolves the two CI systems once, in one place
        /// (`10-domain-model.md` §3.3), and a rollup that agreed with the
        /// contexts by construction is one that cannot contradict its own
        /// run list.
        fn full(&self) -> CheckRollup {
            let mut runs = Vec::new();
            let mut statuses = Vec::new();
            for ctx in self
                .contexts
                .as_ref()
                .map(|c| c.nodes.as_slice())
                .unwrap_or_default()
            {
                match ctx {
                    RollupNode::CheckRun {
                        name,
                        status,
                        conclusion,
                        details_url,
                    } => runs.push(CheckRun {
                        name: name.clone(),
                        status: *status,
                        conclusion: *conclusion,
                        url: details_url.clone(),
                    }),
                    RollupNode::StatusContext {
                        context,
                        state,
                        target_url,
                    } => statuses.push(CommitStatus {
                        context: context.clone(),
                        state: *state,
                        url: target_url.clone(),
                    }),
                    RollupNode::Unknown => {}
                }
            }
            if runs.is_empty() && statuses.is_empty() {
                // Contexts beyond the first hundred, or a rollup with a state
                // but a context list this token cannot read: the verdict is
                // still worth more than "no CI".
                return self.summary();
            }
            CheckRollup::merge(runs, &statuses)
        }
    }

    impl WireThread {
        /// The thread, and the id of the review it belongs to, if any.
        pub(super) fn into_model(self) -> (ReviewThread, Option<String>, Option<OffsetDateTime>) {
            let review = self
                .comments
                .nodes
                .first()
                .and_then(|c| c.pull_request_review.as_ref())
                .map(|r| r.id.clone());
            let first_at = self.comments.nodes.first().map(|c| c.created_at);
            let thread = ReviewThread {
                path: self.path,
                line: self.line,
                is_resolved: self.is_resolved,
                is_outdated: self.is_outdated,
                comments: self
                    .comments
                    .nodes
                    .into_iter()
                    .map(|c| ThreadComment {
                        author: c.author.map(WireActor::into_model),
                        body: Markdown::from_source(c.body),
                        at: c.created_at,
                    })
                    .collect(),
            };
            (thread, review, first_at)
        }
    }
}

/// Timeline items into [`TimelineEvent`]s.
mod timeline {
    use super::*;
    use wire::{WireActor, WireLabel, WireThread};

    /// What every event carries when it carries anything.
    #[derive(Debug, Default, Deserialize)]
    struct Generic {
        actor: Option<WireActor>,
        #[serde(rename = "createdAt", default, with = "time::serde::rfc3339::option")]
        created_at: Option<OffsetDateTime>,
    }

    #[derive(Debug, Deserialize)]
    struct CommentItem {
        author: Option<WireActor>,
        #[serde(default)]
        body: String,
        #[serde(rename = "createdAt", with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
        #[serde(rename = "lastEditedAt", default)]
        last_edited_at: Option<String>,
        #[serde(rename = "reactionGroups", default)]
        reaction_groups: Vec<ReactionGroup>,
    }

    #[derive(Debug, Deserialize)]
    struct ReactionGroup {
        content: String,
        reactors: Counted,
    }

    #[derive(Debug, Deserialize)]
    struct Counted {
        #[serde(rename = "totalCount", default)]
        total_count: u32,
    }

    #[derive(Debug, Deserialize)]
    struct ReviewItem {
        id: String,
        author: Option<WireActor>,
        state: ReviewState,
        #[serde(default)]
        body: String,
        #[serde(rename = "createdAt", with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
    }

    #[derive(Debug, Deserialize)]
    struct CommitItem {
        commit: CommitFields,
    }

    #[derive(Debug, Deserialize)]
    struct CommitFields {
        oid: String,
        #[serde(rename = "messageHeadline", default)]
        message_headline: String,
        #[serde(rename = "committedDate", with = "time::serde::rfc3339")]
        committed_date: OffsetDateTime,
        author: Option<CommitAuthor>,
    }

    #[derive(Debug, Deserialize)]
    struct CommitAuthor {
        name: Option<String>,
        user: Option<WireActor>,
    }

    #[derive(Debug, Deserialize)]
    struct MergedItem {
        #[serde(flatten)]
        g: Generic,
        commit: Option<Oid>,
        #[serde(rename = "mergeRefName", default)]
        merge_ref_name: String,
    }

    #[derive(Debug, Deserialize)]
    struct ClosedItem {
        #[serde(flatten)]
        g: Generic,
        closer: Option<Closer>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(tag = "__typename")]
    enum Closer {
        Commit {
            oid: String,
        },
        #[serde(other)]
        Other,
    }

    #[derive(Debug, Deserialize)]
    struct Oid {
        oid: String,
    }

    #[derive(Debug, Deserialize)]
    struct RenamedItem {
        #[serde(flatten)]
        g: Generic,
        #[serde(rename = "previousTitle", default)]
        previous_title: String,
        #[serde(rename = "currentTitle", default)]
        current_title: String,
    }

    #[derive(Debug, Deserialize)]
    struct LabelItem {
        #[serde(flatten)]
        g: Generic,
        label: Option<WireLabel>,
    }

    #[derive(Debug, Deserialize)]
    struct WhoItem {
        #[serde(flatten)]
        g: Generic,
        assignee: Option<Named>,
        #[serde(rename = "requestedReviewer")]
        requested_reviewer: Option<Named>,
    }

    /// A `User`, `Bot`, `Mannequin`, `Organization` or `Team` — whichever
    /// field names it.
    #[derive(Debug, Deserialize)]
    struct Named {
        login: Option<String>,
        name: Option<String>,
    }

    impl Named {
        fn actor(self) -> Option<Actor> {
            self.login.or(self.name).map(Actor::new)
        }
    }

    #[derive(Debug, Deserialize)]
    struct CrossRefItem {
        #[serde(flatten)]
        g: Generic,
        #[serde(rename = "willCloseTarget", default)]
        will_close_target: bool,
        source: Option<Source>,
    }

    #[derive(Debug, Deserialize)]
    struct Source {
        #[serde(rename = "__typename", default)]
        typename: String,
        number: Option<u64>,
        repository: Option<NameWithOwner>,
    }

    #[derive(Debug, Deserialize)]
    struct NameWithOwner {
        #[serde(rename = "nameWithOwner")]
        name_with_owner: String,
    }

    #[derive(Debug, Deserialize)]
    struct ForcePushItem {
        #[serde(flatten)]
        g: Generic,
        #[serde(rename = "beforeCommit")]
        before_commit: Option<Oid>,
        #[serde(rename = "afterCommit")]
        after_commit: Option<Oid>,
    }

    /// Build the timeline: translate every item, attach threads to their
    /// reviews, and give the leftovers a place of their own.
    pub(super) fn assemble(
        items: Vec<Value>,
        threads: Vec<WireThread>,
        opened_at: OffsetDateTime,
    ) -> Vec<TimelineEvent> {
        let mut by_review: HashMap<String, Vec<ReviewThread>> = HashMap::new();
        let mut orphans: Vec<(ReviewThread, OffsetDateTime)> = Vec::new();
        for t in threads {
            let (thread, review, first_at) = t.into_model();
            match review {
                Some(id) => by_review.entry(id).or_default().push(thread),
                None => orphans.push((thread, first_at.unwrap_or(opened_at))),
            }
        }

        let mut out = Vec::with_capacity(items.len());
        let mut last_at = opened_at;
        for item in items {
            let Some(mut event) = translate(item, last_at) else {
                continue;
            };
            if let TimelineKind::Review { threads, .. } = &mut event.kind
                && let Some(id) = event.node_id.as_ref()
                && let Some(mine) = by_review.remove(&id.0)
            {
                *threads = mine;
            }
            last_at = event.at;
            out.push(event);
        }

        // Threads whose review is not on the page — or that never had one —
        // keep their place in time.
        for (thread, at) in orphans
            .into_iter()
            .chain(by_review.into_values().flatten().map(|t| {
                let at = t.comments.first().map(|c| c.at).unwrap_or(opened_at);
                (t, at)
            }))
        {
            out.push(TimelineEvent {
                node_id: None,
                actor: thread.comments.first().and_then(|c| c.author.clone()),
                at,
                kind: TimelineKind::ReviewThread(thread),
            });
        }
        out.sort_by_key(|e| e.at);
        out
    }

    fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> Option<T> {
        serde_json::from_value(v.clone()).ok()
    }

    fn generic(v: &Value, fallback_at: OffsetDateTime) -> (Option<Actor>, OffsetDateTime) {
        let g: Generic = parse(v).unwrap_or_default();
        (
            g.actor.map(WireActor::into_model),
            g.created_at.unwrap_or(fallback_at),
        )
    }

    fn event(
        node_id: Option<String>,
        actor: Option<Actor>,
        at: OffsetDateTime,
        kind: TimelineKind,
    ) -> Option<TimelineEvent> {
        Some(TimelineEvent {
            node_id: node_id.map(NodeId),
            actor,
            at,
            kind,
        })
    }

    /// One item. `None` drops it — only for the thread items, which the
    /// threads themselves replace. Everything else renders as something,
    /// even if that something is an `Other` with the schema's name on it.
    fn translate(item: Value, fallback_at: OffsetDateTime) -> Option<TimelineEvent> {
        let typename = item
            .get("__typename")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let other = |item: &Value| {
            let (actor, at) = generic(item, fallback_at);
            event(
                None,
                actor,
                at,
                TimelineKind::Other {
                    kind: typename.clone(),
                },
            )
        };

        match typename.as_str() {
            "IssueComment" => {
                let c: CommentItem = parse(&item)?;
                event(
                    item.get("id").and_then(Value::as_str).map(str::to_owned),
                    c.author.map(WireActor::into_model),
                    c.created_at,
                    TimelineKind::Comment {
                        body: Markdown::from_source(c.body),
                        reactions: reactions(&c.reaction_groups),
                        edited: c.last_edited_at.is_some(),
                    },
                )
            }
            "PullRequestReview" => {
                let r: ReviewItem = parse(&item)?;
                event(
                    Some(r.id),
                    r.author.map(WireActor::into_model),
                    r.created_at,
                    TimelineKind::Review {
                        state: r.state,
                        body: (!r.body.trim().is_empty()).then(|| Markdown::from_source(r.body)),
                        threads: Vec::new(),
                    },
                )
            }
            // Replaced by `reviewThreads`, which carry resolution state and
            // every comment; the timeline item carries neither.
            "PullRequestReviewThread" => None,
            "PullRequestCommit" => {
                let c: CommitItem = parse(&item)?;
                let by = c.commit.author.and_then(|a| {
                    a.user
                        .map(WireActor::into_model)
                        .or_else(|| a.name.filter(|n| !n.is_empty()).map(Actor::new))
                });
                event(
                    None,
                    by.clone(),
                    c.commit.committed_date,
                    TimelineKind::Commit {
                        oid: c.commit.oid,
                        message_headline: c.commit.message_headline,
                        authored_by: by,
                    },
                )
            }
            "MergedEvent" => {
                let m: MergedItem = parse(&item)?;
                let (actor, at) = (m.g.actor.map(WireActor::into_model), m.g.created_at);
                event(
                    None,
                    actor,
                    at.unwrap_or(fallback_at),
                    TimelineKind::Merged {
                        commit: m.commit.map(|c| c.oid),
                        base: m.merge_ref_name,
                    },
                )
            }
            "ClosedEvent" => {
                let c: ClosedItem = parse(&item)?;
                let (actor, at) = (c.g.actor.map(WireActor::into_model), c.g.created_at);
                event(
                    None,
                    actor,
                    at.unwrap_or(fallback_at),
                    TimelineKind::Closed {
                        by_commit: match c.closer {
                            Some(Closer::Commit { oid }) => Some(oid),
                            _ => None,
                        },
                    },
                )
            }
            "ReopenedEvent" => {
                let (actor, at) = generic(&item, fallback_at);
                event(None, actor, at, TimelineKind::Reopened)
            }
            "ReadyForReviewEvent" => {
                let (actor, at) = generic(&item, fallback_at);
                event(None, actor, at, TimelineKind::ReadyForReview)
            }
            "ConvertToDraftEvent" => {
                let (actor, at) = generic(&item, fallback_at);
                event(None, actor, at, TimelineKind::ConvertedToDraft)
            }
            "RenamedTitleEvent" => {
                let r: RenamedItem = parse(&item)?;
                let (actor, at) = (r.g.actor.map(WireActor::into_model), r.g.created_at);
                event(
                    None,
                    actor,
                    at.unwrap_or(fallback_at),
                    TimelineKind::Renamed {
                        from: r.previous_title,
                        to: r.current_title,
                    },
                )
            }
            "LabeledEvent" | "UnlabeledEvent" => {
                let l: LabelItem = parse(&item)?;
                let Some(label) = l.label.map(WireLabel::into_model) else {
                    return other(&item);
                };
                let (actor, at) = (l.g.actor.map(WireActor::into_model), l.g.created_at);
                let kind = if typename == "LabeledEvent" {
                    TimelineKind::Labeled { label }
                } else {
                    TimelineKind::Unlabeled { label }
                };
                event(None, actor, at.unwrap_or(fallback_at), kind)
            }
            "AssignedEvent"
            | "UnassignedEvent"
            | "ReviewRequestedEvent"
            | "ReviewRequestRemovedEvent" => {
                let w: WhoItem = parse(&item)?;
                // A reviewer or assignee this token cannot see comes back
                // null; naming a ghost would invent an actor.
                let Some(who) = w.assignee.or(w.requested_reviewer).and_then(Named::actor) else {
                    return other(&item);
                };
                let (actor, at) = (w.g.actor.map(WireActor::into_model), w.g.created_at);
                let kind = match typename.as_str() {
                    "AssignedEvent" => TimelineKind::Assigned { who },
                    "UnassignedEvent" => TimelineKind::Unassigned { who },
                    "ReviewRequestedEvent" => TimelineKind::ReviewRequested { who },
                    _ => TimelineKind::ReviewRequestRemoved { who },
                };
                event(None, actor, at.unwrap_or(fallback_at), kind)
            }
            "CrossReferencedEvent" => {
                let x: CrossRefItem = parse(&item)?;
                let source = x.source.and_then(|s| {
                    let repo = RepoRef::parse(&s.repository?.name_with_owner)?;
                    let kind = match s.typename.as_str() {
                        "PullRequest" => SubjectKind::PullRequest,
                        "Issue" => SubjectKind::Issue,
                        _ => return None,
                    };
                    Some(SubjectRef {
                        owner: repo.owner,
                        repo: repo.name,
                        kind,
                        id: SubjectId::Number(s.number?),
                    })
                });
                let Some(source) = source else {
                    return other(&item);
                };
                let (actor, at) = (x.g.actor.map(WireActor::into_model), x.g.created_at);
                event(
                    None,
                    actor,
                    at.unwrap_or(fallback_at),
                    TimelineKind::CrossReferenced {
                        source,
                        will_close: x.will_close_target,
                    },
                )
            }
            "HeadRefForcePushedEvent" => {
                let f: ForcePushItem = parse(&item)?;
                let (actor, at) = (f.g.actor.map(WireActor::into_model), f.g.created_at);
                event(
                    None,
                    actor,
                    at.unwrap_or(fallback_at),
                    TimelineKind::HeadRefForcePushed {
                        before: f.before_commit.map(|c| c.oid).unwrap_or_default(),
                        after: f.after_commit.map(|c| c.oid).unwrap_or_default(),
                    },
                )
            }
            "" => None,
            _ => other(&item),
        }
    }

    fn reactions(groups: &[ReactionGroup]) -> Reactions {
        let mut r = Reactions::default();
        for g in groups {
            let n = g.reactors.total_count.min(u32::from(u16::MAX)) as u16;
            match g.content.as_str() {
                "THUMBS_UP" => r.thumbs_up = n,
                "THUMBS_DOWN" => r.thumbs_down = n,
                "LAUGH" => r.laugh = n,
                "HOORAY" => r.hooray = n,
                "CONFUSED" => r.confused = n,
                "HEART" => r.heart = n,
                "ROCKET" => r.rocket = n,
                "EYES" => r.eyes = n,
                _ => {}
            }
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sort_is_added_only_when_the_query_has_none() {
        assert_eq!(
            with_sort("is:pr author:@me"),
            "is:pr author:@me sort:updated-desc"
        );
        assert_eq!(
            with_sort("is:pr sort:created-asc"),
            "is:pr sort:created-asc",
            "the caller's sort wins"
        );
    }

    #[test]
    fn the_query_travels_as_a_variable_never_interpolated() {
        let q = "repo:ShaxP/omaghy is:pr \" } mutation { evil";
        let request = list_query(q, Some("cursor"));
        assert!(!request.query.contains("evil"));
        assert_eq!(request.variables.as_ref().unwrap()["q"], q);
        assert_eq!(request.variables.as_ref().unwrap()["after"], "cursor");
        assert_eq!(request.variables.as_ref().unwrap()["first"], LIST_PAGE);
    }

    #[test]
    fn a_continuation_asks_only_for_more_timeline() {
        let first = detail_query("ShaxP", "omaghy", 34, None);
        let more = detail_query("ShaxP", "omaghy", 34, Some("c"));
        assert_eq!(
            first.query, more.query,
            "one document, so one cassette shape"
        );
        assert_eq!(first.variables.as_ref().unwrap()["full"], true);
        assert_eq!(more.variables.as_ref().unwrap()["full"], false);
        assert_eq!(more.variables.as_ref().unwrap()["after"], "c");
        assert_eq!(first.variables.as_ref().unwrap()["number"], 34);
    }

    #[test]
    fn the_project_scoped_event_types_are_not_selected() {
        // Selecting anything on these fails the whole query under the scopes
        // PREREQUISITES.md asks for. Verified live; see the module docs.
        for gated in [
            "AddedToProjectV2Event",
            "RemovedFromProjectV2Event",
            "ProjectV2ItemStatusChangedEvent",
            "ConvertedFromDraftEvent",
        ] {
            assert!(
                !TIMELINE_SELECTIONS.contains(gated),
                "{gated} needs read:project"
            );
        }
    }

    #[test]
    fn every_modelled_kind_has_a_selection() {
        // The compiler enumerates `TimelineKind`; this enumerates the query.
        for wanted in [
            "IssueComment",
            "PullRequestReview",
            "PullRequestCommit",
            "MergedEvent",
            "ClosedEvent",
            "ReopenedEvent",
            "ReadyForReviewEvent",
            "ConvertToDraftEvent",
            "RenamedTitleEvent",
            "LabeledEvent",
            "UnlabeledEvent",
            "AssignedEvent",
            "UnassignedEvent",
            "ReviewRequestedEvent",
            "ReviewRequestRemovedEvent",
            "CrossReferencedEvent",
            "HeadRefForcePushedEvent",
        ] {
            assert!(
                TIMELINE_SELECTIONS.contains(&format!("... on {wanted} ")),
                "{wanted} is modelled but not selected"
            );
        }
    }
}

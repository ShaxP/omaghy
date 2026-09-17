//! Pull requests, against recorded responses and one constructed one.
//!
//! **No test here opens a socket** (`spec/00-overview.md` §7).
//!
//! The cassettes are this repository's own history: a page of its pull
//! requests, #34 opened, and a number no PR has. Nobody reviews here but the
//! author, so the shapes a review brings — threads, a thread's review id,
//! reactions on a comment, a reviewer the token cannot see — come from a
//! **stub built below**, field for field as the live API answered them for
//! a public PR elsewhere while this module was written. A stub is not a
//! recording and is not labelled as one (`omaghy_api::cassette`).

// The constructed detail below is one `json!` literal, and that macro
// recurses once per nesting level.
#![recursion_limit = "512"]

use omaghy_api::cassette::{CassetteTransport, StubTransport};
use omaghy_api::{
    ClientConfig, Clock, GitHubClient, HttpResponse, LIST_PAGE, TIMELINE_CAP, TIMELINE_PAGE, Token,
};
use omaghy_model::{
    CheckConclusion, Mergeable, PrDisplayStatus, PrState, RepoRef, ReviewState, RollupState,
    StoreError, SubjectId, SubjectKind, SubjectRef, TimelineKind,
};
use serde_json::{Value, json};
use std::sync::Arc;
use time::OffsetDateTime;
use time::macros::datetime;

const NOW: OffsetDateTime = datetime!(2026-09-17 14:00 UTC);

fn config() -> ClientConfig {
    ClientConfig {
        clock: Clock::Fixed(NOW),
        ..ClientConfig::default()
    }
}

fn token() -> Token {
    Token::new("gho_atokenthatmustnevershowup").expect("a non-empty token")
}

fn on_cassette(name: &str) -> (GitHubClient, Arc<CassetteTransport>) {
    let transport = Arc::new(CassetteTransport::load(name));
    (
        GitHubClient::with_config(token(), transport.clone(), config()),
        transport,
    )
}

fn on_stub(responses: Vec<Value>) -> (GitHubClient, Arc<StubTransport>) {
    let transport =
        Arc::new(StubTransport::sequence(responses.into_iter().map(|body| {
            Ok(HttpResponse::new(200).body(body.to_string().into_bytes()))
        })));
    (
        GitHubClient::with_config(token(), transport.clone(), config()),
        transport,
    )
}

fn pr(number: u64) -> SubjectRef {
    SubjectRef::pull_request(&RepoRef::new("ShaxP", "omaghy"), number)
}

/// What the request body actually carried, so the query is checked as sent
/// rather than as built.
fn sent_variables(transport: &CassetteTransport) -> Vec<Value> {
    transport
        .requests()
        .iter()
        .map(|r| {
            let body: Value =
                serde_json::from_slice(r.body.as_deref().unwrap_or_default()).expect("a JSON body");
            body["variables"].clone()
        })
        .collect()
}

// ------------------------------------------------------------------ the list

#[tokio::test]
async fn a_page_of_rows_arrives_newest_activity_first_with_a_cursor() {
    let (client, transport) = on_cassette("pull_request_list");
    let page = client
        .pull_requests()
        .list("repo:ShaxP/omaghy is:pr", None)
        .await
        .unwrap();

    assert_eq!(page.items.len(), usize::from(LIST_PAGE));
    assert_eq!(page.total, 35, "how many match, not how many were fetched");
    assert_eq!(
        page.next_cursor.as_deref(),
        Some("Y3Vyc29yOjMw"),
        "thirty-five is more than a page"
    );

    let updated: Vec<_> = page.items.iter().map(|p| p.updated_at).collect();
    assert!(updated.windows(2).all(|w| w[0] >= w[1]), "newest first");
    assert_eq!(page.items[0].number, 35);
    assert!(
        page.items
            .iter()
            .all(|p| p.repo == RepoRef::new("ShaxP", "omaghy")),
        "the query scoped the repository"
    );

    // The sort was added because the query named none, and the query went
    // as a variable.
    let vars = sent_variables(&transport);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0]["q"], "repo:ShaxP/omaghy is:pr sort:updated-desc");
    assert_eq!(vars[0]["first"], LIST_PAGE);
    assert_eq!(vars[0]["after"], Value::Null);
}

#[tokio::test]
async fn a_row_carries_what_a_list_renders_and_nothing_a_detail_would() {
    let (client, _) = on_cassette("pull_request_list");
    let page = client
        .pull_requests()
        .list("repo:ShaxP/omaghy is:pr", None)
        .await
        .unwrap();
    let row = &page.items[1];

    assert_eq!(row.number, 34);
    assert_eq!(row.title, "feat(model,store): the M2 pull-request contract");
    assert_eq!(row.state, PrState::Merged);
    assert_eq!(row.display_status(), PrDisplayStatus::Merged);
    assert_eq!(row.author.as_ref().unwrap().login, "ShaxP");
    assert!(!row.author.as_ref().unwrap().is_bot);
    assert!(row.node_id.0.len() > 10, "a real node id");
    assert_eq!(row.subject_ref(), pr(34));
    assert_eq!(
        row.mergeable,
        Mergeable::Unknown,
        "GitHub stops computing mergeability once merged"
    );
    assert_eq!(
        (row.additions, row.deletions, row.changed_files),
        (1656, 63, 21)
    );

    // The rollup is the head commit's verdict alone: `runs` and the counts
    // are the detail's (`10-domain-model.md` §3.3).
    assert_eq!(row.checks.state, RollupState::Success);
    assert!(row.checks.runs.is_empty());
    assert_eq!(row.checks.passed, 0);

    // Nobody was asked to review, nobody did.
    assert!(!row.review.i_am_requested);
    assert_eq!(row.review.my_review, None);
    assert_eq!(row.review.decision, None);
    assert!(row.review.reviewers.is_empty());
    assert!(row.labels.is_empty());
}

// ------------------------------------------------------------------ the detail

#[tokio::test]
async fn a_merged_pull_request_opens_with_its_checks_and_timeline() {
    let (client, transport) = on_cassette("pull_request_detail");
    let d = client.pull_requests().detail(&pr(34)).await.unwrap();

    assert_eq!(d.pr.number, 34);
    assert_eq!(d.base_ref, "main");
    assert_eq!(d.head_ref, "feat/m2-pr-contract");
    assert!(d.body.source.contains("contract"), "the body came through");

    // The detail's rollup is built from the contexts, and carries them.
    assert_eq!(d.pr.checks.state, RollupState::Success);
    assert_eq!(d.pr.checks.runs.len(), 1);
    assert_eq!(d.pr.checks.runs[0].name, "build · clippy · test");
    assert_eq!(
        d.pr.checks.runs[0].conclusion,
        Some(CheckConclusion::Success)
    );
    assert_eq!((d.pr.checks.passed, d.pr.checks.failed), (1, 0));
    assert!(
        d.pr.checks.runs[0]
            .url
            .as_deref()
            .unwrap()
            .contains("/actions/runs/")
    );

    // Oldest first, and the kinds GitHub recorded — including both the
    // merge and the close it writes beside it.
    let kinds: Vec<&str> = d
        .timeline
        .iter()
        .map(|e| match &e.kind {
            TimelineKind::Commit { .. } => "commit",
            TimelineKind::Merged { .. } => "merged",
            TimelineKind::Closed { .. } => "closed",
            TimelineKind::CrossReferenced { .. } => "cross-referenced",
            TimelineKind::Other { kind } => kind.as_str(),
            _ => "?",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "commit",
            "merged",
            "closed",
            "HeadRefDeletedEvent",
            "cross-referenced"
        ]
    );
    let times: Vec<_> = d.timeline.iter().map(|e| e.at).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]));

    // What each carried.
    let TimelineKind::Commit {
        oid,
        message_headline,
        authored_by,
    } = &d.timeline[0].kind
    else {
        panic!()
    };
    assert!(oid.starts_with("6f4c1e7"));
    assert!(message_headline.starts_with("feat(model,store)"));
    assert_eq!(authored_by.as_ref().unwrap().login, "ShaxP");

    let TimelineKind::Merged { commit, base } = &d.timeline[1].kind else {
        panic!()
    };
    assert_eq!(base, "main");
    assert!(commit.as_deref().unwrap().starts_with("fca189b"));
    assert_eq!(d.timeline[1].actor.as_ref().unwrap().login, "ShaxP");

    // An unmodelled event still has its actor and its time.
    assert_eq!(d.timeline[3].actor.as_ref().unwrap().login, "ShaxP");
    assert_eq!(d.timeline[3].at, datetime!(2026-09-17 08:44:31 UTC));

    let TimelineKind::CrossReferenced { source, will_close } = &d.timeline[4].kind else {
        panic!()
    };
    assert_eq!(*source, pr(35));
    assert!(!will_close);

    // One request: the timeline fit in a page.
    assert_eq!(transport.request_count(), 1);
    let vars = sent_variables(&transport);
    assert_eq!(vars[0]["number"], 34);
    assert_eq!(vars[0]["full"], true);
    assert_eq!(vars[0]["timeline"], TIMELINE_PAGE);
}

#[tokio::test]
async fn a_number_no_pull_request_has_is_not_found() {
    let (client, _) = on_cassette("pull_request_not_found");
    let err = client
        .pull_requests()
        .detail(&pr(999_999))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn a_coordinate_that_is_not_a_pull_request_is_refused_before_any_request() {
    let (client, transport) = on_stub(vec![]);
    let issue = SubjectRef::parse_numbered("ShaxP/omaghy#1", SubjectKind::Issue).unwrap();
    assert!(client.pull_requests().detail(&issue).await.is_err());
    let sha = SubjectRef {
        owner: "ShaxP".into(),
        repo: "omaghy".into(),
        kind: SubjectKind::Commit,
        id: SubjectId::Sha("abc".into()),
    };
    assert!(client.pull_requests().detail(&sha).await.is_err());
    assert_eq!(transport.request_count(), 0);
}

// ------------------------------------------------------------------ constructed

fn who(login: &str) -> Value {
    json!({ "login": login, "avatarUrl": format!("https://avatars.githubusercontent.com/{login}"), "__typename": "User" })
}

/// A reviewed PR, as the live API shapes one. Built rather than recorded —
/// see the module docs.
fn reviewed_detail(timeline: Vec<Value>, has_next: bool) -> Value {
    json!({
        "data": { "repository": { "pullRequest": {
            "__typename": "PullRequest",
            "id": "PR_stub", "number": 7, "title": "Reviewed", "url": "https://github.com/o/r/pull/7",
            "state": "OPEN", "isDraft": false, "mergeable": "CONFLICTING",
            "createdAt": "2026-09-01T10:00:00Z", "updatedAt": "2026-09-02T10:00:00Z",
            "additions": 10, "deletions": 2, "changedFiles": 1, "totalCommentsCount": 3,
            "repository": { "owner": { "login": "o" }, "name": "r" },
            "author": who("alice"),
            "labels": { "nodes": [
                { "name": "bug", "color": "d73a4a", "description": "Something is not working" },
                { "name": "odd", "color": "not-a-colour", "description": "" }
            ] },
            "reviewDecision": "CHANGES_REQUESTED",
            "viewerLatestReviewRequest": { "id": "RR_1" },
            "viewerLatestReview": { "state": "COMMENTED" },
            "latestReviews": { "nodes": [
                { "author": who("bob"), "state": "CHANGES_REQUESTED" },
                { "author": { "login": "some-bot[bot]", "avatarUrl": null, "__typename": "Bot" }, "state": "COMMENTED" }
            ] },
            "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "FAILURE" } } } ] },
            "body": "Please look.",
            "baseRefName": "main", "headRefName": "fix",
            "checks": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "FAILURE", "contexts": { "nodes": [
                { "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://ci/1" },
                { "__typename": "CheckRun", "name": "lint", "status": "IN_PROGRESS", "conclusion": null, "detailsUrl": null },
                { "__typename": "StatusContext", "context": "codecov/patch", "state": "SUCCESS", "targetUrl": "https://cov/1" },
                { "__typename": "SomethingNew", "name": "ignored" }
            ] } } } } ] },
            "reviewThreads": { "nodes": [
                { "id": "T_1", "isResolved": false, "isOutdated": false, "path": "src/lib.rs", "line": 12,
                  "comments": { "nodes": [
                    { "author": who("bob"), "body": "Why?", "createdAt": "2026-09-01T12:00:00Z", "pullRequestReview": { "id": "PRR_bob" } },
                    { "author": who("alice"), "body": "Because.", "createdAt": "2026-09-01T12:30:00Z", "pullRequestReview": { "id": "PRR_alice_reply" } }
                  ] } },
                { "id": "T_2", "isResolved": true, "isOutdated": true, "path": "README.md", "line": null,
                  "comments": { "nodes": [
                    { "author": who("carol"), "body": "Typo.", "createdAt": "2026-09-01T13:00:00Z", "pullRequestReview": { "id": "PRR_not_on_this_page" } }
                  ] } }
            ] },
            "timelineItems": {
                "pageInfo": { "hasNextPage": has_next, "endCursor": if has_next { json!("next") } else { Value::Null } },
                "nodes": timeline
            }
        } } }
    })
}

fn first_page_events() -> Vec<Value> {
    vec![
        json!({ "__typename": "PullRequestCommit", "commit": { "oid": "abcdef1234567", "messageHeadline": "fix", "committedDate": "2026-09-01T10:00:00Z", "author": { "name": "Alice", "user": who("alice") } } }),
        json!({ "__typename": "ReviewRequestedEvent", "actor": who("alice"), "createdAt": "2026-09-01T10:01:00Z", "requestedReviewer": null }),
        json!({ "__typename": "ReviewRequestedEvent", "actor": who("alice"), "createdAt": "2026-09-01T10:01:01Z", "requestedReviewer": { "__typename": "Team", "name": "maintainers" } }),
        json!({ "__typename": "AddedToProjectV2Event" }),
        json!({ "__typename": "IssueComment", "id": "IC_1", "author": who("dave"), "body": "Nice", "createdAt": "2026-09-01T11:00:00Z", "lastEditedAt": "2026-09-01T11:05:00Z",
                "reactionGroups": [ { "content": "THUMBS_UP", "reactors": { "totalCount": 2 } }, { "content": "ROCKET", "reactors": { "totalCount": 1 } }, { "content": "EYES", "reactors": { "totalCount": 0 } } ] }),
        json!({ "__typename": "PullRequestReview", "id": "PRR_bob", "author": who("bob"), "state": "CHANGES_REQUESTED", "body": "Two things.", "createdAt": "2026-09-01T12:00:00Z" }),
        json!({ "__typename": "PullRequestReviewThread", "id": "T_1" }),
        json!({ "__typename": "PullRequestReview", "id": "PRR_alice_reply", "author": who("alice"), "state": "COMMENTED", "body": "", "createdAt": "2026-09-01T12:30:00Z" }),
    ]
}

#[tokio::test]
async fn reviews_threads_reactions_and_the_awkward_cases_translate() {
    let (client, transport) = on_stub(vec![reviewed_detail(first_page_events(), false)]);
    let d = client
        .pull_requests()
        .detail(&SubjectRef::pull_request(&RepoRef::new("o", "r"), 7))
        .await
        .unwrap();
    assert_eq!(transport.request_count(), 1);

    // The row's review summary is viewer-relative without knowing the viewer.
    assert!(d.pr.review.i_am_requested);
    assert_eq!(d.pr.review.my_review, Some(ReviewState::Commented));
    assert!(d.pr.review.awaits_me(), "a comment is not a verdict");
    assert_eq!(d.pr.review.reviewers.len(), 2);
    assert_eq!(d.pr.review.reviewers[0].0.login, "bob");
    assert!(d.pr.review.reviewers[1].0.is_bot);
    assert_eq!(d.pr.mergeable, Mergeable::Conflicting);

    // Labels: a colour GitHub would never send still yields a label.
    assert_eq!(d.pr.labels.len(), 2);
    assert_eq!(
        d.pr.labels[0].description.as_deref(),
        Some("Something is not working")
    );
    assert_eq!(d.pr.labels[1].description, None, "empty is absent");

    // The rollup is the model's precedence over the contexts, not GitHub's
    // aggregate: one failure, one pending, one legacy success; an unknown
    // context type is skipped.
    assert_eq!(d.pr.checks.state, RollupState::Failure);
    assert_eq!(
        (d.pr.checks.failed, d.pr.checks.pending, d.pr.checks.passed),
        (1, 1, 1)
    );
    assert_eq!(
        d.pr.checks.runs.len(),
        2,
        "runs are check runs; the status is counted, not listed"
    );

    // Threads attach to the review that opened them; the thread item itself
    // is dropped so it is not shown twice.
    let reviews: Vec<_> = d
        .timeline
        .iter()
        .filter_map(|e| match &e.kind {
            TimelineKind::Review { state, threads, .. } => Some((state, threads)),
            _ => None,
        })
        .collect();
    assert_eq!(reviews.len(), 2);
    assert_eq!(*reviews[0].0, ReviewState::ChangesRequested);
    assert_eq!(reviews[0].1.len(), 1, "bob's review owns the thread");
    assert_eq!(reviews[0].1[0].path, "src/lib.rs");
    assert_eq!(
        reviews[0].1[0].comments.len(),
        2,
        "both comments, the reply included"
    );
    assert!(reviews[1].1.is_empty(), "the reply's review owns nothing");

    // A thread whose review is not on the page keeps its place in time.
    let orphan = d
        .timeline
        .iter()
        .find(|e| matches!(&e.kind, TimelineKind::ReviewThread(t) if t.path == "README.md"))
        .expect("the orphan thread is an event of its own");
    assert_eq!(orphan.at, datetime!(2026-09-01 13:00 UTC));
    assert_eq!(orphan.actor.as_ref().unwrap().login, "carol");
    assert!(
        matches!(&orphan.kind, TimelineKind::ReviewThread(t) if t.is_resolved && t.is_outdated)
    );

    // Reactions and edits on a comment.
    let comment = d
        .timeline
        .iter()
        .find_map(|e| match &e.kind {
            TimelineKind::Comment {
                reactions, edited, ..
            } => Some((reactions, edited)),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (comment.0.thumbs_up, comment.0.rocket, comment.0.eyes),
        (2, 1, 0)
    );
    assert_eq!(comment.0.total(), 3);
    assert!(comment.1);

    // A null reviewer is an `Other`, not a ghost; a team is named.
    let requests: Vec<_> = d
        .timeline
        .iter()
        .filter_map(|e| match &e.kind {
            TimelineKind::ReviewRequested { who } => Some(Some(who.login.as_str())),
            TimelineKind::Other { kind } if kind == "ReviewRequestedEvent" => Some(None),
            _ => None,
        })
        .collect();
    assert_eq!(requests, [None, Some("maintainers")]);

    // The project-gated event arrives bare and takes the previous event's
    // time rather than vanishing.
    let gated = d
        .timeline
        .iter()
        .find(
            |e| matches!(&e.kind, TimelineKind::Other { kind } if kind == "AddedToProjectV2Event"),
        )
        .unwrap();
    assert_eq!(gated.actor, None);
    assert_eq!(gated.at, datetime!(2026-09-01 10:01:01 UTC));

    // The commit's author is the user behind it.
    assert!(matches!(
        &d.timeline[0].kind,
        TimelineKind::Commit { authored_by: Some(a), .. } if a.login == "alice"
    ));

    // No thread item survives, and the order is by time throughout.
    assert_eq!(
        d.timeline.len(),
        8,
        "seven items kept, the thread item dropped, one orphan added"
    );
    let times: Vec<_> = d.timeline.iter().map(|e| e.at).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]));
}

#[tokio::test]
async fn a_long_timeline_is_followed_page_by_page() {
    let second = vec![
        json!({ "__typename": "ReopenedEvent", "actor": who("alice"), "createdAt": "2026-09-03T10:00:00Z" }),
    ];
    let (client, transport) = on_stub(vec![
        reviewed_detail(first_page_events(), true),
        reviewed_detail(second, false),
    ]);
    let d = client
        .pull_requests()
        .detail(&SubjectRef::pull_request(&RepoRef::new("o", "r"), 7))
        .await
        .unwrap();
    assert_eq!(transport.request_count(), 2);
    assert!(matches!(
        d.timeline.last().unwrap().kind,
        TimelineKind::Reopened
    ));

    // The continuation asked for the timeline only.
    let bodies: Vec<Value> = transport
        .requests()
        .iter()
        .map(|r| serde_json::from_slice(r.body.as_deref().unwrap_or_default()).unwrap())
        .collect();
    assert_eq!(bodies[0]["variables"]["full"], true);
    assert_eq!(bodies[1]["variables"]["full"], false);
    assert_eq!(bodies[1]["variables"]["after"], "next");
}

#[tokio::test]
async fn a_timeline_beyond_the_cap_stops_fetching_and_keeps_what_it_has() {
    let page = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|i| json!({ "__typename": "SubscribedEvent", "actor": who("x"), "createdAt": format!("2026-09-01T10:{:02}:00Z", i % 60) }))
            .collect()
    };
    let pages_needed = TIMELINE_CAP / usize::from(TIMELINE_PAGE);
    let mut responses: Vec<Value> = (0..pages_needed)
        .map(|_| reviewed_detail(page(usize::from(TIMELINE_PAGE)), true))
        .collect();
    // One more than the cap allows, which must never be asked for.
    responses.push(reviewed_detail(page(1), false));

    let (client, transport) = on_stub(responses);
    let d = client
        .pull_requests()
        .detail(&SubjectRef::pull_request(&RepoRef::new("o", "r"), 7))
        .await
        .unwrap();
    assert_eq!(transport.request_count(), pages_needed);
    // Every event fetched is kept (plus the two threads from the stub).
    assert_eq!(d.timeline.len(), TIMELINE_CAP + 2);
}

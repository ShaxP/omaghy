//! Notifications, end to end, against recorded responses.
//!
//! **No test here opens a socket** (`spec/00-overview.md` §7). Every one runs
//! against a cassette recorded from `api.github.com` by
//! `examples/record_cassettes.rs`, or a stub built in the test — see
//! `omaghy_api::cassette` for why the two are kept distinct.
//!
//! Several of these assert things about GitHub's *shapes* rather than about
//! our logic. That is the point: `omaghy-model`'s serde attributes were
//! written before anything had deserialized a real response, and this is the
//! first code that does.

use omaghy_api::cassette::{CassetteTransport, RecordingSleeper, StubTransport};
use omaghy_api::{
    ClientConfig, Clock, Conditional, GitHubClient, HttpResponse, NotificationFilter, Token,
    TransportError, Validators,
};
use omaghy_model::{
    Enrichment, LimitKind, Notification, NotificationId, NotificationReason, PrDisplayStatus,
    RepoRef, RollupState, StoreError, SubjectId, SubjectKind, SubjectRef,
};
use std::sync::Arc;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

const NOW: OffsetDateTime = datetime!(2026-09-13 06:00 UTC);

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
    let client = GitHubClient::with_config(token(), transport.clone(), config());
    (client, transport)
}

fn on_stub(stub: Arc<StubTransport>) -> GitHubClient {
    GitHubClient::with_config(token(), stub, config())
        .with_sleeper(Arc::new(RecordingSleeper::new()))
}

/// The filter `notifications_page.json` was recorded with.
fn recorded_filter() -> NotificationFilter {
    NotificationFilter::default()
        .in_repo(RepoRef::new("ShaxP", "shax"))
        .per_page(3)
}

// ---- the translation ------------------------------------------------------

#[tokio::test]
async fn a_recorded_page_becomes_our_own_vocabulary() {
    let (client, _) = on_cassette("notifications_page");
    let page = client
        .notifications()
        .list(&recorded_filter(), Validators::none())
        .await
        .unwrap()
        .modified()
        .expect("a first fetch is never a 304");

    assert_eq!(page.items.len(), 3);
    let first = &page.items[0];

    assert_eq!(first.id, NotificationId("24555446034".into()));
    assert_eq!(first.reason, NotificationReason::Author);
    assert_eq!(first.kind, SubjectKind::PullRequest);
    assert_eq!(first.repo, RepoRef::new("ShaxP", "shax"));
    assert_eq!(
        first.title,
        "fix: syntax highlighting follows the Dark/Light/System toggle"
    );
    assert_eq!(first.updated_at, datetime!(2026-07-10 16:04:40 UTC));
    assert!(!first.unread);

    // Parsed from the API URL, with no second request: this is what makes an
    // unenriched row openable.
    let subject = first.subject.as_ref().expect("a pull request URL parses");
    assert_eq!(subject.id, SubjectId::Number(61));
    assert_eq!(
        first.browser_url().as_deref(),
        Some("https://github.com/ShaxP/shax/pull/61")
    );

    // Nothing is enriched yet, and that is the honest state of a first paint.
    assert!(
        page.items.iter().all(|n| n.detail == Enrichment::Absent),
        "the REST payload carries none of the detail"
    );
}

#[tokio::test]
async fn the_subject_type_on_the_wire_is_pascal_case_and_the_model_does_not_know_it() {
    // Recorded: `"type": "PullRequest"`. `SubjectKind` carries
    // `rename_all = "snake_case"`, which is the cache's representation and not
    // GitHub's — so the translation in `omaghy_api::notifications` is load
    // bearing rather than decorative. This test fails the day somebody
    // "simplifies" it into a derive.
    assert!(
        serde_json::from_str::<SubjectKind>("\"PullRequest\"").is_err(),
        "if this starts passing, the model's serde changed and the mapping should be revisited"
    );
    assert_eq!(
        serde_json::from_str::<SubjectKind>("\"pull_request\"").unwrap(),
        SubjectKind::PullRequest
    );

    let (client, _) = on_cassette("notifications_page");
    let page = client
        .notifications()
        .list(&recorded_filter(), Validators::none())
        .await
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(page.items[0].kind, SubjectKind::PullRequest);
}

#[tokio::test]
async fn a_reason_github_invents_tomorrow_survives_as_other() {
    // `NotificationReason::Other` is a newtype variant, which serde's default
    // enum representation reads from a *map*, not a bare string. A derived
    // `Deserialize` would therefore reject an unknown reason outright rather
    // than keeping it — which is the whole reason the mapping is hand-written.
    assert!(serde_json::from_str::<NotificationReason>("\"security_alert\"").is_ok());
    assert!(
        serde_json::from_str::<NotificationReason>("\"a_reason_from_2028\"").is_err(),
        "the model cannot parse an unknown reason; omaghy-api must"
    );

    let client = on_stub(Arc::new(StubTransport::always(
        HttpResponse::new(200).body(page_body(
            r#""reason": "a_reason_from_2028", "subject": {"title":"t","url":null,"type":"Sponsorship"}"#,
        )),
    )));
    let page = client
        .notifications()
        .list(&NotificationFilter::default(), Validators::none())
        .await
        .unwrap()
        .modified()
        .unwrap();

    assert_eq!(
        page.items[0].reason,
        NotificationReason::Other("a_reason_from_2028".into())
    );
    assert_eq!(
        page.items[0].kind,
        SubjectKind::Other("Sponsorship".into()),
        "an unmodelled subject type renders rather than vanishing"
    );
}

#[tokio::test]
async fn a_subject_with_no_url_still_yields_a_row() {
    // A check-suite notification carries `"url": null`. The row must render;
    // it just cannot be opened.
    let client = on_stub(Arc::new(StubTransport::always(
        HttpResponse::new(200).body(page_body(
            r#""reason": "ci_activity", "subject": {"title":"CI failed on main","url":null,"type":"CheckSuite"}"#,
        )),
    )));
    let page = client
        .notifications()
        .list(&NotificationFilter::default(), Validators::none())
        .await
        .unwrap()
        .modified()
        .unwrap();

    let row = &page.items[0];
    assert_eq!(row.kind, SubjectKind::CheckSuite);
    assert_eq!(row.subject, None);
    assert_eq!(row.browser_url(), None);
    assert_eq!(row.title, "CI failed on main");
}

/// One notification, with the caller supplying the fields that vary.
fn page_body(fields: &str) -> Vec<u8> {
    format!(
        r#"[{{"id":"1","unread":true,"updated_at":"2026-07-10T16:04:40Z",{fields},
            "repository":{{"full_name":"ShaxP/shax","name":"shax","private":false,
                           "owner":{{"login":"ShaxP"}}}}}}]"#
    )
    .into_bytes()
}

// ---- what makes polling affordable ---------------------------------------

#[tokio::test]
async fn a_poll_that_changed_nothing_costs_nothing() {
    let (client, transport) = on_cassette("notifications_page");
    let filter = recorded_filter();

    let first = client
        .notifications()
        .list(&filter, Validators::none())
        .await
        .unwrap()
        .modified()
        .unwrap();
    let spent_after_first = client.rate_limits().rest.unwrap().used;

    let second = client
        .notifications()
        .list(&filter, first.validators.clone())
        .await
        .unwrap();

    assert!(
        second.is_not_modified(),
        "the second poll must be answered 304, not with an empty list"
    );
    assert_eq!(
        client.rate_limits().rest.unwrap().used,
        spent_after_first,
        "a 304 spends no REST budget — the whole argument for storing a validator"
    );

    // And the conditional header actually went out, rather than the 304 being
    // a coincidence of the fixture.
    let sent = transport.requests();
    assert_eq!(sent.len(), 2);
    assert!(sent[1].headers.get("if-none-match").is_some());
    assert!(sent[1].headers.get("if-modified-since").is_some());
}

#[tokio::test]
async fn the_validator_survives_the_304_even_though_its_form_changes() {
    // Recorded, and worth knowing: this endpoint answers a 200 with a **weak**
    // ETag (`W/"…"`) and the matching 304 with the **strong** form of the same
    // value. Verified against the live API that GitHub accepts either on the
    // way back, so keeping the newer one — as `merged_with` does — does not
    // quietly turn a free poll into a paid one.
    let (client, _) = on_cassette("notifications_page");
    let filter = recorded_filter();

    let first = client
        .notifications()
        .list(&filter, Validators::none())
        .await
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        first.validators.etag.as_deref().unwrap().starts_with("W/"),
        "got {:?}",
        first.validators.etag
    );
    assert!(
        first.validators.last_modified.is_some(),
        "this endpoint does send Last-Modified, unlike an empty inbox"
    );

    let Conditional::NotModified { validators } = client
        .notifications()
        .list(&filter, first.validators)
        .await
        .unwrap()
    else {
        panic!("expected a 304");
    };
    assert!(!validators.etag.as_deref().unwrap().starts_with("W/"));
    assert!(validators.last_modified.is_some());
}

#[tokio::test]
async fn the_poll_interval_github_asks_for_is_honoured() {
    let (client, _) = on_cassette("notifications_page");
    let page = client
        .notifications()
        .list(&recorded_filter(), Validators::none())
        .await
        .unwrap()
        .modified()
        .unwrap();

    // Recorded: `X-Poll-Interval: 60`. Ignoring it earns a secondary limit.
    assert_eq!(page.poll_interval, Duration::seconds(60));
    assert_eq!(
        client.rate_limits().poll_interval,
        Some(Duration::seconds(60))
    );
}

#[tokio::test]
async fn a_continuing_inbox_says_where_it_continues() {
    let (client, _) = on_cassette("notifications_page");
    let page = client
        .notifications()
        .list(&recorded_filter(), Validators::none())
        .await
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        page.next_page.as_deref().unwrap().contains("page=2"),
        "got {:?}",
        page.next_page
    );
}

// ---- enrichment -----------------------------------------------------------

/// The four subjects `notifications_enrichment.json` was recorded against:
/// three real pull requests and one in a repository that does not exist.
fn recorded_subjects() -> Vec<Notification> {
    let mut items: Vec<Notification> = [61u64, 60, 59]
        .into_iter()
        .map(|n| {
            subject_notification(
                &format!("245554460{n}"),
                &format!("https://api.github.com/repos/ShaxP/shax/pulls/{n}"),
                SubjectKind::PullRequest,
            )
        })
        .collect();
    items.push(subject_notification(
        "0",
        "https://api.github.com/repos/ShaxP/this-repo-does-not-exist-omaghy-fixture/pulls/1",
        SubjectKind::PullRequest,
    ));
    items
}

fn subject_notification(id: &str, url: &str, kind: SubjectKind) -> Notification {
    Notification {
        id: NotificationId(id.to_owned()),
        unread: true,
        reason: NotificationReason::Author,
        updated_at: datetime!(2026-07-10 16:04:40 UTC),
        title: "t".into(),
        kind,
        repo: RepoRef::new("ShaxP", "shax"),
        subject: SubjectRef::from_api_url(url),
        detail: Enrichment::Absent,
    }
}

#[tokio::test]
async fn one_query_enriches_a_whole_page() {
    let (client, transport) = on_cassette("notifications_enrichment");
    let mut items = recorded_subjects();

    client.notifications().enrich(&mut items).await.unwrap();

    assert_eq!(
        transport.request_count(),
        1,
        "a page is one query, not one query per row"
    );

    let detail = items[0].detail.ready().expect("enriched");
    assert_eq!(detail.number, 61);
    // `PrState`'s SCREAMING_SNAKE_CASE meets a real GraphQL response here:
    // GitHub answered `"MERGED"`.
    assert_eq!(detail.status, PrDisplayStatus::Merged);
    assert_eq!(detail.html_url, "https://github.com/ShaxP/shax/pull/61");
    assert_eq!(detail.checks.state, RollupState::Success);

    let actor = detail.last_actor.as_ref().expect("a last actor");
    assert_eq!(actor.login, "cursor");
    assert!(actor.is_bot, "__typename: Bot is how a bot is told apart");

    // The enriched URL is what `o` now opens.
    assert_eq!(
        items[0].browser_url().as_deref(),
        Some("https://github.com/ShaxP/shax/pull/61")
    );
}

#[tokio::test]
async fn one_unreachable_subject_does_not_fail_the_other_three() {
    // Recorded: HTTP 200 carrying both data and a NOT_FOUND for alias `s3`.
    // `GitHubClient::graphql` would call that a failure — correctly, for a
    // single-subject query. A batch is the case where it is not.
    let (client, _) = on_cassette("notifications_enrichment");
    let mut items = recorded_subjects();
    client.notifications().enrich(&mut items).await.unwrap();

    assert_eq!(
        items.iter().filter(|n| n.detail.ready().is_some()).count(),
        3
    );
    match &items[3].detail {
        Enrichment::Failed { reason } => assert!(
            reason.contains("Could not resolve"),
            "GitHub's own words, so the UI can say why: {reason}"
        ),
        other => panic!("expected a permanent failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_repository_we_lost_access_to_is_never_asked_about_twice() {
    let (client, transport) = on_cassette("notifications_enrichment");
    let mut items = recorded_subjects();

    client.notifications().enrich(&mut items).await.unwrap();
    client.notifications().enrich(&mut items).await.unwrap();

    assert_eq!(
        transport.request_count(),
        1,
        "Failed is terminal; re-asking on every open is how a lost-access repo \
         burns the rate limit"
    );
    assert!(items.iter().all(|n| n.detail.is_settled()));
}

#[tokio::test]
async fn an_in_flight_row_is_not_scheduled_a_second_time() {
    let (client, transport) = on_cassette("notifications_enrichment");
    let mut items = recorded_subjects();
    for n in &mut items {
        n.detail = Enrichment::Pending;
    }

    client.notifications().enrich(&mut items).await.unwrap();

    assert_eq!(
        transport.request_count(),
        0,
        "Pending means already in flight"
    );
    assert!(items.iter().all(|n| n.detail == Enrichment::Pending));
}

#[tokio::test]
async fn a_subject_with_no_number_is_settled_rather_than_re_asked() {
    // A commit is addressed by SHA, and `SubjectDetail.number` cannot hold
    // one. Leaving it `Absent` would mean asking again on every open, forever.
    let (client, transport) = on_cassette("notifications_enrichment");
    let mut items = vec![subject_notification(
        "7",
        "https://api.github.com/repos/ShaxP/shax/commits/9e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f",
        SubjectKind::Commit,
    )];

    client.notifications().enrich(&mut items).await.unwrap();

    assert_eq!(transport.request_count(), 0, "nothing to ask");
    assert!(items[0].detail.is_settled());
    assert!(!items[0].detail.wants_fetch());
}

#[tokio::test]
async fn a_request_that_never_landed_leaves_the_rows_askable_again() {
    let stub = Arc::new(StubTransport::failing(TransportError::Unreachable(
        "dns error: no record found".to_owned(),
    )));
    let client = on_stub(stub);
    let mut items = recorded_subjects();

    let e = client.notifications().enrich(&mut items).await.unwrap_err();
    assert!(matches!(e, StoreError::Offline(_)));
    assert!(
        items.iter().all(|n| n.detail == Enrichment::Absent),
        "a row stranded in Pending would never be fetched again"
    );
}

#[tokio::test]
async fn a_whole_batch_failing_is_an_error_not_fifty_failed_rows() {
    // `data: null` is how a rate limit, a bad token and an unparseable query
    // all arrive. None of them is news about a particular subject, so none of
    // them may be written into a row as a permanent failure.
    let stub = Arc::new(StubTransport::always(HttpResponse::new(200).body(
        br#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#
            .to_vec(),
    )));
    let client = on_stub(stub);
    let mut items = recorded_subjects();

    let e = client.notifications().enrich(&mut items).await.unwrap_err();
    assert!(matches!(
        e,
        StoreError::RateLimited {
            kind: LimitKind::Primary,
            ..
        }
    ));
    assert!(items.iter().all(|n| n.detail == Enrichment::Absent));
}

#[tokio::test]
async fn two_rows_naming_one_subject_ask_about_it_once() {
    let stub = Arc::new(StubTransport::always(
        HttpResponse::new(200).body(
            br#"{"data":{"s0":{"issueOrPullRequest":{"__typename":"Issue","number":7,
             "url":"https://github.com/ShaxP/shax/issues/7","state":"OPEN",
             "author":{"login":"ShaxP","__typename":"User"},"comments":{"nodes":[]}}}}}"#
                .to_vec(),
        ),
    ));
    let client = on_stub(stub.clone());

    // The same issue, reached as both an author notification and a mention.
    let url = "https://api.github.com/repos/ShaxP/shax/issues/7";
    let mut items = vec![
        subject_notification("1", url, SubjectKind::Issue),
        subject_notification("2", url, SubjectKind::Issue),
    ];
    client.notifications().enrich(&mut items).await.unwrap();

    let body = String::from_utf8(stub.requests()[0].body.clone().unwrap()).unwrap();
    assert!(body.contains("s0:"), "{body}");
    assert!(!body.contains("s1:"), "one alias answers both rows: {body}");

    for n in &items {
        let detail = n.detail.ready().expect("both rows are enriched");
        assert_eq!(detail.number, 7);
        assert_eq!(detail.status, PrDisplayStatus::Open);
        assert_eq!(
            detail.checks.state,
            RollupState::None,
            "an issue has no CI, which is not the same as CI that failed"
        );
    }
}

// ---- mutations ------------------------------------------------------------

#[tokio::test]
async fn marking_an_already_read_thread_does_not_error() {
    // Recorded against a thread that was already read, so the fixture is safe
    // to re-record: GitHub answers 205 Reset Content either way.
    let (client, transport) = on_cassette("notifications_mark_read");
    client
        .notifications()
        .mark_read(&[NotificationId("24555446034".into())])
        .await
        .expect("idempotent, per spec/20-store.md §5");

    let sent = transport.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, omaghy_api::Method::Patch);
    assert_eq!(
        sent[0].path_and_query(),
        "/notifications/threads/24555446034"
    );
}

#[tokio::test]
async fn a_thread_that_has_since_vanished_is_not_a_failure() {
    // Marking a batch read must not fail wholesale because one thread was
    // deleted between the poll and the keypress.
    let stub = Arc::new(StubTransport::always(
        HttpResponse::new(404).body(br#"{"message":"Not Found"}"#.to_vec()),
    ));
    let client = on_stub(stub.clone());
    client
        .notifications()
        .mark_read(&[NotificationId("1".into()), NotificationId("2".into())])
        .await
        .unwrap();
    assert_eq!(stub.request_count(), 2, "the second is still attempted");
}

#[tokio::test]
async fn a_mutation_that_really_failed_is_reported() {
    let stub = Arc::new(StubTransport::always(
        HttpResponse::new(403).body(br#"{"message":"Forbidden"}"#.to_vec()),
    ));
    let client = on_stub(stub.clone());
    let e = client
        .notifications()
        .mark_read(&[NotificationId("1".into())])
        .await
        .unwrap_err();
    assert_eq!(e, StoreError::Forbidden);
    assert_eq!(
        stub.request_count(),
        1,
        "a mutation is never retried, whatever the failure"
    );
}

#[tokio::test]
async fn marking_unread_never_leaves_the_process() {
    // GitHub has no mark-as-unread verb: verified live, `PATCH` with
    // `{"unread": true}` answers 205 and leaves the thread read. So this must
    // send nothing at all rather than pretend.
    let stub = Arc::new(StubTransport::always(HttpResponse::new(500)));
    let client = on_stub(stub.clone());
    client
        .notifications()
        .mark_unread(&[NotificationId("1".into())])
        .await
        .unwrap();
    assert_eq!(stub.request_count(), 0);
}

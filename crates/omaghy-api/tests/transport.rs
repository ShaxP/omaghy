//! The transport, end to end, against recorded responses.
//!
//! **No test here opens a socket** (`spec/00-overview.md` §7). Every one of
//! them runs against either a cassette recorded from `api.github.com` by
//! `examples/record_cassettes.rs`, or a stub built in the test — see
//! `omaghy_api::cassette` for why the two are kept distinct.

use omaghy_api::cassette::{CassetteTransport, RecordingSleeper, StubTransport};
use omaghy_api::{
    API_VERSION, ClientConfig, Clock, Conditional, GitHubClient, GraphQlRequest, HttpResponse,
    Method, RestRequest, Token, TransportError,
};
use omaghy_model::{AuthError, LimitKind, StoreError};
use serde::Deserialize;
use std::sync::Arc;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

const NOW: OffsetDateTime = datetime!(2026-09-12 08:00 UTC);
const SECRET: &str = "gho_atokenthatmustnevershowup";

fn token() -> Token {
    Token::new(SECRET).expect("a non-empty token")
}

fn config() -> ClientConfig {
    ClientConfig {
        clock: Clock::Fixed(NOW),
        ..ClientConfig::default()
    }
}

/// A client whose backoff is observable rather than endured.
fn with_stub(stub: Arc<StubTransport>) -> (GitHubClient, Arc<RecordingSleeper>) {
    let sleeper = Arc::new(RecordingSleeper::new());
    let client = GitHubClient::with_config(token(), stub, config()).with_sleeper(sleeper.clone());
    (client, sleeper)
}

fn on_cassette(name: &str) -> (GitHubClient, Arc<CassetteTransport>) {
    let transport = Arc::new(CassetteTransport::load(name));
    let client = GitHubClient::with_config(token(), transport.clone(), config());
    (client, transport)
}

// ---- what every request carries -----------------------------------------

#[tokio::test]
async fn every_request_carries_the_headers_github_requires() {
    let (client, transport) = on_cassette("rate_limit");
    client.rest(RestRequest::get("/rate_limit")).await.unwrap();

    let sent = transport.requests();
    let headers = &sent[0].headers;

    // GitHub rejects a request with no User-Agent outright.
    assert!(
        headers.get("user-agent").unwrap().starts_with("omaghy/"),
        "got {:?}",
        headers.get("user-agent")
    );
    assert_eq!(headers.get("accept"), Some("application/vnd.github+json"));
    // Pinned: an unpinned client changes behaviour without a deploy.
    assert_eq!(headers.get("x-github-api-version"), Some(API_VERSION));
    assert_eq!(
        headers.get("authorization"),
        Some(format!("Bearer {SECRET}").as_str())
    );
    assert_eq!(sent[0].url, "https://api.github.com/rate_limit");
}

#[tokio::test]
async fn the_token_is_redacted_everywhere_it_could_be_logged() {
    let (client, _) = on_cassette("rate_limit");
    // A TUI logs to a file (`spec/00-overview.md` §5) and `{:?}` on a client
    // is exactly what a panic hook or a tracing span would print.
    let printed = format!("{client:?}");
    assert!(!printed.contains(SECRET), "the token leaked: {printed}");
    assert!(printed.contains("<redacted>"), "got: {printed}");
}

// ---- conditional requests ------------------------------------------------

#[tokio::test]
async fn a_304_is_distinct_from_a_200_and_costs_no_rate_limit() {
    let (client, transport) = on_cassette("notifications_conditional");

    let first = client
        .rest(RestRequest::get("/notifications?per_page=1"))
        .await
        .unwrap();
    let Conditional::Modified(response) = first else {
        panic!("the first request should carry a body");
    };
    let etag = response
        .validators
        .etag
        .clone()
        .expect("GitHub sends an ETag on /notifications");
    let used_after_200 = client.rate_limits().rest.unwrap().used;

    let second = client
        .rest(RestRequest::get("/notifications?per_page=1").conditional(response.validators))
        .await
        .unwrap();

    assert!(
        second.is_not_modified(),
        "a repeat with the validator must not come back as a fresh body"
    );
    assert_eq!(
        transport.requests()[1].headers.get("if-none-match"),
        Some(etag.as_str()),
        "the stored validator has to actually be sent"
    );
    assert_eq!(
        client.rate_limits().rest.unwrap().used,
        used_after_200,
        "recorded from the live API: a 304 spends nothing, which is the whole \
         argument for polling conditionally"
    );

    // And the validator survives, so the *next* poll is conditional too.
    let Conditional::NotModified { validators } = second else {
        unreachable!()
    };
    assert_eq!(validators.etag.as_deref(), Some(etag.as_str()));
}

#[tokio::test]
async fn an_empty_list_is_not_a_304() {
    // The recorded inbox is `[]`. A caller must be able to tell "nothing there"
    // from "nothing changed" — one overwrites the cache, the other must not.
    let (client, _) = on_cassette("notifications_conditional");
    let first = client
        .rest(RestRequest::get("/notifications?per_page=1"))
        .await
        .unwrap();
    assert!(!first.is_not_modified());
    let body: Vec<serde_json::Value> = first.as_modified().unwrap().json().unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn the_poll_interval_is_read_and_floored() {
    let (client, _) = on_cassette("notifications_conditional");
    client
        .rest(RestRequest::get("/notifications?per_page=1"))
        .await
        .unwrap();
    let limits = client.rate_limits();
    assert_eq!(limits.poll_interval, Some(Duration::seconds(60)));
    assert!(limits.effective_poll_interval() >= Duration::seconds(60));
}

// ---- the two budgets -----------------------------------------------------

#[tokio::test]
async fn rest_and_graphql_budgets_are_tracked_separately() {
    let (rest_client, _) = on_cassette("rate_limit");
    rest_client
        .rest(RestRequest::get("/rate_limit"))
        .await
        .unwrap();
    let limits = rest_client.rate_limits();
    assert!(limits.rest.is_some(), "X-RateLimit-Resource said core");
    assert!(
        limits.graphql.is_none(),
        "a REST call must not move the points budget"
    );

    #[derive(Debug, Deserialize)]
    struct Data {
        viewer: Viewer,
    }
    #[derive(Debug, Deserialize)]
    struct Viewer {
        login: String,
    }

    let (gql_client, _) = on_cassette("graphql_viewer");
    let data: Data = gql_client
        .graphql(&GraphQlRequest::query(
            "query { viewer { login } rateLimit { limit cost remaining resetAt } }",
        ))
        .await
        .unwrap();
    assert_eq!(data.viewer.login, "ShaxP");

    let limits = gql_client.rate_limits();
    assert!(limits.graphql.is_some(), "recorded resource was graphql");
    assert!(
        limits.rest.is_none(),
        "points spent are not requests spent — 20-store.md §6"
    );
}

// ---- the error taxonomy, against real bodies -----------------------------

#[tokio::test]
async fn a_missing_repository_is_notfound() {
    let (client, _) = on_cassette("not_found");
    let e = client
        .rest(RestRequest::get(
            "/repos/ShaxP/this-repo-does-not-exist-omaghy-fixture",
        ))
        .await
        .unwrap_err();
    assert_eq!(e, StoreError::NotFound);
}

#[tokio::test]
async fn a_rejected_token_is_auth_and_says_what_to_run() {
    let (client, _) = on_cassette("unauthorized");
    let e = client.rest(RestRequest::get("/user")).await.unwrap_err();
    assert_eq!(e, StoreError::Auth(AuthError::Rejected));
    assert!(e.terse().contains("gh auth login"), "got: {}", e.terse());
}

#[tokio::test]
async fn a_graphql_200_carrying_errors_is_a_failure() {
    let (client, _) = on_cassette("graphql_not_found");
    let e = client
        .graphql::<serde_json::Value>(&GraphQlRequest::query(
            "query { repository(owner: \"ShaxP\", \
             name: \"this-repo-does-not-exist-omaghy-fixture\") { name } }",
        ))
        .await
        .unwrap_err();
    assert_eq!(
        e,
        StoreError::NotFound,
        "the status was 200; only the body says otherwise"
    );
}

#[tokio::test]
async fn an_unreachable_host_is_offline_not_upstream() {
    let stub = Arc::new(StubTransport::failing(TransportError::Unreachable(
        "dns error: failed to lookup address information".to_owned(),
    )));
    let (client, _) = with_stub(stub);
    let e = client
        .rest(RestRequest::get("/rate_limit"))
        .await
        .unwrap_err();

    assert!(matches!(e, StoreError::Offline(_)), "got {e:?}");
    assert!(
        e.keeps_cached_content(),
        "offline with a warm cache is a banner, not an empty screen"
    );
}

// ---- retry ---------------------------------------------------------------

#[tokio::test]
async fn a_5xx_is_retried_with_growing_backoff_then_reported() {
    let stub = Arc::new(StubTransport::always(HttpResponse::new(502)));
    let (client, sleeper) = with_stub(stub.clone());

    let e = client
        .rest(RestRequest::get("/rate_limit"))
        .await
        .unwrap_err();

    assert_eq!(stub.request_count(), 3, "three attempts, then give up");
    let waited = sleeper.slept();
    assert_eq!(
        waited,
        vec![Duration::milliseconds(500), Duration::seconds(1)],
        "the wait doubles between attempts"
    );
    assert!(matches!(e, StoreError::Upstream { status: 502, .. }));
}

#[tokio::test]
async fn a_mutation_is_never_retried_however_it_fails() {
    // A duplicated review is worse than a visible error, and GitHub gives us
    // no way to tell a lost response from a lost request.
    for stub in [
        Arc::new(StubTransport::always(HttpResponse::new(502))),
        Arc::new(StubTransport::failing(TransportError::Timeout(
            "timed out".to_owned(),
        ))),
    ] {
        let (client, sleeper) = with_stub(stub.clone());
        let _ = client
            .rest(RestRequest::patch("/notifications/threads/1").json(&serde_json::json!({})))
            .await;
        assert_eq!(stub.request_count(), 1, "exactly one attempt");
        assert!(sleeper.slept().is_empty(), "and no backoff to speak of");
    }
}

#[tokio::test]
async fn a_graphql_mutation_is_not_retried_even_though_a_query_would_be() {
    let stub = Arc::new(StubTransport::always(HttpResponse::new(503)));
    let (client, _) = with_stub(stub.clone());
    let _ = client
        .graphql::<serde_json::Value>(&GraphQlRequest::mutation(
            "mutation { addComment(input: {subjectId: \"x\", body: \"y\"}) { clientMutationId } }",
        ))
        .await;
    assert_eq!(
        stub.request_count(),
        1,
        "POST /graphql is a read most of the time, so the method cannot decide \
         this — the request says so itself"
    );

    let stub = Arc::new(StubTransport::always(HttpResponse::new(503)));
    let (client, _) = with_stub(stub.clone());
    let _ = client
        .graphql::<serde_json::Value>(&GraphQlRequest::query("query { viewer { login } }"))
        .await;
    assert_eq!(stub.request_count(), 3, "a query is safe to repeat");
}

#[tokio::test]
async fn a_404_is_not_retried() {
    let stub = Arc::new(StubTransport::always(
        HttpResponse::new(404).body(br#"{"message":"Not Found"}"#.to_vec()),
    ));
    let (client, _) = with_stub(stub.clone());
    let _ = client.rest(RestRequest::get("/repos/a/b")).await;
    assert_eq!(stub.request_count(), 1, "retrying cannot make it exist");
}

// ---- the governor gates what is sent -------------------------------------

#[tokio::test]
async fn a_secondary_limit_stops_the_next_request_before_it_is_sent() {
    let stub = Arc::new(StubTransport::always(
        HttpResponse::new(403)
            .header("x-ratelimit-remaining", "4200")
            .body(br#"{"message":"You have exceeded a secondary rate limit."}"#.to_vec()),
    ));
    let (client, sleeper) = with_stub(stub.clone());

    let first = client
        .rest(RestRequest::get("/rate_limit"))
        .await
        .unwrap_err();
    assert!(matches!(
        first,
        StoreError::RateLimited {
            kind: LimitKind::Secondary,
            ..
        }
    ));
    assert_eq!(stub.request_count(), 1, "a limit is not retried inline");
    assert!(
        sleeper.slept().is_empty(),
        "waiting it out inline would be indistinguishable from a hang"
    );

    let second = client
        .rest(RestRequest::get("/rate_limit"))
        .await
        .unwrap_err();
    assert!(matches!(second, StoreError::RateLimited { .. }));
    assert_eq!(
        stub.request_count(),
        1,
        "the second request never left: ignoring the penalty is what earns the \
         next one"
    );
}

#[tokio::test]
async fn an_exhausted_primary_budget_refuses_the_next_request() {
    let reset = (NOW + Duration::minutes(42)).unix_timestamp();
    let stub = Arc::new(StubTransport::always(
        HttpResponse::new(200)
            .header("x-ratelimit-resource", "core")
            .header("x-ratelimit-limit", "5000")
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-used", "5000")
            .header("x-ratelimit-reset", reset.to_string())
            .body(b"[]".to_vec()),
    ));
    let (client, _) = with_stub(stub.clone());

    // The request that spends the last of the budget still succeeds.
    client.rest(RestRequest::get("/rate_limit")).await.unwrap();
    assert_eq!(stub.request_count(), 1);

    match client.rest(RestRequest::get("/rate_limit")).await {
        Err(StoreError::RateLimited {
            kind: LimitKind::Primary,
            at,
        }) => assert_eq!(at, NOW + Duration::minutes(42)),
        other => panic!("expected a primary limit, got {other:?}"),
    }
    assert_eq!(stub.request_count(), 1, "nothing was sent");

    // The GraphQL budget is untouched, so a dashboard query still goes out.
    let _ = client
        .graphql::<serde_json::Value>(&GraphQlRequest::query("query { viewer { login } }"))
        .await;
    assert_eq!(stub.request_count(), 2, "points are not requests");
}

// ---- shape of what callers get back --------------------------------------

#[tokio::test]
async fn a_rest_body_deserialises_into_a_type_of_ours() {
    #[derive(Debug, Deserialize)]
    struct RateLimit {
        resources: Resources,
    }
    #[derive(Debug, Deserialize)]
    struct Resources {
        core: Bucket,
        graphql: Bucket,
    }
    #[derive(Debug, Deserialize)]
    struct Bucket {
        limit: u32,
    }

    let (client, _) = on_cassette("rate_limit");
    let response = client.rest(RestRequest::get("/rate_limit")).await.unwrap();
    let parsed: RateLimit = response.as_modified().unwrap().json().unwrap();
    assert_eq!(parsed.resources.core.limit, 5000);
    assert_eq!(parsed.resources.graphql.limit, 5000);
}

#[tokio::test]
async fn a_body_that_is_not_what_we_expected_says_so_rather_than_panicking() {
    let stub = Arc::new(StubTransport::always(
        HttpResponse::new(200).body(b"{\"resources\": \"not an object\"}".to_vec()),
    ));
    let (client, _) = with_stub(stub);
    let response = client.rest(RestRequest::get("/rate_limit")).await.unwrap();

    #[derive(Debug, Deserialize)]
    struct Expected {
        #[allow(dead_code)]
        resources: std::collections::BTreeMap<String, u32>,
    }
    let e = response
        .as_modified()
        .unwrap()
        .json::<Expected>()
        .unwrap_err();
    assert!(matches!(e, StoreError::Upstream { .. }), "got {e:?}");
}

#[tokio::test]
async fn a_request_for_something_the_cassette_does_not_hold_fails_loudly() {
    // The property that keeps "no test opens a socket" true: an unmatched
    // request is an error, never a fall-through to the network.
    let (client, _) = on_cassette("rate_limit");
    let e = client
        .rest(RestRequest::new(Method::Get, "/somewhere-else"))
        .await
        .unwrap_err();
    assert!(
        e.terse().contains("no recorded interaction"),
        "got: {}",
        e.terse()
    );
}

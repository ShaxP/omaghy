//! Search counts, against recorded responses.
//!
//! **No test here opens a socket** (`spec/00-overview.md` §7).
//!
//! The interesting assertions are about GitHub's behaviour rather than ours:
//! search answers a nonsense query with a number instead of an error, and a
//! dashboard built on it has to be honest about that.

use omaghy_api::cassette::{CassetteTransport, StubTransport};
use omaghy_api::{ClientConfig, Clock, GitHubClient, HttpResponse, Token};
use omaghy_model::StoreError;
use std::sync::Arc;
use time::OffsetDateTime;
use time::macros::datetime;

const NOW: OffsetDateTime = datetime!(2026-09-14 14:00 UTC);

fn config() -> ClientConfig {
    ClientConfig {
        clock: Clock::Fixed(NOW),
        ..ClientConfig::default()
    }
}

fn token() -> Token {
    Token::new("gho_atokenthatmustnevershowup").expect("a non-empty token")
}

fn on_cassette(name: &str) -> GitHubClient {
    GitHubClient::with_config(token(), Arc::new(CassetteTransport::load(name)), config())
}

/// The queries `search_counts.json` was recorded with, in order.
fn recorded_queries() -> Vec<String> {
    vec![
        "repo:ShaxP/omaghy is:open is:pr".to_owned(),
        "repo:ShaxP/omaghy is:closed is:pr".to_owned(),
        "repo:ShaxP/this-repo-does-not-exist-omaghy-fixture is:open".to_owned(),
    ]
}

#[tokio::test]
async fn a_batch_of_searches_answers_in_order() {
    let client = on_cassette("search_counts");
    let counts = client.search().counts(&recorded_queries()).await.unwrap();

    assert_eq!(counts.len(), 3, "one answer per query, positionally");
    assert_eq!(counts[0], Ok(0));
    assert_eq!(counts[1], Ok(26));
}

/// The finding this fixture exists for.
///
/// `repo:` naming a repository that does not exist is answered `0` with no
/// error at all. A dashboard section cannot tell a typo from an empty queue,
/// which is why the count is only ever as good as the query.
#[tokio::test]
async fn a_query_naming_nothing_real_is_zero_and_not_an_error() {
    let client = on_cassette("search_counts");
    let counts = client.search().counts(&recorded_queries()).await.unwrap();

    assert_eq!(
        counts[2],
        Ok(0),
        "GitHub does not reject an unresolvable repo: verified live while recording"
    );
}

#[tokio::test]
async fn no_queries_asks_nothing() {
    // A transport with nothing queued: zero sections must cost zero requests,
    // not an empty query GitHub would charge us a point for. Asking for one
    // would exhaust the sequence and fail the test.
    let client =
        GitHubClient::with_config(token(), Arc::new(StubTransport::sequence([])), config());
    assert_eq!(client.search().counts(&[]).await.unwrap(), Vec::new());
}

/// One alias failing is news about that section, not about the batch.
///
/// Never observed from search live — it does not reject queries — but the
/// batch shape makes it expressible, and the alternative is one unlucky
/// section blanking three good ones.
#[tokio::test]
async fn a_per_alias_error_does_not_lose_the_other_answers() {
    let body = br#"{
      "data": { "s0": {"issueCount": 7}, "s1": null },
      "errors": [{"type":"FORBIDDEN","path":["s1"],"message":"Resource not accessible"}]
    }"#;
    let stub = Arc::new(StubTransport::always(
        HttpResponse::new(200).body(body.to_vec()),
    ));
    let client = GitHubClient::with_config(token(), stub, config());

    let counts = client
        .search()
        .counts(&["one".to_owned(), "two".to_owned()])
        .await
        .unwrap();

    assert_eq!(counts[0], Ok(7));
    assert_eq!(counts[1], Err("Resource not accessible".to_owned()));
}

/// No `data` at all is not news about a field.
///
/// The same `FORBIDDEN` that means "this one section" when it carries a path
/// and a `data` alongside means "the whole request" when it does not.
#[tokio::test]
async fn an_error_with_no_data_fails_the_whole_call() {
    let body = br#"{"errors":[{"type":"FORBIDDEN","message":"Bad credentials"}]}"#;
    let stub = Arc::new(StubTransport::always(
        HttpResponse::new(200).body(body.to_vec()),
    ));
    let client = GitHubClient::with_config(token(), stub, config());

    let err = client
        .search()
        .counts(&["one".to_owned()])
        .await
        .expect_err("no data means the request failed, not the section");
    assert!(
        matches!(err, StoreError::Forbidden),
        "no data means the request failed as a whole: got {err:?}"
    );
}

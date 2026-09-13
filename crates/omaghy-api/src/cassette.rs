//! Recorded responses, and the stubs that stand in for what cannot be
//! recorded.
//!
//! **No test in this crate opens a socket** (`spec/00-overview.md` §7). There
//! is no `wiremock` and no local server; tests substitute a [`Transport`] and
//! the rest of the crate cannot tell.
//!
//! Two kinds of fixture, kept distinct on purpose:
//!
//! - **Cassettes** ([`CassetteTransport`]) are *recorded* — byte-for-byte what
//!   `api.github.com` returned, captured by `examples/record_cassettes.rs`.
//!   They are the ones that keep us honest about shapes we did not invent.
//! - **Stubs** ([`StubTransport`]) are *constructed*, for the responses we
//!   cannot ethically or practically provoke: a 500, a secondary rate limit, a
//!   DNS failure. Calling those "recordings" would be a lie, so they are built
//!   in the test that needs them and never committed as JSON.
//!
//! ## Recording is a public-repository-only operation
//!
//! This repository is public. A cassette may only ever contain data from a
//! public repository, and [`scrub`] enforces the header half of that by
//! **allowlist**: anything not in [`RECORDED_HEADERS`] is dropped, so
//! `Authorization`, `Set-Cookie` and anything else token-bearing cannot reach
//! a file by being forgotten. A denylist would have to anticipate every header
//! GitHub might add.
//!
//! This module is compiled into the library rather than behind `#[cfg(test)]`
//! because integration tests are a separate crate and W2.1 needs it too. It
//! costs a few hundred bytes and buys one fixture format for the whole crate.

use crate::retry::Sleeper;
use crate::transport::{Headers, HttpRequest, HttpResponse, Method, Transport, TransportError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use time::Duration;

/// The only headers a cassette may contain.
///
/// Every one of these is read by some part of this crate. The list is an
/// allowlist so that a header we have never seen — including a future
/// token-bearing one — cannot be recorded by default.
pub const RECORDED_HEADERS: &[&str] = &[
    "content-type",
    "etag",
    "last-modified",
    "link",
    "retry-after",
    "x-accepted-oauth-scopes",
    "x-github-api-version-selected",
    "x-github-media-type",
    "x-oauth-scopes",
    "x-poll-interval",
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "x-ratelimit-resource",
    "x-ratelimit-used",
];

/// Drop every header that is not on the allowlist.
pub fn scrub(headers: &Headers) -> Headers {
    headers
        .iter()
        .filter(|(name, _)| {
            RECORDED_HEADERS
                .iter()
                .any(|allowed| name.eq_ignore_ascii_case(allowed))
        })
        .map(|(n, v)| (n.to_ascii_lowercase(), v.to_owned()))
        .collect()
}

/// What a recorded request is matched on.
///
/// Deliberately not the whole request: the `Authorization` header is never
/// written to a cassette, and matching on `User-Agent` would make the fixtures
/// break on a version bump. `if_none_match` is here because it is the one
/// request header that changes the *answer* — it is what turns a 200 into a
/// 304.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedRequest {
    pub method: Method,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_none_match: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Headers,
    /// Kept as text so a cassette is readable in a diff, which is most of the
    /// point of committing one.
    #[serde(default)]
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    pub request: RecordedRequest,
    pub response: RecordedResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cassette {
    pub name: String,
    /// Why this one exists and what it is meant to prove.
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub recorded_at: String,
    pub interactions: Vec<Interaction>,
}

impl Cassette {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("malformed cassette: {e}"))
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        Self::from_json(&text)
    }

    /// Load `tests/cassettes/<name>.json` from this crate.
    ///
    /// Resolved against `CARGO_MANIFEST_DIR` rather than the working
    /// directory, so it works from an integration test, a unit test and
    /// another crate alike.
    ///
    /// Panics rather than returning a `Result`: the only callers are tests,
    /// and a missing fixture is a broken test rather than a condition to
    /// handle.
    #[must_use]
    pub fn load(name: &str) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/cassettes")
            .join(format!("{name}.json"));
        Self::from_path(&path).unwrap_or_else(|e| panic!("{e}"))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

fn matches(recorded: &RecordedRequest, actual: &HttpRequest) -> bool {
    recorded.method == actual.method
        && recorded.path == actual.path_and_query()
        && recorded.if_none_match.as_deref() == actual.headers.get("if-none-match")
}

/// Plays a cassette back.
///
/// Matching is by method, path and `If-None-Match`, in that order of
/// specificity — never by ordinal. A test that changes which request it makes
/// should fail by not matching, not by silently getting the next recording in
/// the file.
#[derive(Debug)]
pub struct CassetteTransport {
    cassette: Cassette,
    played: Mutex<Vec<usize>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl CassetteTransport {
    pub fn new(cassette: Cassette) -> Self {
        Self {
            cassette,
            played: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Load and play `tests/cassettes/<name>.json`.
    #[must_use]
    pub fn load(name: &str) -> Self {
        Self::new(Cassette::load(name))
    }

    /// Every request that was made, in order. What a test asserts headers on.
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .map_or_else(|e| e.into_inner().clone(), |r| r.clone())
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().map_or(0, |r| r.len())
    }
}

#[async_trait]
impl Transport for CassetteTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if let Ok(mut r) = self.requests.lock() {
            r.push(request.clone());
        }

        let mut played = self
            .played
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let candidates: Vec<usize> = self
            .cassette
            .interactions
            .iter()
            .enumerate()
            .filter(|(_, i)| matches(&i.request, &request))
            .map(|(idx, _)| idx)
            .collect();

        // Prefer one that has not been played, so a cassette holding a 200 and
        // then a 304 for the same path returns them in that order. Fall back
        // to the last match, so a retry test can hit the same 500 three times.
        let chosen = candidates
            .iter()
            .find(|idx| !played.contains(idx))
            .or_else(|| candidates.last())
            .copied()
            .ok_or_else(|| {
                TransportError::Malformed(format!(
                    "no recorded interaction in cassette `{}` for {} {} (if-none-match: {:?})",
                    self.cassette.name,
                    request.method,
                    request.path_and_query(),
                    request.headers.get("if-none-match"),
                ))
            })?;
        played.push(chosen);

        let recorded = &self.cassette.interactions[chosen].response;
        Ok(HttpResponse {
            status: recorded.status,
            headers: recorded.headers.clone(),
            body: recorded.body.clone().into_bytes(),
        })
    }
}

/// Responses built in the test that needs them.
///
/// For everything a recording cannot honestly provide: a 500, a secondary rate
/// limit, a DNS failure. Provoking those against the live API would mean
/// abusing it.
#[derive(Debug)]
pub struct StubTransport {
    queued: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
    last: Mutex<Option<Result<HttpResponse, TransportError>>>,
    repeat: bool,
    requests: Mutex<Vec<HttpRequest>>,
}

impl StubTransport {
    /// Answer with each response once, in order.
    pub fn sequence(
        responses: impl IntoIterator<Item = Result<HttpResponse, TransportError>>,
    ) -> Self {
        Self {
            queued: Mutex::new(responses.into_iter().collect()),
            last: Mutex::new(None),
            repeat: false,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Answer with the same response every time. What a retry test wants.
    pub fn always(response: HttpResponse) -> Self {
        Self {
            queued: Mutex::new(VecDeque::new()),
            last: Mutex::new(Some(Ok(response))),
            repeat: true,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Fail every time, without a response.
    pub fn failing(error: TransportError) -> Self {
        Self {
            queued: Mutex::new(VecDeque::new()),
            last: Mutex::new(Some(Err(error))),
            repeat: true,
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .map_or_else(|e| e.into_inner().clone(), |r| r.clone())
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().map_or(0, |r| r.len())
    }
}

#[async_trait]
impl Transport for StubTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if let Ok(mut r) = self.requests.lock() {
            r.push(request.clone());
        }

        let next = self
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front();

        match next {
            Some(response) => {
                if let Ok(mut slot) = self.last.lock() {
                    *slot = Some(response.clone());
                }
                response
            }
            None if self.repeat => self
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .unwrap_or_else(|| {
                    Err(TransportError::Malformed("stub has no response".to_owned()))
                }),
            None => Err(TransportError::Malformed(format!(
                "the stub ran out of responses at {} {}",
                request.method,
                request.path_and_query()
            ))),
        }
    }
}

/// A [`Sleeper`] that records what it was asked to wait for and waits for
/// none of it.
///
/// This is what makes backoff testable: the assertion is on the delays that
/// were *requested*, so the test proves the policy without spending it.
#[derive(Debug, Default)]
pub struct RecordingSleeper {
    slept: Mutex<Vec<Duration>>,
}

impl RecordingSleeper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn slept(&self) -> Vec<Duration> {
        self.slept
            .lock()
            .map_or_else(|e| e.into_inner().clone(), |s| s.clone())
    }

    pub fn total(&self) -> Duration {
        self.slept().into_iter().sum()
    }
}

#[async_trait]
impl Sleeper for RecordingSleeper {
    async fn sleep(&self, duration: Duration) {
        if let Ok(mut s) = self.slept.lock() {
            s.push(duration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubbing_drops_everything_it_was_not_told_to_keep() {
        let recorded: Headers = [
            ("Authorization", "Bearer gho_realtoken"),
            ("Set-Cookie", "logged_in=yes"),
            ("X-GitHub-Request-Id", "E874:4F478"),
            ("ETag", "\"abc\""),
            ("X-Poll-Interval", "60"),
        ]
        .into_iter()
        .collect();

        let safe = scrub(&recorded);
        assert_eq!(safe.get("etag"), Some("\"abc\""));
        assert_eq!(safe.get("x-poll-interval"), Some("60"));
        assert_eq!(safe.get("authorization"), None);
        assert_eq!(safe.get("set-cookie"), None);
        assert_eq!(
            safe.get("x-github-request-id"),
            None,
            "an allowlist drops headers nobody thought about"
        );
        assert_eq!(safe.len(), 2);
    }

    #[tokio::test]
    async fn a_cassette_matches_on_the_conditional_header() {
        let cassette = Cassette::from_json(
            r#"{
              "name": "t",
              "interactions": [
                {"request": {"method":"GET","path":"/x"},
                 "response": {"status":200,"headers":{"etag":"\"v1\""},"body":"[]"}},
                {"request": {"method":"GET","path":"/x","if_none_match":"\"v1\""},
                 "response": {"status":304,"headers":{"etag":"\"v1\""}}}
              ]
            }"#,
        )
        .unwrap();
        let t = CassetteTransport::new(cassette);

        let first = t
            .execute(HttpRequest::new(Method::Get, "https://api.github.com/x"))
            .await
            .unwrap();
        assert_eq!(first.status, 200);

        let second = t
            .execute(
                HttpRequest::new(Method::Get, "https://api.github.com/x")
                    .header("If-None-Match", "\"v1\""),
            )
            .await
            .unwrap();
        assert_eq!(second.status, 304);
    }

    #[tokio::test]
    async fn an_unrecorded_request_fails_loudly_rather_than_reaching_the_network() {
        let t = CassetteTransport::new(
            Cassette::from_json(r#"{"name":"t","interactions":[]}"#).unwrap(),
        );
        let e = t
            .execute(HttpRequest::new(Method::Get, "https://api.github.com/y"))
            .await
            .unwrap_err();
        assert!(
            e.to_string().contains("no recorded interaction"),
            "got: {e}"
        );
    }

    #[tokio::test]
    async fn a_stub_repeats_its_answer_so_a_retry_can_be_observed() {
        let t = StubTransport::always(HttpResponse::new(503));
        for _ in 0..3 {
            assert_eq!(
                t.execute(HttpRequest::new(Method::Get, "https://x/y"))
                    .await
                    .unwrap()
                    .status,
                503
            );
        }
        assert_eq!(t.request_count(), 3);
    }

    #[tokio::test]
    async fn a_sequence_stub_runs_out_rather_than_repeating() {
        let t = StubTransport::sequence([Ok(HttpResponse::new(200))]);
        assert!(
            t.execute(HttpRequest::new(Method::Get, "https://x/y"))
                .await
                .is_ok()
        );
        assert!(
            t.execute(HttpRequest::new(Method::Get, "https://x/y"))
                .await
                .is_err(),
            "an extra request is a test bug, not a repeat"
        );
    }

    #[tokio::test]
    async fn the_recording_sleeper_records_without_sleeping() {
        let s = RecordingSleeper::new();
        s.sleep(Duration::seconds(30)).await;
        s.sleep(Duration::seconds(60)).await;
        assert_eq!(
            s.slept(),
            vec![Duration::seconds(30), Duration::seconds(60)]
        );
        assert_eq!(s.total(), Duration::seconds(90));
    }
}

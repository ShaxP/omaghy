//! Record the cassettes in `tests/cassettes/`.
//!
//! ```text
//! cargo run -p omaghy-api --example record_cassettes
//! ```
//!
//! Uses whatever token the normal chain finds (`OMAGHY_TOKEN`, `GH_TOKEN`,
//! `gh auth token`) and never prints it.
//!
//! # This repository is public, so recording has rules
//!
//! - **Public repositories only.** No recording addresses a repository that
//!   exists privately, and the notification recording is *refused in code*
//!   unless the inbox comes back empty — an inbox with contents may name a
//!   private repository. That check is in code rather than in a comment,
//!   because a comment does not run.
//! - **Headers are scrubbed by allowlist.** `omaghy_api::cassette::scrub`
//!   keeps only the headers this crate reads, so `Authorization`,
//!   `Set-Cookie`, and anything token-bearing that GitHub adds in future
//!   cannot reach a file by being forgotten.
//!
//! Not everything can be recorded. A 500, a secondary rate limit and a DNS
//! failure are built in the test that needs them
//! (`omaghy_api::cassette::StubTransport`) — provoking them against the live
//! API would mean abusing it, and calling a fabrication a "recording" would be
//! a lie.

use async_trait::async_trait;
use omaghy_api::cassette::{Cassette, Interaction, RecordedRequest, RecordedResponse, scrub};
use omaghy_api::{
    ClientConfig, Conditional, GitHubClient, GraphQlRequest, HttpRequest, HttpResponse,
    ReqwestTransport, RestRequest, RetryPolicy, Token, Transport, TransportError, auth,
};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

/// The account the "does not exist" recordings address. Any public owner
/// works; using ours keeps the fixtures self-explanatory.
const OWNER: &str = "ShaxP";

type Boxed = Box<dyn std::error::Error>;

/// Wraps the real transport and keeps every exchange, so the cassette records
/// exactly what the client sent and what GitHub answered — rather than what a
/// second, hand-written copy of the header logic would have sent.
#[derive(Debug)]
struct RecordingTransport {
    inner: ReqwestTransport,
    captured: Mutex<Vec<Interaction>>,
}

impl RecordingTransport {
    fn new(inner: ReqwestTransport) -> Self {
        Self {
            inner,
            captured: Mutex::new(Vec::new()),
        }
    }

    /// Take everything captured so far, leaving the recorder empty.
    fn drain(&self) -> Vec<Interaction> {
        std::mem::take(&mut self.captured.lock().expect("recorder lock"))
    }
}

#[async_trait]
impl Transport for RecordingTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let recorded_request = RecordedRequest {
            method: request.method,
            path: request.path_and_query().to_owned(),
            if_none_match: request.headers.get("if-none-match").map(str::to_owned),
        };

        let response = self.inner.execute(request).await?;

        self.captured
            .lock()
            .expect("recorder lock")
            .push(Interaction {
                request: recorded_request,
                response: RecordedResponse {
                    status: response.status,
                    headers: scrub(&response.headers),
                    body: response.text().into_owned(),
                },
            });

        Ok(response)
    }
}

fn cassette_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes")
}

fn write(name: &str, note: &str, interactions: Vec<Interaction>) -> Result<(), Boxed> {
    let cassette = Cassette {
        name: name.to_owned(),
        note: note.to_owned(),
        recorded_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?,
        interactions,
    };
    let path = cassette_dir().join(format!("{name}.json"));
    std::fs::create_dir_all(cassette_dir())?;
    std::fs::write(&path, format!("{}\n", cassette.to_json()))?;
    println!("  wrote {}", path.display());
    Ok(())
}

/// Prints the error's own words rather than its `Debug`. The whole point of
/// the taxonomy is that each arm says something actionable; `Auth(Rejected)`
/// says none of it.
#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Boxed> {
    let resolved = auth::resolve_token()?;
    println!("token from {}", resolved.source.describe());

    let recorder = Arc::new(RecordingTransport::new(ReqwestTransport::new()?));
    let config = ClientConfig {
        // One attempt: a cassette should record what GitHub said, not what it
        // said on the third try.
        retry: RetryPolicy::none(),
        ..Default::default()
    };
    let client = GitHubClient::with_config(resolved.token, recorder.clone(), config.clone());

    record_rate_limit(&client, &recorder).await?;
    record_notifications(&client, &recorder).await?;
    record_graphql(&client, &recorder).await?;
    record_failures(&client, &recorder, config).await?;

    println!("done");
    Ok(())
}

async fn record_rate_limit(
    client: &GitHubClient,
    recorder: &RecordingTransport,
) -> Result<(), Boxed> {
    println!("GET /rate_limit");
    client.rest(RestRequest::get("/rate_limit")).await?;
    write(
        "rate_limit",
        "Both budgets in one body, and the X-RateLimit-* headers the governor \
         reads. `core` is requests, `graphql` is points.",
        recorder.drain(),
    )
}

async fn record_notifications(
    client: &GitHubClient,
    recorder: &RecordingTransport,
) -> Result<(), Boxed> {
    println!("GET /notifications, then again conditionally");

    let first = client
        .rest(RestRequest::get("/notifications?per_page=1"))
        .await?;
    let Conditional::Modified(response) = first else {
        return Err("the first request was answered 304; nothing to record".into());
    };

    // The guard that makes this safe to commit from a public repository: an
    // inbox with anything in it may name private repositories, so it is never
    // written. An empty inbox is a perfectly good fixture for the thing this
    // cassette exists to prove — that a 304 is distinct from a 200 and costs
    // no rate limit.
    let body = response.body.clone();
    if String::from_utf8_lossy(&body).trim() != "[]" {
        recorder.drain();
        println!(
            "  SKIPPED: this account's inbox is not empty, and a notification \
             may name a private repository. Re-record from an account with an \
             empty inbox, or hand-write the fixture."
        );
        return Ok(());
    }

    let second = client
        .rest(RestRequest::get("/notifications?per_page=1").conditional(response.validators))
        .await?;
    if !second.is_not_modified() {
        println!("  note: the second request was not answered 304");
    }

    write(
        "notifications_conditional",
        "A 200 and then the same request with If-None-Match, answered 304. \
         X-RateLimit-Used is identical across the pair: a 304 costs nothing, \
         which is what makes polling affordable. The empty body is deliberate \
         — an inbox with contents may name private repositories and is never \
         recorded from this public repository.",
        recorder.drain(),
    )
}

async fn record_graphql(client: &GitHubClient, recorder: &RecordingTransport) -> Result<(), Boxed> {
    println!("POST /graphql");
    let _: serde_json::Value = client
        .graphql(&GraphQlRequest::query(
            "query { viewer { login } rateLimit { limit cost remaining resetAt } }",
        ))
        .await?;
    write(
        "graphql_viewer",
        "The smallest useful query. Note X-RateLimit-Resource: graphql — the \
         same headers as REST, billed against a different budget — and that \
         `cost` is 1.",
        recorder.drain(),
    )?;

    println!("POST /graphql for a repository that does not exist");
    let failed: Result<serde_json::Value, _> = client
        .graphql(&GraphQlRequest::query(format!(
            "query {{ repository(owner: \"{OWNER}\", name: \
             \"this-repo-does-not-exist-omaghy-fixture\") {{ name }} }}"
        )))
        .await;
    println!("  -> {failed:?}");
    write(
        "graphql_not_found",
        "HTTP 200 with an `errors` array. A caller that only checked the \
         status would report success; this is why omaghy_api::graphql exists.",
        recorder.drain(),
    )
}

async fn record_failures(
    client: &GitHubClient,
    recorder: &RecordingTransport,
    config: ClientConfig,
) -> Result<(), Boxed> {
    println!("GET a repository that does not exist");
    let failed = client
        .rest(RestRequest::get(format!(
            "/repos/{OWNER}/this-repo-does-not-exist-omaghy-fixture"
        )))
        .await;
    println!("  -> {failed:?}");
    write(
        "not_found",
        "404 with GitHub's error envelope. Distinct from 403: this one is \
         gone, not forbidden.",
        recorder.drain(),
    )?;

    // A token that is not one, so the 401 is recorded rather than described.
    // This request carries no real credential — the machine's own token is not
    // involved, and the response body is GitHub's generic "Bad credentials".
    println!("GET /user with a token that is not one");
    let bad_recorder = Arc::new(RecordingTransport::new(ReqwestTransport::new()?));
    let bad = GitHubClient::with_config(
        Token::new("ghp_0000000000000000000000000000000000000")
            .ok_or("the placeholder token is empty")?,
        bad_recorder.clone(),
        config,
    );
    let unauthorized = bad.rest(RestRequest::get("/user")).await;
    println!("  -> {unauthorized:?}");
    write(
        "unauthorized",
        "401 from a token that is not one. Recorded rather than described, so \
         the error mapping is checked against GitHub's actual envelope. Note \
         that a 401 carries no X-RateLimit-* headers at all.",
        bad_recorder.drain(),
    )
}

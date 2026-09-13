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
//!   exists privately, and the whole-inbox recording is *refused in code*
//!   unless the inbox comes back empty — an inbox with contents may name a
//!   private repository. That check is in code rather than in a comment,
//!   because a comment does not run.
//! - **A real notification body comes from one repository, not the inbox.**
//!   `GET /repos/{owner}/{repo}/notifications` can only return that
//!   repository's threads, so "is this safe to commit" reduces to "is that
//!   repository public" — which the recorder asks GitHub rather than assuming,
//!   and refuses on anything but a clear yes. That is a structural guarantee;
//!   reading an inbox and deciding it looks fine is not one, which is why the
//!   guard above stays.
//! - **A recorded mutation must be a no-op.** The `mark_read` recording picks
//!   a thread that is already read, so re-recording it changes nothing on
//!   anybody's account. It refuses if there is no such thread rather than
//!   marking one read to get a fixture.
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
    NotificationFilter, ReqwestTransport, RestRequest, RetryPolicy, Token, Transport,
    TransportError, Validators, auth,
};
use omaghy_model::{
    Enrichment, Notification, NotificationId, NotificationReason, RepoRef, SubjectKind, SubjectRef,
};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

/// The account the "does not exist" recordings address. Any public owner
/// works; using ours keeps the fixtures self-explanatory.
const OWNER: &str = "ShaxP";

/// The repository the notification recordings are scoped to.
///
/// Must be **public**, and that is checked against the API rather than
/// asserted here — see [`record_notification_page`].
const NOTIFICATION_REPO: &str = "shax";

/// A repository that does not exist, so that a batch query has one alias
/// GitHub cannot resolve. That is the recording's whole point: it is what
/// proves a failed subject does not fail the other forty-nine.
const MISSING_REPO: &str = "this-repo-does-not-exist-omaghy-fixture";

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
    let page = record_notification_page(&client, &recorder).await?;
    record_enrichment(&client, &recorder, page.as_deref().unwrap_or_default()).await?;
    record_mark_read(&client, &recorder, page.as_deref().unwrap_or_default()).await?;
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

/// A real page of notifications, scoped to one repository we have checked is
/// public.
///
/// [`record_notifications`] above refuses to write a non-empty `/notifications`
/// body, and that guard stays: the *whole* inbox may name private
/// repositories, and no amount of eyeballing makes committing it safe. This
/// recording is a different claim, and it is structural rather than visual:
/// `GET /repos/{owner}/{repo}/notifications` can only return threads belonging
/// to that one repository, so if the repository is public, so is every row.
/// The recorder asks GitHub whether it is, and refuses if the answer is no or
/// if it cannot tell.
async fn record_notification_page(
    client: &GitHubClient,
    recorder: &RecordingTransport,
) -> Result<Option<Vec<Notification>>, Boxed> {
    println!("GET /repos/{OWNER}/{NOTIFICATION_REPO} to check it is public");
    let about = client
        .rest(RestRequest::get(format!(
            "/repos/{OWNER}/{NOTIFICATION_REPO}"
        )))
        .await?;
    // The check itself is not a fixture; drop it rather than committing four
    // kilobytes of repository metadata nothing reads.
    recorder.drain();

    let public = about
        .as_modified()
        .map(|r| r.json::<serde_json::Value>())
        .transpose()?
        .and_then(|v| v.get("private").and_then(serde_json::Value::as_bool))
        .map(|private| !private);

    if public != Some(true) {
        println!(
            "  SKIPPED: could not confirm {OWNER}/{NOTIFICATION_REPO} is public \
             (private = {public:?}). A notification body is only ever recorded \
             from a repository GitHub says is public."
        );
        return Ok(None);
    }

    println!("GET /repos/{OWNER}/{NOTIFICATION_REPO}/notifications, then again conditionally");
    let filter = NotificationFilter::default()
        .in_repo(RepoRef::new(OWNER, NOTIFICATION_REPO))
        .per_page(3);

    let first = client
        .notifications()
        .list(&filter, Validators::none())
        .await?;
    let Conditional::Modified(page) = first else {
        return Err("the first request was answered 304; nothing to record".into());
    };

    let second = client
        .notifications()
        .list(&filter, page.validators.clone())
        .await?;
    if !second.is_not_modified() {
        println!("  note: the second request was not answered 304");
    }

    write(
        "notifications_page",
        "A real page of notifications and its 304, from a repository the \
         recorder asked GitHub to confirm is public before writing a byte. \
         The whole-inbox recording next door stays empty on purpose; this \
         endpoint can only return one repository's threads, which is a \
         structural guarantee rather than an eyeballed one. Note what the \
         payload does *not* carry: no number, no state, no actor, no browser \
         URL — the argument for enrichment. Note also `subject.type`, which \
         is PascalCase, and the `Last-Modified` this endpoint sends alongside \
         a weak `ETag`.",
        recorder.drain(),
    )?;
    Ok(Some(page.items))
}

/// One GraphQL query resolving a whole page, with one alias GitHub cannot.
async fn record_enrichment(
    client: &GitHubClient,
    recorder: &RecordingTransport,
    page: &[Notification],
) -> Result<(), Boxed> {
    if page.is_empty() {
        println!("  SKIPPED: no notification page to enrich");
        return Ok(());
    }

    // A subject in a repository that does not exist, appended to the real
    // page. GitHub answers 200 with the rest resolved and one NOT_FOUND, and
    // that combination is the thing worth recording: it is what a repository
    // you lost access to looks like, and it must not fail the other rows.
    let mut subjects = page.to_vec();
    subjects.push(Notification {
        id: NotificationId("0".to_owned()),
        unread: false,
        reason: NotificationReason::Subscribed,
        updated_at: time::OffsetDateTime::UNIX_EPOCH,
        title: "a subject in a repository that does not exist".to_owned(),
        kind: SubjectKind::PullRequest,
        repo: RepoRef::new(OWNER, MISSING_REPO),
        subject: SubjectRef::from_api_url(&format!(
            "https://api.github.com/repos/{OWNER}/{MISSING_REPO}/pulls/1"
        )),
        detail: Enrichment::Absent,
    });

    println!(
        "POST /graphql to enrich {} subjects at once",
        subjects.len()
    );
    client.notifications().enrich(&mut subjects).await?;
    for n in &subjects {
        println!("  {} -> {:?}", n.id, n.detail);
    }

    write(
        "notifications_enrichment",
        "One query resolving a page of subjects, plus one alias in a \
         repository that does not exist. GitHub answers HTTP 200 with the rest \
         of the data present and a single NOT_FOUND naming the failed alias in \
         its `path` — which is why omaghy_api::graphql::decode_partial exists \
         alongside decode. Compare X-RateLimit-Used against the request before \
         it: a whole page costs one point.",
        recorder.drain(),
    )
}

/// Marking a thread read that is already read.
///
/// Chosen deliberately over an unread one: this recording has to be safe to
/// re-run from anyone's account, and the only mutation that is is a no-op. The
/// recorder refuses if every thread on the page is unread, rather than marking
/// somebody's inbox read to get a fixture.
async fn record_mark_read(
    client: &GitHubClient,
    recorder: &RecordingTransport,
    page: &[Notification],
) -> Result<(), Boxed> {
    let Some(already_read) = page.iter().find(|n| !n.unread) else {
        println!(
            "  SKIPPED: every thread on the page is unread, and recording this would mark one read"
        );
        return Ok(());
    };

    println!("PATCH /notifications/threads/{}", already_read.id);
    client
        .notifications()
        .mark_read(std::slice::from_ref(&already_read.id))
        .await?;

    write(
        "notifications_mark_read",
        "PATCH on a thread that was already read. GitHub answers 205 Reset \
         Content with an empty body, which is what makes mark_read idempotent \
         — `spec/20-store.md` §5 requires it and this is the proof rather than \
         the assertion. Recorded against an already-read thread on purpose: a \
         fixture that is safe to re-record is one that changes nothing.",
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

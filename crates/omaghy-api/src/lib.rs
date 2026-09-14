//! GitHub transport: GraphQL reads, REST writes, ETags, retries, rate limiting.
//!
//! This crate is the only place in omaghy that speaks HTTP. It knows nothing
//! about surfaces or caching; it turns a path or a query into either bytes or
//! an [`omaghy_model::StoreError`] the UI already knows how to render.
//!
//! ```text
//!   auth ──► GitHubClient ──► Transport ──► api.github.com
//!             │  │  │                 ▲
//!             │  │  └ conditional     └ CassetteTransport (tests)
//!             │  └ governor
//!             └ retry
//! ```
//!
//! Four things it owns, and why each is here rather than at a call site:
//!
//! - **[`auth`]** — the resolution chain, and a token that redacts itself in
//!   `Debug`. A token in a log is a leaked token.
//! - **[`conditional`]** — ETags and `Last-Modified`. A 304 is a distinct
//!   outcome from a 200, because it costs no rate limit and means "your cache
//!   is still right", not "there is nothing there".
//! - **[`ratelimit`]** — REST requests and GraphQL points, tracked separately,
//!   plus the two headers that are instructions rather than data:
//!   `Retry-After` and `X-Poll-Interval`.
//! - **[`retry`]** — exponential backoff on 5xx, and the rule that a mutation
//!   is never retried automatically.
//!
//! Everything above [`transport::Transport`] is tested against recorded
//! responses; **no test in this crate opens a socket** (`spec/00-overview.md`
//! §7). See [`cassette`].
//!
//! # Surfaces
//!
//! On top of that transport sit the endpoint modules — one per surface, each
//! owning the translation from GitHub's wire shapes into `omaghy-model`. So
//! far that is [`notifications`]: the conditional poll, the `SubjectRef`
//! parsed from an API URL, one batched GraphQL query that enriches a whole
//! page, and marking threads read.
//!
//! # Getting a client
//!
//! ```no_run
//! use omaghy_api::{GitHubClient, ReqwestTransport, RestRequest, auth};
//! use std::sync::Arc;
//!
//! # async fn f() -> Result<(), omaghy_model::StoreError> {
//! let token = auth::resolve_token()?.token;
//! let client = GitHubClient::new(token, Arc::new(ReqwestTransport::new()?));
//! let limits = client.rest(RestRequest::get("/rate_limit")).await?;
//! # Ok(())
//! # }
//! ```

pub mod auth;
pub mod cassette;
pub mod client;
pub mod conditional;
mod error;
pub mod graphql;
pub mod notifications;
pub mod ratelimit;
pub mod reqwest_transport;
pub mod retry;
pub mod transport;
pub mod viewer;

pub use auth::{ResolvedToken, Token, TokenSource, resolve_token};
pub use client::{
    API_VERSION, ClientConfig, DEFAULT_ACCEPT, DEFAULT_BASE_URL, GitHubClient, RestRequest,
    RestResponse,
};
pub use conditional::{Conditional, Validators};
pub use graphql::{GraphQlError, GraphQlRequest, Partial, RateLimitField};
pub use notifications::{
    ENRICHMENT_BATCH, MAX_PER_PAGE, NotificationFilter, NotificationPage, Notifications,
};
pub use ratelimit::{Budget, Clock, Governor, POLL_INTERVAL_FLOOR, RateLimits, Resource};
pub use reqwest_transport::{ReqwestTransport, install_crypto_provider};
pub use retry::{RealSleeper, RetryPolicy, Sleeper};
pub use transport::{Headers, HttpRequest, HttpResponse, Method, Transport, TransportError};
pub use viewer::viewer_login;

//! The client every other omaghy crate talks to.
//!
//! It owns the four things that must not be decided per-call-site: the headers
//! GitHub requires, conditional requests, the rate governor, and retry. A
//! caller supplies a path and gets back either data or a [`StoreError`] the UI
//! already knows how to render.
//!
//! **What is retried, and what is not.** A 5xx or an unreachable host is
//! retried with exponential backoff. A rate limit — primary or secondary — is
//! not: the governor records when sending may resume and the call fails, so a
//! refresh never blocks for minutes (`spec/20-store.md` §6). A mutation is
//! never retried at all, whatever the failure; a duplicated review is worse
//! than a visible error.

use crate::auth::Token;
use crate::conditional::{Conditional, Validators};
use crate::error;
use crate::graphql::{self, GraphQlRequest};
use crate::ratelimit::{Clock, Governor, RateLimits, Resource};
use crate::retry::{RealSleeper, RetryPolicy, Sleeper};
use crate::transport::{Headers, HttpRequest, HttpResponse, Method, Transport};
use omaghy_model::StoreError;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use time::Duration;

/// The default API host. Separate from the GraphQL endpoint because GitHub
/// Enterprise puts them at different paths, and M4 will want that.
pub const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// Pinned deliberately. GitHub's REST API is versioned by date and an
/// unpinned client is one that changes behaviour without a deploy.
pub const API_VERSION: &str = "2022-11-28";

pub const DEFAULT_ACCEPT: &str = "application/vnd.github+json";

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub base_url: String,
    pub user_agent: String,
    pub api_version: String,
    pub accept: String,
    pub retry: RetryPolicy,
    pub clock: Clock,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            // GitHub requires a User-Agent and rejects requests without one.
            // Naming the project makes us identifiable in their logs, which is
            // what they ask for in exchange.
            user_agent: concat!("omaghy/", env!("CARGO_PKG_VERSION")).to_owned(),
            api_version: API_VERSION.to_owned(),
            accept: DEFAULT_ACCEPT.to_owned(),
            retry: RetryPolicy::default(),
            clock: Clock::System,
        }
    }
}

/// A REST call, before the client adds the parts it owns.
#[derive(Debug, Clone)]
pub struct RestRequest {
    pub method: Method,
    /// Path and query, e.g. `/notifications?all=true`. Joined onto the base.
    pub path: String,
    /// Sent as `If-None-Match` / `If-Modified-Since`.
    pub validators: Validators,
    /// Overrides the default `Accept` — raw diffs use
    /// `application/vnd.github.v3.diff`.
    pub accept: Option<String>,
    pub body: Option<Vec<u8>>,
}

impl RestRequest {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            validators: Validators::none(),
            accept: None,
            body: None,
        }
    }

    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::Get, path)
    }

    pub fn post(path: impl Into<String>) -> Self {
        Self::new(Method::Post, path)
    }

    pub fn patch(path: impl Into<String>) -> Self {
        Self::new(Method::Patch, path)
    }

    pub fn put(path: impl Into<String>) -> Self {
        Self::new(Method::Put, path)
    }

    pub fn delete(path: impl Into<String>) -> Self {
        Self::new(Method::Delete, path)
    }

    /// Make this a conditional request. A 304 answer costs no rate limit.
    #[must_use]
    pub fn conditional(mut self, validators: Validators) -> Self {
        self.validators = validators;
        self
    }

    #[must_use]
    pub fn accept(mut self, accept: impl Into<String>) -> Self {
        self.accept = Some(accept.into());
        self
    }

    #[must_use]
    pub fn json(mut self, body: &impl serde::Serialize) -> Self {
        // Serialising our own types cannot fail in practice; an empty body and
        // GitHub's 422 is a better outcome than a panic in a refresh task.
        self.body = serde_json::to_vec(body).ok();
        self
    }
}

/// A successful REST response, with its validators already harvested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
    pub validators: Validators,
}

impl RestResponse {
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, StoreError> {
        serde_json::from_slice(&self.body).map_err(|e| StoreError::Upstream {
            status: self.status,
            // A body we cannot parse is GitHub changing shape under us. Say
            // where, because "invalid type" alone is unfindable.
            message: format!("could not read GitHub's response: {e}"),
        })
    }

    /// The `rel="next"` URL from the `Link` header, if the list continues.
    pub fn next_page(&self) -> Option<String> {
        parse_link_rel(self.headers.get("link")?, "next")
    }
}

fn parse_link_rel(link: &str, rel: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .contains(&format!("rel=\"{rel}\""))
            .then(|| url.trim().trim_start_matches('<').trim_end_matches('>'))
            .map(str::to_owned)
    })
}

/// The GitHub transport.
#[derive(Debug, Clone)]
pub struct GitHubClient {
    transport: Arc<dyn Transport>,
    sleeper: Arc<dyn Sleeper>,
    token: Token,
    config: ClientConfig,
    governor: Arc<Governor>,
}

impl GitHubClient {
    pub fn new(token: Token, transport: Arc<dyn Transport>) -> Self {
        Self::with_config(token, transport, ClientConfig::default())
    }

    pub fn with_config(token: Token, transport: Arc<dyn Transport>, config: ClientConfig) -> Self {
        let governor = Arc::new(Governor::new(config.clock));
        Self {
            transport,
            sleeper: Arc::new(RealSleeper),
            token,
            config,
            governor,
        }
    }

    /// Substitute the thing that waits. Tests use this to assert on backoff
    /// without spending the backoff.
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    pub fn governor(&self) -> &Arc<Governor> {
        &self.governor
    }

    pub fn rate_limits(&self) -> RateLimits {
        self.governor.snapshot()
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// A REST call. `Ok(NotModified)` means the cache is still correct — it is
    /// not an error and it is not an empty list.
    pub async fn rest(
        &self,
        request: RestRequest,
    ) -> Result<Conditional<RestResponse>, StoreError> {
        let held = request.validators.clone();
        let http = self.build_rest(&request);
        let response = self
            .send(http, Resource::Rest, request.method.is_mutation())
            .await?;

        let harvested = Validators::from_headers(&response.headers);
        if response.is_not_modified() {
            return Ok(Conditional::NotModified {
                validators: held.merged_with(harvested),
            });
        }

        Ok(Conditional::Modified(RestResponse {
            status: response.status,
            headers: response.headers,
            body: response.body,
            validators: harvested,
        }))
    }

    /// A GraphQL query or mutation, deserialised into a type of ours.
    ///
    /// Failure arrives as HTTP 200 with an `errors` array, so the status is
    /// only half the answer — see [`crate::graphql`].
    pub async fn graphql<T: DeserializeOwned>(
        &self,
        request: &GraphQlRequest,
    ) -> Result<T, StoreError> {
        let body = serde_json::to_vec(request).map_err(|e| StoreError::Upstream {
            status: 0,
            message: format!("could not encode the GraphQL query: {e}"),
        })?;

        let http = self
            .base_headers(HttpRequest::new(
                Method::Post,
                format!("{}/graphql", self.config.base_url.trim_end_matches('/')),
            ))
            .header("Content-Type", "application/json")
            .body(body);

        let response = self.send(http, Resource::GraphQl, request.mutation).await?;

        graphql::decode(&response.body, self.governor.now())
    }

    fn build_rest(&self, request: &RestRequest) -> HttpRequest {
        let url = format!(
            "{}{}",
            self.config.base_url.trim_end_matches('/'),
            request.path
        );
        let mut http = self.base_headers(HttpRequest::new(request.method, url));

        if let Some(accept) = &request.accept {
            http.headers.insert("Accept", accept.clone());
        }
        request.validators.apply(&mut http.headers);

        if let Some(body) = &request.body {
            http.headers.insert("Content-Type", "application/json");
            http.body = Some(body.clone());
        }
        http
    }

    /// The headers GitHub requires on every request.
    ///
    /// `X-GitHub-Api-Version` is pinned, `Accept` selects the JSON media type,
    /// and `User-Agent` is mandatory — GitHub rejects requests without one.
    fn base_headers(&self, request: HttpRequest) -> HttpRequest {
        request
            .header("Authorization", self.token.header_value())
            .header("User-Agent", self.config.user_agent.clone())
            .header("Accept", self.config.accept.clone())
            .header("X-GitHub-Api-Version", self.config.api_version.clone())
    }

    /// The retry loop.
    async fn send(
        &self,
        request: HttpRequest,
        resource: Resource,
        mutation: bool,
    ) -> Result<HttpResponse, StoreError> {
        let attempts = self.config.retry.attempts_for(mutation);

        for attempt in 1..=attempts {
            // Asked before every attempt, not only the first: a 5xx retry must
            // not walk into a limit the previous attempt just revealed.
            self.governor.check(resource)?;

            let delay = self.config.retry.delay_before(attempt);
            if delay > Duration::ZERO {
                self.sleeper.sleep(delay).await;
            }

            let last = attempt == attempts;
            match self.transport.execute(request.clone()).await {
                Ok(response) => {
                    self.governor.observe(resource, &response.headers);

                    if response.is_success() || response.is_not_modified() {
                        self.governor.note_success();
                        return Ok(response);
                    }

                    let err = error::map_response(&response, &self.governor, self.governor.now());
                    if last || !is_worth_retrying(&err) {
                        return Err(err);
                    }
                    tracing::debug!(
                        status = response.status,
                        attempt,
                        "retrying after an upstream failure"
                    );
                }
                Err(transport) => {
                    if last || !transport.is_transient() {
                        return Err(transport.into());
                    }
                    tracing::debug!(%transport, attempt, "retrying after a transport failure");
                }
            }
        }

        // Unreachable: the loop returns on its last iteration. Expressed as an
        // error rather than `unreachable!()` because a panic in a refresh task
        // takes down more than the refresh.
        Err(StoreError::Offline(
            "the request was never attempted".to_owned(),
        ))
    }
}

/// Which failures get another attempt inside one call.
///
/// Rate limits are deliberately absent: the governor already knows when
/// sending may resume, and waiting it out here would block a refresh for as
/// long as GitHub asked us to wait.
fn is_worth_retrying(error: &StoreError) -> bool {
    matches!(error, StoreError::Upstream { status, .. } if *status >= 500)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_page_comes_from_the_link_header() {
        let link = "<https://api.github.com/notifications?page=2>; rel=\"next\", \
                    <https://api.github.com/notifications?page=9>; rel=\"last\"";
        assert_eq!(
            parse_link_rel(link, "next").as_deref(),
            Some("https://api.github.com/notifications?page=2")
        );
        assert_eq!(
            parse_link_rel(link, "prev"),
            None,
            "the last page must not look like it continues"
        );
    }

    #[test]
    fn a_request_body_is_json_encoded() {
        let r = RestRequest::patch("/notifications/threads/1")
            .json(&serde_json::json!({"ignored": true}));
        assert_eq!(r.body.as_deref(), Some(br#"{"ignored":true}"#.as_slice()));
    }

    #[test]
    fn only_a_5xx_earns_another_attempt_inside_one_call() {
        assert!(is_worth_retrying(&StoreError::Upstream {
            status: 503,
            message: String::new()
        }));
        assert!(!is_worth_retrying(&StoreError::Upstream {
            status: 422,
            message: String::new()
        }));
        assert!(!is_worth_retrying(&StoreError::NotFound));
        assert!(!is_worth_retrying(&StoreError::RateLimited {
            kind: omaghy_model::LimitKind::Secondary,
            at: time::OffsetDateTime::UNIX_EPOCH,
        }));
    }
}

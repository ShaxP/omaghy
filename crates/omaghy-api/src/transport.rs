//! The seam a test can substitute for a socket.
//!
//! Everything above this module — conditional requests, the rate governor,
//! retries, error mapping — is written against [`Transport`], so the whole of
//! `omaghy-api` can be exercised from recorded responses. **No test in this
//! crate opens a socket** (`spec/00-overview.md` §7); the only implementation
//! that does is [`crate::reqwest_transport::ReqwestTransport`].
//!
//! The request and response types are deliberately ours rather than
//! `reqwest`'s: they are what a cassette serialises to, and a wire format we
//! do not control is a poor fixture format.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// HTTP methods omaghy uses.
///
/// Ours rather than `reqwest::Method` because [`Method::is_mutation`] is a
/// decision this crate makes and must not be able to lose: a mutation is never
/// retried automatically (`spec/20-store.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    Get,
    Head,
    Post,
    Patch,
    Put,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }

    /// Whether replaying this request could change something on GitHub.
    ///
    /// `POST /graphql` is a read in practice, which is why the GraphQL path
    /// carries its own mutation flag ([`crate::graphql::GraphQlRequest`])
    /// rather than inferring safety from the method.
    pub fn is_mutation(self) -> bool {
        !matches!(self, Self::Get | Self::Head)
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Case-insensitive header storage.
///
/// HTTP header names are case-insensitive and GitHub is inconsistent about
/// case across endpoints, so lookups fold case. Insertion order is kept so a
/// recorded cassette stays diff-stable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        match self
            .0
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(&name))
        {
            Some(slot) => slot.1 = value,
            None => self.0.push((name, value)),
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// A header parsed as a number, or `None` if absent or unparseable.
    ///
    /// Unparseable is treated as absent on purpose: a malformed
    /// `X-RateLimit-Remaining` must not take down a refresh.
    pub fn get_num<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
        self.get(name)?.trim().parse().ok()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl<N: Into<String>, V: Into<String>> FromIterator<(N, V)> for Headers {
    fn from_iter<I: IntoIterator<Item = (N, V)>>(iter: I) -> Self {
        let mut h = Self::new();
        for (n, v) in iter {
            h.insert(n, v);
        }
        h
    }
}

// Cassettes store headers as a JSON object. A map loses duplicates, which is
// fine: nothing omaghy reads is a repeated header, and `Set-Cookie` — the one
// header that is routinely repeated — is scrubbed before a cassette is written.
impl Serialize for Headers {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0
            .iter()
            .map(|(n, v)| (n.to_ascii_lowercase(), v.clone()))
            .collect::<BTreeMap<_, _>>()
            .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Headers {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(BTreeMap::<String, String>::deserialize(d)?
            .into_iter()
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: Method,
    /// Absolute URL. Built by the client from a base and a path.
    pub url: String,
    pub headers: Headers,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Headers::new(),
            body: None,
        }
    }

    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name, value);
        self
    }

    #[must_use]
    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }

    /// Everything after the host — the part a cassette matches on.
    pub fn path_and_query(&self) -> &str {
        match self.url.find("://") {
            Some(i) => match self.url[i + 3..].find('/') {
                Some(j) => &self.url[i + 3 + j..],
                None => "/",
            },
            None => &self.url,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name, value);
        self
    }

    #[must_use]
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// The one status that means "nothing changed, and this cost you nothing".
    pub fn is_not_modified(&self) -> bool {
        self.status == 304
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// Why a request never produced a response.
///
/// Kept separate from an HTTP status because the distinction is exactly the
/// `Offline` / `Upstream` split in `spec/20-store.md` §7: we could not reach
/// GitHub, versus GitHub answered and said no.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// DNS, TLS, connection refused — the socket never carried a request.
    #[error("could not reach GitHub: {0}")]
    Unreachable(String),

    #[error("the request to GitHub timed out: {0}")]
    Timeout(String),

    /// A response arrived but could not be read — a truncated body, a decode
    /// failure. Retryable like a 5xx, not like a 404.
    #[error("the response from GitHub could not be read: {0}")]
    Body(String),

    /// The request could not be built at all. A bug on our side; never
    /// retried, because retrying cannot change it.
    #[error("malformed request: {0}")]
    Malformed(String),
}

impl TransportError {
    /// Whether trying again unchanged could plausibly succeed.
    pub fn is_transient(&self) -> bool {
        !matches!(self, Self::Malformed(_))
    }
}

/// The thing a test replaces.
///
/// `Debug` is a supertrait so that every type holding a `dyn Transport` can
/// still derive `Debug` — the workspace warns on `missing_debug_implementations`,
/// and a client that cannot be printed is a client that cannot be diagnosed.
#[async_trait]
pub trait Transport: fmt::Debug + Send + Sync + 'static {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lookup_folds_case() {
        let h: Headers = [("ETag", "\"abc\""), ("X-Poll-Interval", "60")]
            .into_iter()
            .collect();
        assert_eq!(h.get("etag"), Some("\"abc\""));
        assert_eq!(h.get("ETAG"), Some("\"abc\""));
        assert_eq!(h.get_num::<u64>("x-poll-interval"), Some(60));
        assert_eq!(h.get("missing"), None);
    }

    #[test]
    fn inserting_the_same_header_twice_replaces_rather_than_duplicates() {
        let mut h = Headers::new();
        h.insert("Accept", "a");
        h.insert("accept", "b");
        assert_eq!(h.len(), 1);
        assert_eq!(h.get("Accept"), Some("b"));
    }

    #[test]
    fn a_malformed_number_reads_as_absent_rather_than_failing() {
        let h: Headers = [("X-RateLimit-Remaining", "not-a-number")]
            .into_iter()
            .collect();
        assert_eq!(h.get_num::<u32>("x-ratelimit-remaining"), None);
    }

    #[test]
    fn only_reads_are_safe_to_replay() {
        assert!(!Method::Get.is_mutation());
        assert!(!Method::Head.is_mutation());
        for m in [Method::Post, Method::Patch, Method::Put, Method::Delete] {
            assert!(m.is_mutation(), "{m} must never be auto-retried");
        }
    }

    #[test]
    fn a_cassette_matches_on_the_path_not_the_host() {
        let r = HttpRequest::new(Method::Get, "https://api.github.com/notifications?all=true");
        assert_eq!(r.path_and_query(), "/notifications?all=true");
        let r = HttpRequest::new(Method::Get, "https://api.github.com");
        assert_eq!(r.path_and_query(), "/");
    }

    #[test]
    fn headers_round_trip_through_a_cassette() {
        let h: Headers = [("ETag", "\"x\""), ("X-Poll-Interval", "60")]
            .into_iter()
            .collect();
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("\"etag\""), "names are lowercased: {json}");
        let back: Headers = serde_json::from_str(&json).unwrap();
        assert_eq!(back.get("etag"), Some("\"x\""));
    }
}

//! Conditional requests — the thing that makes polling affordable.
//!
//! A 304 costs **no REST rate limit** (verified against the live API while
//! recording `tests/cassettes/notifications_304.json`: `X-RateLimit-Used` was
//! unchanged across it). That is the whole argument for storing a validator
//! beside every cached list and always sending it — `spec/20-store.md` §4.
//!
//! The 304 is therefore a *distinct* outcome rather than an error or an empty
//! 200: a caller that cannot tell them apart will either throw away good cache
//! or write an empty list over it.

use crate::transport::Headers;
use serde::{Deserialize, Serialize};

/// What we send back to GitHub to ask "has this changed?".
///
/// Both are opaque strings. `Last-Modified` is never parsed — it is echoed
/// verbatim into `If-Modified-Since`, which is both what the spec requires and
/// what avoids a date-format bug in the one place a date format would bite.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validators {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl Validators {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn etag(tag: impl Into<String>) -> Self {
        Self {
            etag: Some(tag.into()),
            last_modified: None,
        }
    }

    /// Harvest whatever the response offered.
    ///
    /// GitHub returns `ETag` on a 304 as well as a 200, so the stored
    /// validator is refreshed on every poll rather than only when the body
    /// changes.
    pub fn from_headers(headers: &Headers) -> Self {
        Self {
            etag: headers.get("etag").map(str::to_owned),
            last_modified: headers.get("last-modified").map(str::to_owned),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }

    /// The request headers that turn a fetch into a conditional fetch.
    pub fn apply(&self, headers: &mut Headers) {
        if let Some(etag) = &self.etag {
            headers.insert("If-None-Match", etag.clone());
        }
        if let Some(lm) = &self.last_modified {
            headers.insert("If-Modified-Since", lm.clone());
        }
    }

    /// Keep whatever the new response carried, falling back to what we had.
    ///
    /// A 304 that omits `Last-Modified` must not silently drop the one we were
    /// sending, or the next poll stops being conditional.
    #[must_use]
    pub fn merged_with(&self, newer: Validators) -> Validators {
        Validators {
            etag: newer.etag.or_else(|| self.etag.clone()),
            last_modified: newer.last_modified.or_else(|| self.last_modified.clone()),
        }
    }
}

/// The outcome of a conditional request.
///
/// `NotModified` carries validators because GitHub refreshes them on a 304,
/// and storing the new one keeps the *next* request conditional too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conditional<T> {
    NotModified { validators: Validators },
    Modified(T),
}

impl<T> Conditional<T> {
    pub fn is_not_modified(&self) -> bool {
        matches!(self, Self::NotModified { .. })
    }

    /// The body, if there was one. `None` is "your cache is still correct",
    /// never "there is no data".
    pub fn modified(self) -> Option<T> {
        match self {
            Self::Modified(v) => Some(v),
            Self::NotModified { .. } => None,
        }
    }

    pub fn as_modified(&self) -> Option<&T> {
        match self {
            Self::Modified(v) => Some(v),
            Self::NotModified { .. } => None,
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Conditional<U> {
        match self {
            Self::Modified(v) => Conditional::Modified(f(v)),
            Self::NotModified { validators } => Conditional::NotModified { validators },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_validator_becomes_a_conditional_request() {
        let v = Validators {
            etag: Some("\"abc\"".to_owned()),
            last_modified: Some("Sat, 12 Sep 2026 07:50:36 GMT".to_owned()),
        };
        let mut h = Headers::new();
        v.apply(&mut h);
        assert_eq!(h.get("If-None-Match"), Some("\"abc\""));
        assert_eq!(
            h.get("if-modified-since"),
            Some("Sat, 12 Sep 2026 07:50:36 GMT")
        );
    }

    #[test]
    fn no_validator_means_no_conditional_headers() {
        let mut h = Headers::new();
        Validators::none().apply(&mut h);
        assert!(h.is_empty());
    }

    #[test]
    fn a_304_that_omits_last_modified_does_not_lose_it() {
        let held = Validators {
            etag: Some("\"old\"".to_owned()),
            last_modified: Some("Sat, 12 Sep 2026 07:50:36 GMT".to_owned()),
        };
        let from_304: Headers = [("ETag", "\"new\"")].into_iter().collect();
        let merged = held.merged_with(Validators::from_headers(&from_304));
        assert_eq!(merged.etag.as_deref(), Some("\"new\""));
        assert!(
            merged.last_modified.is_some(),
            "dropping it makes the next poll unconditional"
        );
    }

    #[test]
    fn not_modified_is_not_an_empty_body() {
        let empty_200: Conditional<Vec<u8>> = Conditional::Modified(Vec::new());
        assert!(!empty_200.is_not_modified());
        assert_eq!(empty_200.modified(), Some(Vec::new()));

        let unchanged: Conditional<Vec<u8>> = Conditional::NotModified {
            validators: Validators::etag("\"x\""),
        };
        assert!(unchanged.is_not_modified());
        assert_eq!(unchanged.modified(), None);
    }
}

//! Conditional-request validators.
//!
//! An opaque pair of strings that answers "has this changed?" — stored beside
//! the data they validate, sent back on the next fetch.
//!
//! **Why this is in the vocabulary and not in a transport.** It crosses a
//! boundary that nothing else in `omaghy-api` does: the fetcher harvests it
//! and the cache persists it, and neither crate may depend on the other. Both
//! defined it independently, with identical fields, and `omaghy-sync` spent
//! four lines converting between the two — exactly the duplication this crate
//! exists to prevent (`spec/90-plan.md` §8).
//!
//! What is *not* here is anything HTTP. Turning these into request headers, or
//! harvesting them from a response, belongs to `omaghy-api`, which owns the
//! wire format (`spec/10-domain-model.md` §1).

use serde::{Deserialize, Serialize};

/// What we send back to GitHub to ask whether something changed.
///
/// Both fields are opaque strings. `Last-Modified` is never parsed — it is
/// echoed verbatim, which is both what the HTTP spec requires and what avoids
/// a date-format bug in the one place a date format would bite.
///
/// A 304 costs **no REST rate limit**, which is the whole argument for storing
/// one of these beside every cached list and always sending it
/// (`spec/20-store.md` §4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validators {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl Validators {
    /// Nothing to revalidate against — the fetch will be unconditional.
    ///
    /// The same value as `Validators::default()`, named because at a call site
    /// passing a validator, "none" states an intent that `default()` does not.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn etag(tag: impl Into<String>) -> Self {
        Self {
            etag: Some(tag.into()),
            last_modified: None,
        }
    }

    pub fn last_modified(at: impl Into<String>) -> Self {
        Self {
            etag: None,
            last_modified: Some(at.into()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }

    /// Keep whatever the newer response carried, falling back to what we held.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_empty_and_default_agrees() {
        assert!(Validators::none().is_empty());
        assert_eq!(Validators::none(), Validators::default());
        assert!(!Validators::etag("\"x\"").is_empty());
        assert!(!Validators::last_modified("Sat, 12 Sep 2026 07:50:36 GMT").is_empty());
    }

    #[test]
    fn a_304_that_omits_last_modified_does_not_lose_it() {
        let held = Validators {
            etag: Some("\"old\"".to_owned()),
            last_modified: Some("Sat, 12 Sep 2026 07:50:36 GMT".to_owned()),
        };
        let merged = held.merged_with(Validators::etag("\"new\""));
        assert_eq!(merged.etag.as_deref(), Some("\"new\""));
        assert!(
            merged.last_modified.is_some(),
            "dropping it makes the next poll unconditional"
        );
    }

    #[test]
    fn merging_over_nothing_keeps_everything() {
        let held = Validators::etag("\"old\"");
        assert_eq!(held.merged_with(Validators::none()), held);
    }

    #[test]
    fn an_absent_validator_is_not_serialised() {
        let json = serde_json::to_string(&Validators::etag("\"x\"")).unwrap();
        assert_eq!(json, r#"{"etag":"\"x\""}"#);
        assert_eq!(serde_json::to_string(&Validators::none()).unwrap(), "{}");

        let back: Validators = serde_json::from_str("{}").unwrap();
        assert!(
            back.is_empty(),
            "an empty object is no validator, not an error"
        );
    }
}

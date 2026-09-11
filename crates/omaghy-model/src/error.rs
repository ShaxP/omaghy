//! The error taxonomy.
//!
//! Lives in `omaghy-model` because every crate speaks it. The distinctions
//! earn their place by producing *different UI* — see `spec/20-store.md` §7.

use std::fmt;
use thiserror::Error;
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthError {
    #[error("no GitHub token found; run `gh auth login` or set OMAGHY_TOKEN")]
    Missing,
    #[error("the GitHub token was rejected; run `gh auth login` to refresh it")]
    Rejected,
    #[error("the token lacks the {scope} scope; run `gh auth refresh -s {scope}`")]
    MissingScope { scope: String },
    #[error("could not run `gh auth token`: {0}")]
    HelperFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CacheError {
    #[error("the cache could not be opened: {0}")]
    Open(String),
    #[error("the cache is corrupt and will be rebuilt: {0}")]
    Corrupt(String),
    #[error("a cache query failed: {0}")]
    Query(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// The published hourly budget. Resets at a known time.
    Primary,
    /// Abuse detection. Never retry a mutation automatically.
    Secondary,
}

impl fmt::Display for LimitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Primary => "rate limit",
            Self::Secondary => "secondary rate limit",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    #[error(transparent)]
    Auth(#[from] AuthError),

    #[error("{kind} reached; resets at {at}")]
    RateLimited { kind: LimitKind, at: OffsetDateTime },

    /// DNS, TLS, timeout — anything meaning "we could not reach GitHub".
    /// Distinct from an error *from* GitHub, because with a warm cache this is
    /// a banner rather than an empty screen.
    #[error("GitHub is unreachable: {0}")]
    Offline(String),

    #[error("not found")]
    NotFound,

    /// Distinct from [`StoreError::NotFound`]: access was lost, so retrying
    /// will not help and the subject is recorded as permanently unenriched.
    #[error("access denied")]
    Forbidden,

    #[error("GitHub returned {status}: {message}")]
    Upstream { status: u16, message: String },

    #[error(transparent)]
    Cache(#[from] CacheError),
}

impl StoreError {
    /// Whether retrying unchanged could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Offline(_) => true,
            Self::Upstream { status, .. } => *status >= 500,
            Self::RateLimited { .. } => true, // after waiting
            Self::Auth(_) | Self::NotFound | Self::Forbidden | Self::Cache(_) => false,
        }
    }

    /// Whether cached content should still be shown beneath a banner.
    ///
    /// An unreachable server does not invalidate what we already have; a
    /// rejected token does not either, but a corrupt cache does.
    pub fn keeps_cached_content(&self) -> bool {
        !matches!(self, Self::Cache(CacheError::Corrupt(_)))
    }

    /// One line, for the footer. Never a stack trace — `spec/30-ui.md` §8.
    pub fn terse(&self) -> String {
        self.to_string()
    }
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn auth_errors_name_the_command_that_fixes_them() {
        assert!(AuthError::Missing.to_string().contains("gh auth login"));
        assert!(
            AuthError::MissingScope {
                scope: "workflow".into()
            }
            .to_string()
            .contains("gh auth refresh -s workflow")
        );
    }

    #[test]
    fn retryability_distinguishes_transient_from_permanent() {
        assert!(StoreError::Offline("dns".into()).is_retryable());
        assert!(
            StoreError::Upstream {
                status: 502,
                message: "bad gateway".into()
            }
            .is_retryable()
        );
        assert!(
            !StoreError::Upstream {
                status: 422,
                message: "unprocessable".into()
            }
            .is_retryable()
        );
        assert!(!StoreError::Forbidden.is_retryable());
        assert!(!StoreError::Auth(AuthError::Missing).is_retryable());
    }

    #[test]
    fn only_a_corrupt_cache_invalidates_what_we_already_have() {
        assert!(StoreError::Offline("timeout".into()).keeps_cached_content());
        assert!(StoreError::Auth(AuthError::Rejected).keeps_cached_content());
        assert!(
            !StoreError::Cache(CacheError::Corrupt("bad header".into())).keeps_cached_content()
        );
    }

    #[test]
    fn rate_limit_message_says_when_it_resets() {
        let e = StoreError::RateLimited {
            kind: LimitKind::Secondary,
            at: datetime!(2026-09-10 12:00 UTC),
        };
        let s = e.terse();
        assert!(s.contains("secondary rate limit"));
        assert!(s.contains("2026"));
    }

    #[test]
    fn errors_convert_upward_without_boilerplate() {
        let e: StoreError = AuthError::Missing.into();
        assert!(matches!(e, StoreError::Auth(AuthError::Missing)));
        let e: StoreError = CacheError::Open("locked".into()).into();
        assert!(matches!(e, StoreError::Cache(CacheError::Open(_))));
    }
}

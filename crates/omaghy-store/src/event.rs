//! Change notification. Events name *what* changed and never carry the data —
//! carrying payloads means two paths into UI state, and they diverge.
//!
//! See `spec/20-store.md` §1.

use omaghy_model::{StoreError, SubjectRef};
use time::OffsetDateTime;

/// Something refreshable. Used to schedule, to coalesce, and to cancel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RefreshTarget {
    Dashboard,
    Notifications,
    /// Second-pass enrichment for a page of notifications.
    NotificationDetails,
    PullRequests {
        key: String,
    },
    PullRequest(SubjectRef),
    Issues {
        key: String,
    },
    Issue(SubjectRef),
}

impl RefreshTarget {
    /// Whether this target is worth scheduling while another is in flight.
    ///
    /// The same target requested twice is one request with two waiters
    /// (`spec/20-store.md` §6).
    pub fn coalesces_with(&self, other: &Self) -> bool {
        self == other
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreEvent {
    /// New data landed. Re-query; the event carries none of it.
    Updated(RefreshTarget),
    RefreshStarted(RefreshTarget),
    RefreshFailed {
        target: RefreshTarget,
        error: StoreError,
    },
    RateLimited {
        until: OffsetDateTime,
    },
    AuthLost(omaghy_model::AuthError),
}

impl StoreEvent {
    pub fn target(&self) -> Option<&RefreshTarget> {
        match self {
            Self::Updated(t) | Self::RefreshStarted(t) => Some(t),
            Self::RefreshFailed { target, .. } => Some(target),
            Self::RateLimited { .. } | Self::AuthLost(_) => None,
        }
    }

    /// Whether a surface not watching this target still needs to react.
    ///
    /// Auth loss and rate limiting are global: every surface shows them.
    pub fn is_global(&self) -> bool {
        matches!(self, Self::RateLimited { .. } | Self::AuthLost(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omaghy_model::AuthError;
    use time::macros::datetime;

    #[test]
    fn events_name_a_target_or_are_global() {
        let e = StoreEvent::Updated(RefreshTarget::Notifications);
        assert_eq!(e.target(), Some(&RefreshTarget::Notifications));
        assert!(!e.is_global());

        let e = StoreEvent::AuthLost(AuthError::Rejected);
        assert_eq!(e.target(), None);
        assert!(e.is_global());

        let e = StoreEvent::RateLimited {
            until: datetime!(2026-09-11 13:00 UTC),
        };
        assert!(e.is_global());
    }

    #[test]
    fn identical_targets_coalesce() {
        let a = RefreshTarget::Notifications;
        assert!(a.coalesces_with(&RefreshTarget::Notifications));
        assert!(!a.coalesces_with(&RefreshTarget::Dashboard));

        let pr = |n: u64| RefreshTarget::PullRequests {
            key: format!("k{n}"),
        };
        assert!(pr(1).coalesces_with(&pr(1)));
        assert!(!pr(1).coalesces_with(&pr(2)));
    }
}

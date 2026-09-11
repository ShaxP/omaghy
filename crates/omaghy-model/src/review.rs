//! Review state. Viewer-relative, which is why the cache is keyed by viewer.
//!
//! See `spec/10-domain-model.md` §3.2.

use crate::actor::Actor;
use serde::{Deserialize, Serialize};

/// Verified against the live schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewState {
    Pending,
    Commented,
    Approved,
    ChangesRequested,
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub decision: Option<ReviewDecision>,
    pub reviewers: Vec<(Actor, ReviewState)>,
    /// Viewer-relative: does this need *me*.
    pub i_am_requested: bool,
    /// Viewer-relative.
    pub my_review: Option<ReviewState>,
}

impl ReviewSummary {
    /// Whether the viewer still owes this PR a review.
    ///
    /// Requested and already approved is not outstanding; requested after a
    /// dismissal is.
    pub fn awaits_me(&self) -> bool {
        if !self.i_am_requested {
            return false;
        }
        !matches!(
            self.my_review,
            Some(ReviewState::Approved | ReviewState::ChangesRequested)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(requested: bool, mine: Option<ReviewState>) -> ReviewSummary {
        ReviewSummary {
            i_am_requested: requested,
            my_review: mine,
            ..Default::default()
        }
    }

    #[test]
    fn not_requested_never_awaits_me() {
        assert!(!summary(false, None).awaits_me());
        assert!(!summary(false, Some(ReviewState::Commented)).awaits_me());
    }

    #[test]
    fn requested_and_unreviewed_awaits_me() {
        assert!(summary(true, None).awaits_me());
        assert!(summary(true, Some(ReviewState::Pending)).awaits_me());
        // A comment is not a verdict.
        assert!(summary(true, Some(ReviewState::Commented)).awaits_me());
        // Re-requested after a dismissal.
        assert!(summary(true, Some(ReviewState::Dismissed)).awaits_me());
    }

    #[test]
    fn a_delivered_verdict_clears_it() {
        assert!(!summary(true, Some(ReviewState::Approved)).awaits_me());
        assert!(!summary(true, Some(ReviewState::ChangesRequested)).awaits_me());
    }
}

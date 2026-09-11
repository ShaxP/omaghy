//! Pull requests.
//!
//! The trap: `PullRequestState` is `OPEN | CLOSED | MERGED` — draft is a
//! separate boolean. Rendering `state` alone shows a draft PR as plain "open",
//! so surfaces use [`PullRequest::display_status`] instead.
//!
//! See `spec/10-domain-model.md` §3.1.

use crate::{
    actor::Actor, checks::CheckRollup, ids::NodeId, label::Label, repo::RepoRef,
    review::ReviewSummary,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Verified against the live schema: there is no `Draft` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    Unknown,
}

/// What a surface actually renders — `state` and `is_draft` collapsed into the
/// four cases a reader recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrDisplayStatus {
    Draft,
    Open,
    Merged,
    Closed,
}

impl PrDisplayStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub node_id: NodeId,
    pub number: u64,
    pub repo: RepoRef,
    pub title: String,
    /// `None` for a ghost author — a deleted account.
    pub author: Option<Actor>,
    pub state: PrState,
    pub is_draft: bool,
    pub mergeable: Mergeable,
    pub labels: Vec<Label>,
    pub review: ReviewSummary,
    pub checks: CheckRollup,
    pub comment_count: u32,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl PullRequest {
    /// Always use this over [`PullRequest::state`] when rendering.
    pub fn display_status(&self) -> PrDisplayStatus {
        match self.state {
            // Draft only means anything while open: a merged PR was not a
            // draft when it merged, and GitHub leaves the flag set on close.
            PrState::Open if self.is_draft => PrDisplayStatus::Draft,
            PrState::Open => PrDisplayStatus::Open,
            PrState::Merged => PrDisplayStatus::Merged,
            PrState::Closed => PrDisplayStatus::Closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::CheckRollup;
    use time::macros::datetime;

    fn pr(state: PrState, is_draft: bool) -> PullRequest {
        PullRequest {
            node_id: NodeId("PR_test".into()),
            number: 61,
            repo: RepoRef::new("ShaxP", "shax"),
            title: "fix: syntax highlighting follows the Dark/Light/System toggle".into(),
            author: Some(Actor::new("ShaxP")),
            state,
            is_draft,
            mergeable: Mergeable::Unknown,
            labels: vec![],
            review: ReviewSummary::default(),
            checks: CheckRollup::empty(),
            comment_count: 0,
            additions: 0,
            deletions: 0,
            changed_files: 0,
            created_at: datetime!(2026-07-10 12:00 UTC),
            updated_at: datetime!(2026-07-10 16:04:40 UTC),
        }
    }

    #[test]
    fn draft_is_a_display_status_not_a_state() {
        assert_eq!(
            pr(PrState::Open, true).display_status(),
            PrDisplayStatus::Draft
        );
        assert_eq!(
            pr(PrState::Open, false).display_status(),
            PrDisplayStatus::Open
        );
    }

    #[test]
    fn the_draft_flag_does_not_survive_merge_or_close() {
        // GitHub leaves is_draft set on PRs that were closed while draft.
        // Rendering "draft" for a merged PR would be a lie.
        assert_eq!(
            pr(PrState::Merged, true).display_status(),
            PrDisplayStatus::Merged
        );
        assert_eq!(
            pr(PrState::Closed, true).display_status(),
            PrDisplayStatus::Closed
        );
    }
}

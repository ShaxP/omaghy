//! Issues. See `spec/10-domain-model.md` §3.1.

use crate::{actor::Actor, ids::NodeId, label::Label, repo::RepoRef};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IssueState {
    Open,
    Closed,
}

/// Why an issue closed. "Closed as not planned" reads very differently from
/// "closed", so the reason is part of the display status, not a footnote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IssueStateReason {
    Completed,
    NotPlanned,
    Reopened,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueDisplayStatus {
    Open,
    Completed,
    NotPlanned,
    Duplicate,
}

impl IssueDisplayStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Completed => "closed",
            Self::NotPlanned => "not planned",
            Self::Duplicate => "duplicate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub node_id: NodeId,
    pub number: u64,
    pub repo: RepoRef,
    pub title: String,
    pub author: Option<Actor>,
    pub state: IssueState,
    pub state_reason: Option<IssueStateReason>,
    pub labels: Vec<Label>,
    pub assignees: Vec<Actor>,
    pub comment_count: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Issue {
    pub fn display_status(&self) -> IssueDisplayStatus {
        match (self.state, self.state_reason) {
            (IssueState::Open, _) => IssueDisplayStatus::Open,
            (IssueState::Closed, Some(IssueStateReason::NotPlanned)) => {
                IssueDisplayStatus::NotPlanned
            }
            (IssueState::Closed, Some(IssueStateReason::Duplicate)) => {
                IssueDisplayStatus::Duplicate
            }
            (IssueState::Closed, _) => IssueDisplayStatus::Completed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn issue(state: IssueState, reason: Option<IssueStateReason>) -> Issue {
        Issue {
            node_id: NodeId("I_test".into()),
            number: 7,
            repo: RepoRef::new("ShaxP", "omaghy"),
            title: "Notifications inbox: decide enrichment strategy".into(),
            author: Some(Actor::new("ShaxP")),
            state,
            state_reason: reason,
            labels: vec![],
            assignees: vec![],
            comment_count: 0,
            created_at: datetime!(2026-09-01 09:00 UTC),
            updated_at: datetime!(2026-09-10 08:00 UTC),
        }
    }

    #[test]
    fn not_planned_reads_differently_from_completed() {
        assert_eq!(
            issue(IssueState::Closed, Some(IssueStateReason::NotPlanned)).display_status(),
            IssueDisplayStatus::NotPlanned
        );
        assert_eq!(
            issue(IssueState::Closed, Some(IssueStateReason::Completed)).display_status(),
            IssueDisplayStatus::Completed
        );
        // Closed with no reason recorded is still closed, not "not planned".
        assert_eq!(
            issue(IssueState::Closed, None).display_status(),
            IssueDisplayStatus::Completed
        );
    }

    #[test]
    fn reopened_is_open_regardless_of_reason() {
        assert_eq!(
            issue(IssueState::Open, Some(IssueStateReason::Reopened)).display_status(),
            IssueDisplayStatus::Open
        );
    }
}

//! CI status. GitHub runs two independent systems and a client must merge
//! them; the precedence is fixed here so every surface agrees.
//!
//! See `spec/10-domain-model.md` §3.3.

use serde::{Deserialize, Serialize};

/// Legacy commit statuses. Verified against the live schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StatusState {
    Expected,
    Error,
    Failure,
    Pending,
    Success,
}

/// Check run lifecycle. Verified against the live schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CheckStatus {
    Requested,
    Queued,
    InProgress,
    Completed,
    Waiting,
    Pending,
}

/// Check run outcome. Verified against the live schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CheckConclusion {
    ActionRequired,
    TimedOut,
    Cancelled,
    Failure,
    Success,
    Neutral,
    Skipped,
    StartupFailure,
    Stale,
}

/// How one signal folds into the rollup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    Failed,
    Pending,
    Passed,
    Skipped,
}

impl CheckConclusion {
    fn signal(self) -> Signal {
        match self {
            // A cancelled or timed-out build is not a green one.
            Self::Failure
            | Self::Cancelled
            | Self::TimedOut
            | Self::StartupFailure
            | Self::ActionRequired => Signal::Failed,
            Self::Success => Signal::Passed,
            Self::Neutral | Self::Skipped | Self::Stale => Signal::Skipped,
        }
    }
}

impl StatusState {
    fn signal(self) -> Signal {
        match self {
            Self::Failure | Self::Error => Signal::Failed,
            Self::Pending | Self::Expected => Signal::Pending,
            Self::Success => Signal::Passed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub name: String,
    pub status: CheckStatus,
    /// `None` until the run completes.
    pub conclusion: Option<CheckConclusion>,
    pub url: Option<String>,
}

impl CheckRun {
    fn signal(&self) -> Signal {
        match (self.status, self.conclusion) {
            (CheckStatus::Completed, Some(c)) => c.signal(),
            // Completed with no conclusion is malformed; treat as pending
            // rather than inventing a verdict.
            _ => Signal::Pending,
        }
    }
}

/// A legacy commit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitStatus {
    pub context: String,
    pub state: StatusState,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollupState {
    Success,
    Failure,
    Pending,
    Neutral,
    /// No CI configured at all — distinct from everything being skipped.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRollup {
    pub state: RollupState,
    pub passed: u16,
    pub failed: u16,
    pub pending: u16,
    pub skipped: u16,
    /// Empty in list contexts; populated in detail views.
    pub runs: Vec<CheckRun>,
}

impl CheckRollup {
    /// No CI at all.
    pub fn empty() -> Self {
        Self {
            state: RollupState::None,
            passed: 0,
            failed: 0,
            pending: 0,
            skipped: 0,
            runs: Vec::new(),
        }
    }

    /// Merge both CI systems into one verdict.
    ///
    /// Precedence, per `spec/10-domain-model.md` §3.3: any failure wins; else
    /// any pending; else any success; else neutral/skipped only; else none.
    pub fn merge(runs: Vec<CheckRun>, statuses: &[CommitStatus]) -> Self {
        let mut r = Self::empty();
        let signals = runs
            .iter()
            .map(CheckRun::signal)
            .chain(statuses.iter().map(|s| s.state.signal()));

        for signal in signals {
            match signal {
                Signal::Failed => r.failed += 1,
                Signal::Pending => r.pending += 1,
                Signal::Passed => r.passed += 1,
                Signal::Skipped => r.skipped += 1,
            }
        }

        r.state = if r.failed > 0 {
            RollupState::Failure
        } else if r.pending > 0 {
            RollupState::Pending
        } else if r.passed > 0 {
            RollupState::Success
        } else if r.skipped > 0 {
            RollupState::Neutral
        } else {
            RollupState::None
        };
        r.runs = runs;
        r
    }

    pub fn total(&self) -> u16 {
        self.passed + self.failed + self.pending + self.skipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: CheckStatus, conclusion: Option<CheckConclusion>) -> CheckRun {
        CheckRun {
            name: "build".into(),
            status,
            conclusion,
            url: None,
        }
    }

    fn done(c: CheckConclusion) -> CheckRun {
        run(CheckStatus::Completed, Some(c))
    }

    fn status(state: StatusState) -> CommitStatus {
        CommitStatus {
            context: "ci/legacy".into(),
            state,
            url: None,
        }
    }

    #[test]
    fn no_ci_is_distinct_from_everything_skipped() {
        assert_eq!(CheckRollup::merge(vec![], &[]).state, RollupState::None);
        assert_eq!(
            CheckRollup::merge(vec![done(CheckConclusion::Skipped)], &[]).state,
            RollupState::Neutral
        );
    }

    #[test]
    fn failure_beats_everything() {
        let r = CheckRollup::merge(
            vec![
                done(CheckConclusion::Success),
                done(CheckConclusion::Failure),
                run(CheckStatus::InProgress, None),
            ],
            &[],
        );
        assert_eq!(r.state, RollupState::Failure);
        assert_eq!((r.passed, r.failed, r.pending), (1, 1, 1));
    }

    #[test]
    fn cancelled_and_timed_out_are_failures_not_green() {
        for c in [
            CheckConclusion::Cancelled,
            CheckConclusion::TimedOut,
            CheckConclusion::StartupFailure,
            CheckConclusion::ActionRequired,
        ] {
            let r = CheckRollup::merge(vec![done(CheckConclusion::Success), done(c)], &[]);
            assert_eq!(r.state, RollupState::Failure, "{c:?} must count as failure");
        }
    }

    #[test]
    fn pending_beats_success() {
        let r = CheckRollup::merge(
            vec![
                done(CheckConclusion::Success),
                run(CheckStatus::Queued, None),
            ],
            &[],
        );
        assert_eq!(r.state, RollupState::Pending);
    }

    #[test]
    fn both_ci_systems_merge_into_one_verdict() {
        // The case a real client must handle: check runs green, legacy status red.
        let r = CheckRollup::merge(
            vec![done(CheckConclusion::Success)],
            &[status(StatusState::Failure)],
        );
        assert_eq!(r.state, RollupState::Failure);
        assert_eq!(r.total(), 2);
    }

    #[test]
    fn completed_without_a_conclusion_is_pending_not_a_verdict() {
        let r = CheckRollup::merge(vec![run(CheckStatus::Completed, None)], &[]);
        assert_eq!(r.state, RollupState::Pending);
    }
}

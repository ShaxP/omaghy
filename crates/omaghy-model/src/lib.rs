//! The vocabulary every other omaghy crate speaks.
//!
//! This crate depends on nothing but `serde`, `time` and `thiserror`, and it
//! is the contract that lets the rest be built in parallel
//! (`spec/90-plan.md` §2).
//!
//! Two rules hold everywhere:
//!
//! **We own the vocabulary; GitHub does not.** API shapes are a wire format,
//! translated at the `omaghy-api` boundary. No GraphQL-generated type and no
//! `serde_json::Value` reaches this crate.
//!
//! **Computed display state belongs here, not in the view.** Rollups, merged
//! draft/state, review precedence and relative age all resolve in the model,
//! so that two surfaces cannot disagree about a domain fact.
//!
//! See `spec/10-domain-model.md`.

pub mod actor;
pub mod age;
pub mod checks;
pub mod content;
pub mod error;
pub mod ids;
pub mod issue;
pub mod label;
pub mod notification;
pub mod pull_request;
pub mod repo;
pub mod review;
pub mod timeline;
pub mod validators;

pub use actor::Actor;
pub use checks::{
    CheckConclusion, CheckRollup, CheckRun, CheckStatus, CommitStatus, RollupState, StatusState,
};
pub use content::{Block, Markdown, SpanStyle, StyledSpan};
pub use error::{AuthError, CacheError, LimitKind, Result, StoreError};
pub use ids::{NodeId, NotificationId, SubjectId, SubjectKind, SubjectRef};
pub use issue::{Issue, IssueDisplayStatus, IssueState, IssueStateReason};
pub use label::{Label, Rgb};
pub use notification::{
    Enrichment, Notification, NotificationReason, SubjectDetail, SubjectStatus,
};
pub use pull_request::{Mergeable, PrDetail, PrDisplayStatus, PrState, PullRequest};
pub use repo::{Repo, RepoRef};
pub use review::{ReviewDecision, ReviewState, ReviewSummary};
pub use timeline::{
    Reactions, ReviewThread, ThreadComment, TimelineEntry, TimelineEvent, TimelineKind, fold,
};
pub use validators::Validators;

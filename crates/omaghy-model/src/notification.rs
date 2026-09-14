//! Notifications, and the enrichment problem.
//!
//! The REST payload carries only `id`, `unread`, `reason`, `updated_at`,
//! `subject{title,type,url}` and `repository`. It does **not** carry the
//! number, the state, who acted, or a browser URL — so a useful row needs a
//! second pass, which lands *after* first paint.
//!
//! See `spec/10-domain-model.md` §3.5.

use crate::{
    actor::Actor,
    checks::CheckRollup,
    ids::{NotificationId, SubjectKind, SubjectRef},
    issue::IssueDisplayStatus,
    pull_request::PrDisplayStatus,
    repo::RepoRef,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Why this notification exists. The most information-dense field in the
/// payload, and the main axis for visual differentiation.
///
/// Like [`crate::SubjectKind`], the `serde` representation is omaghy's storage
/// format. `Other` round-trips as `{"other": "..."}` — it is an escape hatch
/// for *our* cache, not a way to swallow an unrecognised wire string. The
/// translation from GitHub's `"a_reason_from_2028"` happens in `omaghy-api`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationReason {
    ReviewRequested,
    Mention,
    TeamMention,
    Assign,
    Author,
    Comment,
    StateChange,
    CiActivity,
    Subscribed,
    Manual,
    Invitation,
    SecurityAlert,
    /// GitHub adds reasons without warning.
    Other(String),
}

impl NotificationReason {
    pub fn label(&self) -> &str {
        match self {
            Self::ReviewRequested => "review requested",
            Self::Mention => "mentioned",
            Self::TeamMention => "team mentioned",
            Self::Assign => "assigned",
            Self::Author => "author",
            Self::Comment => "commented",
            Self::StateChange => "state changed",
            Self::CiActivity => "ci",
            Self::Subscribed => "subscribed",
            Self::Manual => "manual",
            Self::Invitation => "invited",
            Self::SecurityAlert => "security",
            Self::Other(s) => s,
        }
    }

    /// Whether this reason means the viewer is personally on the hook, as
    /// opposed to merely subscribed. Drives dashboard sectioning and ordering.
    pub fn is_directed_at_me(&self) -> bool {
        matches!(
            self,
            Self::ReviewRequested | Self::Mention | Self::TeamMention | Self::Assign
        )
    }
}

/// The four states a second-pass fetch can be in.
///
/// Deliberately not `Option`: a row must render from `Absent` or `Pending` on
/// the first frame and re-render on `Ready` **without changing height**, and
/// `Failed` must not be retried in a loop.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enrichment<T> {
    /// Never requested.
    #[default]
    Absent,
    /// Requested, in flight.
    Pending,
    /// Permanently failed — a repo you lost access to, a deleted subject.
    Failed {
        reason: String,
    },
    /// There is nothing to fetch. A commit has no state and no number; a
    /// release has no check run. Distinct from [`Enrichment::Failed`], which
    /// means we tried and could not: writing "not applicable" as a failure
    /// makes one state do the work of two, and makes an ordinary subject look
    /// broken in the UI. Found by W2.1, which had to do exactly that.
    NotApplicable,
    Ready(T),
}

impl<T> Enrichment<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(v) => Some(v),
            _ => None,
        }
    }

    /// Whether a fetch is worth scheduling. `Failed` is terminal — retrying it
    /// on every open is how a lost-access repo burns the rate limit — and so
    /// is `NotApplicable`, which will never become applicable.
    pub fn wants_fetch(&self) -> bool {
        matches!(self, Self::Absent)
    }

    pub fn is_settled(&self) -> bool {
        matches!(
            self,
            Self::Ready(_) | Self::Failed { .. } | Self::NotApplicable
        )
    }

    /// Whether the absence of detail is something to apologise for.
    ///
    /// A commit row showing no state is correct; a pull request showing none
    /// because the fetch failed is not, and only one of them deserves a mark
    /// in the UI.
    pub fn is_a_problem(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// How a subject stands, once enrichment resolves it.
///
/// Not every subject has one: a commit is neither open nor merged, and a
/// release has no state at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectStatus {
    PullRequest(PrDisplayStatus),
    Issue(IssueDisplayStatus),
    /// Nothing we fetch reads as a state: a commit, a release, a check suite,
    /// or a discussion. Deliberately not a `Discussion { answered, locked }`
    /// variant — the enrichment query does not ask for those fields, and a
    /// variant whose data nobody fetches only invites them to be invented.
    None,
}

/// What one batched GraphQL query adds to a whole page of notifications.
///
/// `number` and `status` are optional because two of the seven subject kinds
/// cannot supply them: a commit is addressed by SHA and has no state, and a
/// release has neither. Modelling them as required made W2.1 record ordinary
/// commits as `Enrichment::Failed` purely to stop them being re-fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectDetail {
    /// `None` for SHA-addressed subjects.
    pub number: Option<u64>,
    pub status: SubjectStatus,
    pub checks: CheckRollup,
    pub last_actor: Option<Actor>,
    pub html_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub id: NotificationId,
    pub unread: bool,
    pub reason: NotificationReason,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub title: String,
    pub kind: SubjectKind,
    pub repo: RepoRef,
    /// Parsed from the API URL. `None` when it is not a subject URL we can
    /// address — the row still renders, it just cannot be opened.
    pub subject: Option<SubjectRef>,
    pub detail: Enrichment<SubjectDetail>,
}

impl Notification {
    /// Where `o` should take the viewer.
    ///
    /// Prefers the enriched URL, falls back to one derived from the API URL,
    /// so an unenriched row is still actionable.
    pub fn browser_url(&self) -> Option<String> {
        if let Enrichment::Ready(d) = &self.detail {
            return Some(d.html_url.clone());
        }
        self.subject.as_ref().and_then(SubjectRef::browser_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SubjectId;
    use time::macros::datetime;

    fn notification(detail: Enrichment<SubjectDetail>) -> Notification {
        Notification {
            id: NotificationId("24555446034".into()),
            unread: false,
            reason: NotificationReason::Author,
            updated_at: datetime!(2026-07-10 16:04:40 UTC),
            title: "fix: syntax highlighting follows the Dark/Light/System toggle".into(),
            kind: SubjectKind::PullRequest,
            repo: RepoRef::new("ShaxP", "shax"),
            subject: SubjectRef::from_api_url("https://api.github.com/repos/ShaxP/shax/pulls/61"),
            detail,
        }
    }

    #[test]
    fn an_unenriched_row_is_still_openable() {
        let n = notification(Enrichment::Absent);
        assert_eq!(
            n.browser_url().as_deref(),
            Some("https://github.com/ShaxP/shax/pull/61")
        );
    }

    #[test]
    fn enrichment_url_wins_when_present() {
        let n = notification(Enrichment::Ready(SubjectDetail {
            number: Some(61),
            status: SubjectStatus::PullRequest(PrDisplayStatus::Merged),
            checks: CheckRollup::empty(),
            last_actor: None,
            html_url: "https://github.com/ShaxP/shax/pull/61#issuecomment-1".into(),
        }));
        assert!(n.browser_url().unwrap().contains("#issuecomment-1"));
    }

    #[test]
    fn failed_enrichment_is_terminal() {
        // Retrying a repo you lost access to on every open burns the rate limit.
        let failed: Enrichment<SubjectDetail> = Enrichment::Failed {
            reason: "403".into(),
        };
        assert!(!failed.wants_fetch());
        assert!(failed.is_settled());

        let absent: Enrichment<SubjectDetail> = Enrichment::Absent;
        assert!(absent.wants_fetch());
        assert!(!absent.is_settled());

        let pending: Enrichment<SubjectDetail> = Enrichment::Pending;
        assert!(
            !pending.wants_fetch(),
            "must not double-schedule an in-flight fetch"
        );
    }

    #[test]
    fn unparseable_subject_still_yields_a_row() {
        let mut n = notification(Enrichment::Absent);
        n.subject = None;
        assert_eq!(n.browser_url(), None);
        assert!(!n.title.is_empty());
    }

    #[test]
    fn directed_reasons_are_distinguished_from_subscriptions() {
        assert!(NotificationReason::ReviewRequested.is_directed_at_me());
        assert!(NotificationReason::Mention.is_directed_at_me());
        assert!(!NotificationReason::Subscribed.is_directed_at_me());
        assert!(!NotificationReason::CiActivity.is_directed_at_me());
        assert_eq!(
            NotificationReason::Other("new_thing".into()).label(),
            "new_thing"
        );
    }

    #[test]
    fn subject_id_renders_for_both_shapes() {
        assert_eq!(SubjectId::Number(61).to_string(), "#61");
    }
}

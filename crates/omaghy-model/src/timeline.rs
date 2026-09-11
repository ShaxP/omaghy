//! Timeline events — the flattening.
//!
//! Verified against the live schema: `PullRequestTimelineItems` has **78**
//! members and `IssueTimelineItems` **51**. We model the ones worth rendering
//! distinctly and collapse the rest into [`TimelineKind::Other`], which keeps
//! the schema type name so an unmodelled event still renders as a line rather
//! than vanishing.
//!
//! See `spec/10-domain-model.md` §3.4.

use crate::{
    actor::Actor,
    content::Markdown,
    ids::{NodeId, SubjectRef},
    label::Label,
    review::ReviewState,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewThread {
    pub path: String,
    pub line: Option<u32>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub comments: Vec<ThreadComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadComment {
    pub author: Option<Actor>,
    pub body: Markdown,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Reactions {
    pub thumbs_up: u16,
    pub thumbs_down: u16,
    pub laugh: u16,
    pub hooray: u16,
    pub confused: u16,
    pub heart: u16,
    pub rocket: u16,
    pub eyes: u16,
}

impl Reactions {
    pub fn total(&self) -> u16 {
        self.thumbs_up
            + self.thumbs_down
            + self.laugh
            + self.hooray
            + self.confused
            + self.heart
            + self.rocket
            + self.eyes
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineKind {
    Comment {
        body: Markdown,
        reactions: Reactions,
        edited: bool,
    },
    Review {
        state: ReviewState,
        body: Option<Markdown>,
        threads: Vec<ReviewThread>,
    },
    ReviewThread(ReviewThread),
    Commit {
        oid: String,
        message_headline: String,
        authored_by: Option<Actor>,
    },
    Merged {
        commit: Option<String>,
        base: String,
    },
    Closed {
        by_commit: Option<String>,
    },
    Reopened,
    ReadyForReview,
    ConvertedToDraft,
    Renamed {
        from: String,
        to: String,
    },
    Labeled {
        label: Label,
    },
    Unlabeled {
        label: Label,
    },
    Assigned {
        who: Actor,
    },
    Unassigned {
        who: Actor,
    },
    ReviewRequested {
        who: Actor,
    },
    ReviewRequestRemoved {
        who: Actor,
    },
    CrossReferenced {
        source: SubjectRef,
        will_close: bool,
    },
    HeadRefForcePushed {
        before: String,
        after: String,
    },
    /// Everything else — roughly sixty further schema types. Carries the
    /// GraphQL type name so it can still be rendered and, eventually, promoted.
    Other {
        kind: String,
    },
}

impl TimelineKind {
    /// Whether this event carries prose a reader would stop to read.
    ///
    /// Drives the fold in `spec/10-domain-model.md` §3.4: runs of low-signal
    /// events collapse behind "*n more events*". Timelines on active PRs are
    /// dominated by noise, and rendering all 78 kinds equally is unusable.
    pub fn is_substantive(&self) -> bool {
        matches!(
            self,
            Self::Comment { .. }
                | Self::Review { .. }
                | Self::ReviewThread(_)
                | Self::Merged { .. }
                | Self::Closed { .. }
                | Self::Reopened
                | Self::ReadyForReview
                | Self::ConvertedToDraft
        )
    }

    /// The GraphQL type name for unmodelled events, if this is one.
    pub fn unmodelled_kind(&self) -> Option<&str> {
        match self {
            Self::Other { kind } => Some(kind),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub node_id: Option<NodeId>,
    pub actor: Option<Actor>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub kind: TimelineKind,
}

/// One entry in a rendered timeline: either an event, or a collapsed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineEntry<'a> {
    Event(&'a TimelineEvent),
    Folded { events: &'a [TimelineEvent] },
}

/// Collapse runs of low-signal events.
///
/// Runs shorter than `min_run` are left expanded — folding a single "labeled"
/// event behind "1 more event" costs a line and saves none.
pub fn fold(events: &[TimelineEvent], min_run: usize) -> Vec<TimelineEntry<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < events.len() {
        if events[i].kind.is_substantive() {
            out.push(TimelineEntry::Event(&events[i]));
            i += 1;
            continue;
        }
        let start = i;
        while i < events.len() && !events[i].kind.is_substantive() {
            i += 1;
        }
        let run = &events[start..i];
        if run.len() >= min_run {
            out.push(TimelineEntry::Folded { events: run });
        } else {
            out.extend(run.iter().map(TimelineEntry::Event));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn ev(kind: TimelineKind) -> TimelineEvent {
        TimelineEvent {
            node_id: None,
            actor: Some(Actor::new("octocat")),
            at: datetime!(2026-09-10 12:00 UTC),
            kind,
        }
    }

    fn label() -> Label {
        Label {
            name: "bug".into(),
            color: crate::label::Rgb {
                r: 0xd7,
                g: 0x3a,
                b: 0x4a,
            },
            description: None,
        }
    }

    fn comment() -> TimelineEvent {
        ev(TimelineKind::Comment {
            body: Markdown::from_source("looks good"),
            reactions: Reactions::default(),
            edited: false,
        })
    }

    #[test]
    fn unmodelled_events_keep_their_schema_name() {
        let e = ev(TimelineKind::Other {
            kind: "AddedToMergeQueueEvent".into(),
        });
        assert_eq!(e.kind.unmodelled_kind(), Some("AddedToMergeQueueEvent"));
        assert!(!e.kind.is_substantive());
    }

    #[test]
    fn folds_runs_of_noise_but_keeps_prose() {
        let events = vec![
            comment(),
            ev(TimelineKind::Labeled { label: label() }),
            ev(TimelineKind::Unlabeled { label: label() }),
            ev(TimelineKind::Assigned {
                who: Actor::new("a"),
            }),
            comment(),
        ];
        let folded = fold(&events, 2);
        assert_eq!(folded.len(), 3);
        assert!(matches!(folded[0], TimelineEntry::Event(_)));
        assert!(matches!(folded[1], TimelineEntry::Folded { events } if events.len() == 3));
        assert!(matches!(folded[2], TimelineEntry::Event(_)));
    }

    #[test]
    fn short_runs_stay_expanded() {
        // Folding one event behind "1 more event" costs a line and saves none.
        let events = vec![
            comment(),
            ev(TimelineKind::Labeled { label: label() }),
            comment(),
        ];
        let folded = fold(&events, 2);
        assert_eq!(folded.len(), 3);
        assert!(folded.iter().all(|e| matches!(e, TimelineEntry::Event(_))));
    }

    #[test]
    fn an_all_noise_timeline_folds_to_one_entry() {
        let events: Vec<_> = (0..9)
            .map(|_| ev(TimelineKind::Labeled { label: label() }))
            .collect();
        assert_eq!(fold(&events, 2).len(), 1);
    }

    #[test]
    fn empty_timeline_folds_to_nothing() {
        assert!(fold(&[], 2).is_empty());
    }
}

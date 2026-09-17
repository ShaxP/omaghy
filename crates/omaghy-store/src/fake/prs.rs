//! The pull-request corpus.
//!
//! Twelve curated pull requests, built as details so one corpus serves both
//! reads: `pull_requests` strips each to its row, `pull_request` hands one
//! over whole. Eight share a coordinate with a `PullRequest` row of the
//! notification corpus (`ShaxP/shax#4` is both the fourth notification and
//! a PR here), so `Enter` on a notification lands on a detail in the fixture
//! store, not on "never fetched".
//!
//! Covers every `PrDisplayStatus`, every `RollupState`, every `Mergeable`,
//! every `ReviewDecision`, a ghost author, a bot, a title too long for any
//! column, a repository name too long for any column, a PR with no labels and
//! one with six, an empty body and one with a code block and a task list, and
//! a timeline long enough and noisy enough for `fold` to have work to do —
//! plus one with a timeline of nothing at all, which is what a PR opened a
//! minute ago looks like.

use super::FIXTURE_NOW;
use omaghy_model::{
    Actor, CheckConclusion, CheckRollup, CheckRun, CheckStatus, CommitStatus, Label, Markdown,
    Mergeable, NodeId, PrDetail, PrState, PullRequest, Reactions, RepoRef, ReviewDecision,
    ReviewState, ReviewSummary, ReviewThread, Rgb, StatusState, SubjectKind, SubjectRef,
    ThreadComment, TimelineEvent, TimelineKind,
};
use time::{Duration, OffsetDateTime};

/// The viewer the corpus is built around. Must agree with `FakeStore`'s.
pub const ME: &str = "ShaxP";

fn ago(mins: i64) -> OffsetDateTime {
    FIXTURE_NOW - Duration::minutes(mins)
}

fn who(login: &str) -> Actor {
    Actor::new(login)
}

fn bot(login: &str) -> Actor {
    Actor {
        is_bot: true,
        ..Actor::new(login)
    }
}

fn label(name: &str, hex: &str) -> Label {
    Label {
        name: name.into(),
        color: Rgb::parse_hex(hex).expect("fixture colours are well-formed"),
        description: None,
    }
}

fn md(source: &str) -> Markdown {
    Markdown::from_source(source)
}

fn run(name: &str, conclusion: Option<CheckConclusion>) -> CheckRun {
    CheckRun {
        name: name.into(),
        status: match conclusion {
            Some(_) => CheckStatus::Completed,
            None => CheckStatus::InProgress,
        },
        conclusion,
        url: None,
    }
}

fn ev(actor: Option<&str>, mins: i64, kind: TimelineKind) -> TimelineEvent {
    TimelineEvent {
        node_id: None,
        actor: actor.map(who),
        at: ago(mins),
        kind,
    }
}

fn comment(actor: &str, mins: i64, body: &str) -> TimelineEvent {
    ev(
        Some(actor),
        mins,
        TimelineKind::Comment {
            body: md(body),
            reactions: Reactions::default(),
            edited: false,
        },
    )
}

fn commit(actor: &str, mins: i64, oid: &str, headline: &str) -> TimelineEvent {
    ev(
        Some(actor),
        mins,
        TimelineKind::Commit {
            oid: oid.into(),
            message_headline: headline.into(),
            authored_by: Some(who(actor)),
        },
    )
}

fn review(actor: &str, mins: i64, state: ReviewState, body: Option<&str>) -> TimelineEvent {
    ev(
        Some(actor),
        mins,
        TimelineKind::Review {
            state,
            body: body.map(md),
            threads: Vec::new(),
        },
    )
}

fn thread(path: &str, line: u32, resolved: bool, comments: &[(&str, i64, &str)]) -> ReviewThread {
    ReviewThread {
        path: path.into(),
        line: Some(line),
        is_resolved: resolved,
        is_outdated: false,
        comments: comments
            .iter()
            .map(|(a, mins, body)| ThreadComment {
                author: Some(who(a)),
                body: md(body),
                at: ago(*mins),
            })
            .collect(),
    }
}

/// Everything a row needs, with the boring defaults filled in.
#[allow(clippy::too_many_arguments)]
fn row(
    repo: &str,
    number: u64,
    title: &str,
    author: Option<&str>,
    state: PrState,
    created_mins: i64,
    updated_mins: i64,
) -> PullRequest {
    PullRequest {
        node_id: NodeId(format!("PR_fixture_{}_{number}", repo.replace('/', "_"))),
        number,
        repo: RepoRef::parse(repo).expect("fixture repo names are well-formed"),
        title: title.into(),
        author: author.map(who),
        state,
        is_draft: false,
        mergeable: Mergeable::Mergeable,
        labels: Vec::new(),
        review: ReviewSummary::default(),
        checks: CheckRollup::empty(),
        comment_count: 0,
        additions: 0,
        deletions: 0,
        changed_files: 0,
        created_at: ago(created_mins),
        updated_at: ago(updated_mins),
    }
}

/// All green, three runs.
fn green() -> CheckRollup {
    CheckRollup::merge(
        vec![
            run("build · clippy · test", Some(CheckConclusion::Success)),
            run("fmt", Some(CheckConclusion::Success)),
            run("docs", Some(CheckConclusion::Success)),
        ],
        &[],
    )
}

pub fn pr_corpus() -> Vec<PrDetail> {
    let mut out = Vec::new();

    // 1. Needs my review, and CI is red. The row the dashboard exists for.
    //    Shares a coordinate with notification 1.
    {
        let mut pr = row(
            "quickshell/quickshell",
            1,
            "Add SocketServer reconnect backoff and idle timeout",
            Some("outfoxxed"),
            PrState::Open,
            2_900,
            4,
        );
        pr.labels = vec![
            label("area:core", "0e8a16"),
            label("needs-review", "fbca04"),
        ];
        pr.review = ReviewSummary {
            decision: Some(ReviewDecision::ReviewRequired),
            reviewers: vec![(who("nixie"), ReviewState::Commented)],
            i_am_requested: true,
            my_review: None,
        };
        pr.checks = CheckRollup::merge(
            vec![
                run("build (x86_64)", Some(CheckConclusion::Success)),
                run("build (aarch64)", Some(CheckConclusion::Success)),
                run("test", Some(CheckConclusion::Failure)),
            ],
            &[CommitStatus {
                context: "codecov/patch".into(),
                state: StatusState::Failure,
                url: None,
            }],
        );
        pr.comment_count = 3;
        pr.additions = 312;
        pr.deletions = 48;
        pr.changed_files = 6;
        let threads = vec![thread(
            "src/io/SocketServer.cpp",
            142,
            false,
            &[
                (
                    "nixie",
                    180,
                    "This retries forever. Should there be a ceiling on the backoff?",
                ),
                (
                    "outfoxxed",
                    150,
                    "Capped at 30s now — see the follow-up commit.",
                ),
            ],
        )];
        let mut r = review(
            "nixie",
            180,
            ReviewState::Commented,
            Some("A couple of questions inline; nothing blocking."),
        );
        if let TimelineKind::Review { threads: t, .. } = &mut r.kind {
            *t = threads;
        }
        out.push(PrDetail {
            pr,
            body: md("Reconnects with exponential backoff (250ms → 30s) and drops a client after 60s idle.\n\nFixes #1187.\n\n- [x] backoff\n- [x] idle timeout\n- [ ] docs"),
            base_ref: "master".into(),
            head_ref: "socket-reconnect".into(),
            timeline: vec![
                commit("outfoxxed", 2_900, "a1b2c3d4e5f6", "Add reconnect backoff"),
                ev(Some("outfoxxed"), 2_890, TimelineKind::ReviewRequested { who: who(ME) }),
                ev(Some("outfoxxed"), 2_890, TimelineKind::Labeled { label: label("area:core", "0e8a16") }),
                ev(Some("outfoxxed"), 2_890, TimelineKind::Labeled { label: label("needs-review", "fbca04") }),
                r,
                commit("outfoxxed", 150, "b2c3d4e5f6a1", "Cap the backoff at 30s"),
                comment("outfoxxed", 4, "CI is red on the new test — looking."),
            ],
        });
    }

    // 2. Mine, approved, green. Shares a coordinate with notification 4.
    {
        let mut pr = row(
            "ShaxP/shax",
            4,
            "fix: syntax highlighting follows the Dark/Light/System toggle",
            Some(ME),
            PrState::Open,
            1_500,
            120,
        );
        pr.review = ReviewSummary {
            decision: Some(ReviewDecision::Approved),
            reviewers: vec![(who("mika"), ReviewState::Approved)],
            i_am_requested: false,
            my_review: None,
        };
        pr.checks = green();
        pr.comment_count = 1;
        pr.additions = 140;
        pr.deletions = 22;
        pr.changed_files = 4;
        out.push(PrDetail {
            pr,
            body: md("The highlighter cached its theme at startup. It now re-reads on toggle.\n\n```rust\nfn theme(&self) -> &Theme {\n    self.settings.theme()\n}\n```"),
            base_ref: "main".into(),
            head_ref: "fix/highlight-theme".into(),
            timeline: vec![
                commit(ME, 1_500, "c3d4e5f6a1b2", "fix: re-read the theme on toggle"),
                review("mika", 120, ReviewState::Approved, Some("LGTM.")),
            ],
        });
    }

    // 3. A big upstream PR: six labels, a long noisy timeline, one reviewer
    //    asking for changes and another approving. Shares a coordinate with
    //    notification 6.
    {
        let mut pr = row(
            "rust-lang/rust",
            6,
            "Stabilize `let_chains` in the 2024 edition",
            Some("est-c"),
            PrState::Open,
            40_000,
            300,
        );
        pr.labels = vec![
            label("T-lang", "d4c5f9"),
            label("T-compiler", "d4c5f9"),
            label("S-waiting-on-review", "d4c5f9"),
            label("relnotes", "f7e101"),
            label("F-let_chains", "f7e101"),
            label("disposition-merge", "0e8a16"),
        ];
        pr.review = ReviewSummary {
            decision: Some(ReviewDecision::ChangesRequested),
            reviewers: vec![
                (who("compiler-reviewer"), ReviewState::ChangesRequested),
                (who("lang-reviewer"), ReviewState::Approved),
            ],
            i_am_requested: false,
            my_review: None,
        };
        pr.checks = CheckRollup::merge(
            vec![
                run("PR", Some(CheckConclusion::Success)),
                run("mingw-check", Some(CheckConclusion::Success)),
                run("x86_64-gnu-llvm-18", None),
            ],
            &[],
        );
        pr.comment_count = 41;
        pr.additions = 2_104;
        pr.deletions = 611;
        pr.changed_files = 58;
        out.push(PrDetail {
            pr,
            body: md("Stabilization report for `let_chains` on edition 2024.\n\n# Summary\n\nThis PR stabilizes let chains in `if` and `while` on edition 2024 only.\n\n# Tests\n\n- `tests/ui/rfcs/rfc-2497-if-let-chains/`\n\ncc @rust-lang/lang"),
            base_ref: "master".into(),
            head_ref: "stabilize-let-chains".into(),
            timeline: vec![
                comment("rustbot", 40_000, "r? @compiler-reviewer\n\nrustbot has assigned @compiler-reviewer."),
                ev(Some("rustbot"), 40_000, TimelineKind::Labeled { label: label("T-lang", "d4c5f9") }),
                ev(Some("rustbot"), 40_000, TimelineKind::Labeled { label: label("T-compiler", "d4c5f9") }),
                ev(Some("rustbot"), 40_000, TimelineKind::Labeled { label: label("S-waiting-on-review", "d4c5f9") }),
                ev(Some("rustbot"), 40_000, TimelineKind::Assigned { who: who("compiler-reviewer") }),
                ev(Some("est-c"), 39_990, TimelineKind::Other { kind: "AddedToProjectEvent".into() }),
                ev(Some("est-c"), 39_990, TimelineKind::Other { kind: "MilestonedEvent".into() }),
                ev(Some("est-c"), 39_980, TimelineKind::ReviewRequested { who: who("lang-reviewer") }),
                comment("lang-reviewer", 30_000, "@rfcbot fcp merge"),
                comment("rfcbot", 29_990, "Team member @lang-reviewer has proposed to merge this. The next step is review by the rest of the tagged team members."),
                ev(Some("rfcbot"), 29_990, TimelineKind::Labeled { label: label("disposition-merge", "0e8a16") }),
                ev(Some("rfcbot"), 29_990, TimelineKind::Other { kind: "ProposedToMergeEvent".into() }),
                review("compiler-reviewer", 20_000, ReviewState::ChangesRequested, Some("The edition gate is checked in two places and they disagree. Please consolidate.")),
                commit("est-c", 10_000, "d4e5f6a1b2c3", "Consolidate the edition check"),
                commit("est-c", 9_990, "e5f6a1b2c3d4", "Add a test for the 2021 edition"),
                ev(Some("est-c"), 9_980, TimelineKind::HeadRefForcePushed { before: "d4e5f6a1b2c3".into(), after: "f6a1b2c3d4e5".into() }),
                ev(Some("est-c"), 9_970, TimelineKind::Renamed { from: "Stabilize let_chains".into(), to: "Stabilize `let_chains` in the 2024 edition".into() }),
                review("lang-reviewer", 5_000, ReviewState::Approved, None),
                ev(Some("someone-else"), 2_000, TimelineKind::CrossReferenced {
                    source: SubjectRef::parse_numbered("rust-lang/rust#7", SubjectKind::Issue).unwrap(),
                    will_close: false,
                }),
                comment("est-c", 300, "Rebased on master; the LLVM 18 job is still running."),
            ],
        });
    }

    // 4. A draft with a title no column can hold, no CI at all, and a force
    //    push. Shares a coordinate with notification 8.
    {
        let mut pr = row(
            "quickshell/quickshell",
            8,
            "Refactor the Wayland layer-shell surface lifecycle so that a surface is created lazily on first show and destroyed on hide, rather than living for the duration of the window",
            Some("outfoxxed"),
            PrState::Open,
            5_000,
            480,
        );
        pr.is_draft = true;
        pr.mergeable = Mergeable::Unknown;
        pr.additions = 1_204;
        pr.deletions = 987;
        pr.changed_files = 31;
        out.push(PrDetail {
            pr,
            body: md(""),
            base_ref: "master".into(),
            head_ref: "wip/layer-shell-lifecycle".into(),
            timeline: vec![
                commit("outfoxxed", 5_000, "a2b3c4d5e6f7", "wip"),
                ev(Some("outfoxxed"), 4_990, TimelineKind::ConvertedToDraft),
                commit("outfoxxed", 500, "b3c4d5e6f7a2", "wip: destroy on hide"),
                ev(
                    Some("outfoxxed"),
                    480,
                    TimelineKind::HeadRefForcePushed {
                        before: "a2b3c4d5e6f7".into(),
                        after: "b3c4d5e6f7a2".into(),
                    },
                ),
            ],
        });
    }

    // 5. Mine, conflicting, green, no reviewer yet. Shares a coordinate with
    //    notification 11.
    {
        let mut pr = row(
            "ShaxP/shax",
            11,
            "M7 slice 1: light theme + Dark/Light/System toggle",
            Some(ME),
            PrState::Open,
            2_000,
            15,
        );
        pr.mergeable = Mergeable::Conflicting;
        pr.checks = green();
        pr.additions = 488;
        pr.deletions = 96;
        pr.changed_files = 12;
        out.push(PrDetail {
            pr,
            body: md("Adds a light theme and a three-way toggle.\n\nStacked on #4."),
            base_ref: "main".into(),
            head_ref: "feat/m7-light-theme".into(),
            timeline: vec![
                commit(ME, 2_000, "c4d5e6f7a2b3", "M7 slice 1: light theme"),
                commit(ME, 1_000, "d5e6f7a2b3c4", "the toggle"),
                comment(
                    ME,
                    15,
                    "Conflicts with #4 after its rebase; will resolve once that merges.",
                ),
            ],
        });
    }

    // 6–8. Merged. Two of mine and one bot-authored, ageing from days to
    //      weeks. Coordinates shared with notifications 12, 13 and 26.
    for (number, title, author, created, updated, adds, dels, files) in [
        (
            12,
            "fix: tighter markdown spacing in chat bubbles",
            ME,
            3_000,
            2_880,
            18,
            24,
            2,
        ),
        (
            13,
            "Ollama per-model tool + vision probing (closes M6)",
            ME,
            6_000,
            3_240,
            640,
            88,
            9,
        ),
        (
            26,
            "M5 slice 3: ls widget (closes M5)",
            ME,
            90_000,
            89_280,
            1_120,
            140,
            17,
        ),
    ] {
        let mut pr = row(
            "ShaxP/shax",
            number,
            title,
            Some(author),
            PrState::Merged,
            created,
            updated,
        );
        pr.checks = green();
        pr.review = ReviewSummary {
            decision: Some(ReviewDecision::Approved),
            reviewers: vec![(who("mika"), ReviewState::Approved)],
            i_am_requested: false,
            my_review: None,
        };
        pr.comment_count = 2;
        pr.additions = adds;
        pr.deletions = dels;
        pr.changed_files = files;
        out.push(PrDetail {
            pr,
            body: md("See the milestone."),
            base_ref: "main".into(),
            head_ref: format!("feat/pr-{number}"),
            timeline: vec![
                commit(author, created, "e6f7a2b3c4d5", title),
                review("mika", updated + 10, ReviewState::Approved, None),
                ev(
                    Some(author),
                    updated,
                    TimelineKind::Merged {
                        commit: Some("f7a2b3c4d5e6".into()),
                        base: "main".into(),
                    },
                ),
            ],
        });
    }

    // 9. Merged, from a bot, on the second repository of mine. Shares a
    //    coordinate with notification 16.
    {
        let mut pr = row(
            "ShaxP/omaghy",
            16,
            "spec: notifications inbox domain model",
            Some("dependabot[bot]"),
            PrState::Merged,
            6_000,
            5_760,
        );
        pr.author = Some(bot("dependabot[bot]"));
        pr.labels = vec![label("dependencies", "0366d6")];
        pr.checks = green();
        pr.additions = 3;
        pr.deletions = 3;
        pr.changed_files = 1;
        out.push(PrDetail {
            pr,
            body: md("Bumps `serde` from 1.0.219 to 1.0.220."),
            base_ref: "main".into(),
            head_ref: "dependabot/cargo/serde-1.0.220".into(),
            timeline: vec![ev(
                Some(ME),
                5_760,
                TimelineKind::Merged {
                    commit: Some("a3b4c5d6e7f8".into()),
                    base: "main".into(),
                },
            )],
        });
    }

    // 10. Closed without merging, by an account that no longer exists, with
    //     CI that had failed. Every "ghost" path at once.
    {
        let mut pr = row(
            "basecamp/omarchy",
            2210,
            "Add a Wayland-native screenshot annotator",
            None,
            PrState::Closed,
            50_000,
            43_200,
        );
        pr.mergeable = Mergeable::Unknown;
        pr.checks =
            CheckRollup::merge(vec![run("shellcheck", Some(CheckConclusion::Failure))], &[]);
        pr.comment_count = 5;
        pr.additions = 220;
        pr.deletions = 0;
        pr.changed_files = 3;
        out.push(PrDetail {
            pr,
            body: md("Annotate screenshots without leaving the compositor."),
            base_ref: "master".into(),
            head_ref: "screenshot-annotator".into(),
            timeline: vec![
                comment(
                    "dhh",
                    49_000,
                    "We're not adding a second screenshot tool. Thanks for the effort though.",
                ),
                ev(
                    Some("dhh"),
                    43_200,
                    TimelineKind::Closed { by_commit: None },
                ),
            ],
        });
    }

    // 11. Needs my review, I have already commented but not decided, CI is
    //     still running, and the title is longer than any column.
    {
        let mut pr = row(
            "basecamp/omarchy",
            2305,
            "Allow the bar's workspace indicator to be configured per monitor, so that a portrait secondary display can drop the workspace names and show only the numbers while the primary keeps both",
            Some("mrsk-fan"),
            PrState::Open,
            600,
            40,
        );
        pr.labels = vec![label("enhancement", "a2eeef")];
        pr.review = ReviewSummary {
            decision: Some(ReviewDecision::ReviewRequired),
            reviewers: vec![(who(ME), ReviewState::Commented)],
            i_am_requested: true,
            my_review: Some(ReviewState::Commented),
        };
        pr.checks = CheckRollup::merge(vec![run("lint", None), run("test", None)], &[]);
        pr.comment_count = 2;
        pr.additions = 74;
        pr.deletions = 12;
        pr.changed_files = 3;
        out.push(PrDetail {
            pr,
            body: md("Per-monitor `workspaces.show_names`. Defaults unchanged."),
            base_ref: "master".into(),
            head_ref: "per-monitor-workspaces".into(),
            timeline: vec![
                commit("mrsk-fan", 600, "b4c5d6e7f8a3", "Per-monitor workspace indicator config"),
                ev(Some("mrsk-fan"), 590, TimelineKind::ReviewRequested { who: who(ME) }),
                review(ME, 100, ReviewState::Commented, Some("Does this survive a monitor being unplugged and replugged with a different name?")),
                comment("mrsk-fan", 40, "It does — the config is keyed by the connector name, which is stable. Added a test."),
            ],
        });
    }

    // 12. Requested from me, and I have asked for changes: requested but not
    //     outstanding (`ReviewSummary::awaits_me`). On the repository whose
    //     name no column can hold. Opened a minute ago, no timeline yet —
    //     which is what a brand-new PR looks like.
    {
        let mut pr = row(
            "some-very-long-organization-name/an-equally-long-repository-name",
            7,
            "Short one",
            Some("colleague"),
            PrState::Open,
            1,
            1,
        );
        pr.review = ReviewSummary {
            decision: Some(ReviewDecision::ChangesRequested),
            reviewers: vec![(who(ME), ReviewState::ChangesRequested)],
            i_am_requested: true,
            my_review: Some(ReviewState::ChangesRequested),
        };
        pr.checks = CheckRollup::merge(vec![run("noop", Some(CheckConclusion::Skipped))], &[]);
        pr.additions = 1;
        pr.deletions = 1;
        pr.changed_files = 1;
        out.push(PrDetail {
            pr,
            body: md("Typo."),
            base_ref: "main".into(),
            head_ref: "typo".into(),
            timeline: Vec::new(),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use omaghy_model::{PrDisplayStatus, RollupState, TimelineEntry, fold};
    use std::collections::HashSet;

    #[test]
    fn the_corpus_covers_the_design_space() {
        let c = pr_corpus();
        assert_eq!(c.len(), 12);

        let statuses: HashSet<_> = c.iter().map(|d| d.pr.display_status()).collect();
        for s in [
            PrDisplayStatus::Draft,
            PrDisplayStatus::Open,
            PrDisplayStatus::Merged,
            PrDisplayStatus::Closed,
        ] {
            assert!(statuses.contains(&s), "missing display status {s:?}");
        }

        let rollups: HashSet<_> = c.iter().map(|d| d.pr.checks.state).collect();
        for s in [
            RollupState::Success,
            RollupState::Failure,
            RollupState::Pending,
            RollupState::Neutral,
            RollupState::None,
        ] {
            assert!(rollups.contains(&s), "missing rollup state {s:?}");
        }

        let mergeable: HashSet<_> = c.iter().map(|d| d.pr.mergeable).collect();
        assert_eq!(mergeable.len(), 3, "every Mergeable variant");

        let decisions: HashSet<_> = c.iter().filter_map(|d| d.pr.review.decision).collect();
        assert_eq!(decisions.len(), 3, "every ReviewDecision variant");

        assert!(c.iter().any(|d| d.pr.author.is_none()), "a ghost author");
        assert!(
            c.iter()
                .any(|d| d.pr.author.as_ref().is_some_and(|a| a.is_bot)),
            "a bot author"
        );
        assert!(
            c.iter().any(|d| d.pr.title.len() > 130),
            "a very long title"
        );
        assert!(
            c.iter().any(|d| d.pr.repo.to_string().len() > 50),
            "a very long repo name"
        );
        assert!(c.iter().any(|d| d.pr.labels.is_empty()), "no labels");
        assert!(c.iter().any(|d| d.pr.labels.len() >= 6), "many labels");
        assert!(c.iter().any(|d| d.body.is_empty()), "an empty body");
        assert!(
            c.iter().any(|d| d.body.source.contains("```")),
            "a body with a code block"
        );
        assert!(c.iter().any(|d| d.timeline.is_empty()), "an empty timeline");
        assert!(
            c.iter()
                .any(|d| d.pr.review.i_am_requested && !d.pr.review.awaits_me()),
            "requested from me but already answered"
        );
    }

    #[test]
    fn detail_rows_carry_their_check_runs() {
        // `CheckRollup.runs` is "empty in list contexts; populated in detail".
        // The corpus is built as details, so any PR with CI carries its runs
        // and the list read is the one that strips them.
        let c = pr_corpus();
        for d in c.iter().filter(|d| d.pr.checks.state != RollupState::None) {
            assert!(
                !d.pr.checks.runs.is_empty(),
                "{} has CI but no runs",
                d.pr.number
            );
        }
    }

    #[test]
    fn the_noisy_timeline_gives_fold_something_to_do() {
        let c = pr_corpus();
        let big = c.iter().max_by_key(|d| d.timeline.len()).expect("a corpus");
        assert!(big.timeline.len() >= 15, "long enough to be a real one");
        let folded = fold(&big.timeline, 3);
        assert!(
            folded
                .iter()
                .any(|e| matches!(e, TimelineEntry::Folded { .. })),
            "at least one run of noise collapses"
        );
        assert!(
            big.timeline
                .iter()
                .any(|e| e.kind.unmodelled_kind().is_some()),
            "an unmodelled event, so `Other` renders somewhere"
        );
    }

    #[test]
    fn timelines_are_oldest_first() {
        for d in pr_corpus() {
            let times: Vec<_> = d.timeline.iter().map(|e| e.at).collect();
            assert!(
                times.windows(2).all(|w| w[0] <= w[1]),
                "#{} is not in order",
                d.pr.number
            );
        }
    }

    #[test]
    fn coordinates_are_unique() {
        let c = pr_corpus();
        let refs: HashSet<_> = c.iter().map(|d| d.pr.subject_ref().to_string()).collect();
        assert_eq!(refs.len(), c.len());
        let ids: HashSet<_> = c.iter().map(|d| d.pr.node_id.clone()).collect();
        assert_eq!(ids.len(), c.len());
    }
}

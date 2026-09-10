#!/usr/bin/env python3
"""Build the omaghy mockup fixture.

Curated, not random: every row exists to exercise a specific part of the
design space. Real titles come from ShaxP/shax; the rest is synthesized to
cover reasons, subject types, ages, and edge cases the real inbox lacks.

Regenerate:  python3 fixtures/build-fixture.py
"""
import json, datetime, pathlib

NOW = datetime.datetime(2026, 9, 10, 11, 0, 0, tzinfo=datetime.timezone.utc)

AV = {
    "shaxp":  "https://avatars.githubusercontent.com/u/19811678?v=4",
    "octocat":"https://avatars.githubusercontent.com/u/583231?v=4",
    "rust":   "https://avatars.githubusercontent.com/u/5430905?v=4",
    "github": "https://avatars.githubusercontent.com/u/9919?v=4",
    None:     None,
}

R = {  # repo shorthand -> (full_name, private, owner_avatar)
    "shax":    ("ShaxP/shax", False, AV["shaxp"]),
    "omaghy":  ("ShaxP/omaghy", True, AV["shaxp"]),
    "clip":    ("ShaxP/clipboard-sharing-mac-omarchy", False, AV["shaxp"]),
    "qs":      ("quickshell/quickshell", False, AV["octocat"]),
    "rust":    ("rust-lang/rust", False, AV["rust"]),
    "omarchy": ("basecamp/omarchy", False, AV["github"]),
    "long":    ("some-very-long-organization-name/an-equally-long-repository-name", False, AV["octocat"]),
}

def ago(**kw): return (NOW - datetime.timedelta(**kw)).strftime("%Y-%m-%dT%H:%M:%SZ")

def row(i, unread, reason, repo, typ, title, when, enriched=None):
    full, private, avatar = R[repo]
    return {
        "id": str(24555446000 + i),
        "unread": unread,
        "reason": reason,
        "updatedAt": when,
        "subject": {"title": title, "type": typ},
        "repo": {"fullName": full, "private": private, "ownerAvatar": avatar},
        "enriched": enriched,
    }

def enr(number=None, state=None, draft=False, actor=None, checks=None):
    return {"number": number, "state": state, "isDraft": draft,
            "actor": actor, "actorAvatar": AV.get(actor), "checks": checks}

ROWS = [
    # --- fresh + unread: the rows that matter most, top of list ------------
    row(1, True, "review_requested", "qs", "PullRequest",
        "Add SocketServer reconnect backoff and idle timeout", ago(minutes=4),
        enr(1842, "open", False, "octocat", "pending")),
    row(2, True, "mention", "omarchy", "Issue",
        "Bar widget plugins should be able to declare a preferred section", ago(minutes=22),
        enr(903, "open", False, "github", None)),
    row(3, True, "ci_activity", "shax", "CheckSuite",
        "CI failed on main", ago(minutes=41),
        enr(None, None, False, None, "failing")),
    row(4, True, "author", "shax", "PullRequest",
        "fix: syntax highlighting follows the Dark/Light/System toggle", ago(hours=2),
        enr(61, "merged", False, "shaxp", "passing")),
    row(5, True, "assign", "omaghy", "Issue",
        "Notifications inbox: decide enrichment strategy", ago(hours=3),
        enr(7, "open", False, "shaxp", None)),

    # --- awaiting enrichment: enriched == null (two-phase paint) ----------
    row(6, True, "comment", "rust", "PullRequest",
        "Stabilize `let_chains` in the 2024 edition", ago(hours=5)),
    row(7, True, "team_mention", "long", "Discussion",
        "RFC: unifying the plugin manifest across shell surfaces", ago(hours=6)),

    # --- edge: very long title (140 chars) --------------------------------
    row(8, True, "subscribed", "qs", "PullRequest",
        "Refactor the Wayland layer-shell surface lifecycle so that anchors, "
        "exclusive zones, and keyboard focus are reconciled in a single pass",
        ago(hours=8), enr(1799, "open", True, "octocat", "pending")),

    # --- security + release: rare but visually distinct --------------------
    row(9, True, "security_alert", "omaghy", "RepositoryVulnerabilityAlert",
        "Moderate severity vulnerability in openssl 0.10.66", ago(hours=11),
        enr(None, "open", False, None, None)),
    row(10, False, "subscribed", "rust", "Release",
        "Rust 1.94.0", ago(days=1),
        enr(None, None, False, "rust", None)),

    # --- state variety: closed, merged, draft ----------------------------
    row(11, False, "author", "shax", "PullRequest",
        "M7 slice 1: light theme + Dark/Light/System toggle", ago(days=1, hours=4),
        enr(60, "merged", False, "shaxp", "passing")),
    row(12, False, "author", "shax", "PullRequest",
        "fix: tighter markdown spacing in chat bubbles", ago(days=2),
        enr(59, "closed", False, "shaxp", "failing")),
    row(13, False, "author", "shax", "PullRequest",
        "Ollama per-model tool + vision probing (closes M6)", ago(days=2, hours=6),
        enr(58, "merged", False, "shaxp", "passing")),
    row(14, False, "author", "shax", "PullRequest",
        "fix: block-focus works while the assistant panel is open", ago(days=3),
        enr(57, "open", True, "shaxp", "pending")),
    row(15, False, "state_change", "omarchy", "Issue",
        "Theme switcher should preview before applying", ago(days=3, hours=9),
        enr(871, "closed", False, "github", None)),

    # --- private repo -----------------------------------------------------
    row(16, False, "author", "omaghy", "PullRequest",
        "spec: notifications inbox domain model", ago(days=4),
        enr(3, "open", False, "shaxp", "passing")),

    # --- the real corpus, aged out ---------------------------------------
    row(17, False, "author", "shax", "PullRequest",
        "Tool integration: run_command via safety gate (M6 loop close)", ago(days=5)),
    row(18, False, "author", "shax", "PullRequest",
        "M6 slice 4: assistant chat overlay + explain-on-error + capability gating", ago(days=6),
        enr(55, "merged", False, "shaxp", "passing")),
    row(19, False, "author", "shax", "PullRequest",
        "M6 slice 3: Ollama provider (local, capability-probed)", ago(days=8),
        enr(54, "merged", False, "shaxp", "passing")),
    row(20, False, "author", "shax", "PullRequest",
        "M6 slice 2b: Claude subscription lane (local CLI subprocess)", ago(days=9),
        enr(53, "merged", False, "shaxp", "passing")),
    row(21, False, "author", "shax", "PullRequest",
        "M6 slice 2a: Claude provider — API key lane (Rust proxy)", ago(days=11),
        enr(52, "merged", False, "shaxp", "passing")),
    row(22, False, "author", "shax", "PullRequest",
        "M6 slice 1: safety gate + AssistantProvider interface", ago(days=13),
        enr(51, "merged", False, "shaxp", "passing")),
    row(23, False, "author", "shax", "PullRequest",
        "docs: pluggable AssistantProvider model for M6", ago(days=16),
        enr(50, "merged", False, "shaxp", None)),

    # --- long repo name + commit subject ---------------------------------
    row(24, False, "comment", "clip", "Commit",
        "Handle wl-paste MIME negotiation on macOS bridge", ago(days=21),
        enr(None, None, False, "octocat", None)),
    row(25, False, "manual", "long", "Issue",
        "Tracking: plugin manifest schemaVersion 2", ago(days=30),
        enr(412, "open", False, "octocat", None)),

    # --- deep history: age formatting at the extremes ---------------------
    row(26, False, "author", "shax", "PullRequest",
        "M5 slice 3: ls widget (closes M5)", ago(days=62),
        enr(49, "merged", False, "shaxp", "passing")),
    row(27, False, "author", "shax", "PullRequest",
        "M5 slice 2 follow-up: silent-reads model + sticky-bottom live widget", ago(days=118),
        enr(48, "merged", False, "shaxp", "passing")),
    row(28, False, "invitation", "omarchy", "Issue",
        "You were invited to collaborate", ago(days=400),
        enr(None, None, False, "github", None)),

    # --- edge: no avatar available ---------------------------------------
    row(29, True, "comment", "long", "Issue",
        "Short one", ago(minutes=9),
        enr(9001, "open", False, None, None)),
]

out = {
    "generatedAt": NOW.strftime("%Y-%m-%dT%H:%M:%SZ"),
    "note": "Curated design fixture. Real titles from ShaxP/shax; rest synthesized.",
    "rows": ROWS,
}
p = pathlib.Path(__file__).parent / "notifications.json"
p.write_text(json.dumps(out, indent=2) + "\n")
print(f"wrote {p} — {len(ROWS)} rows, {sum(1 for r in ROWS if r['unread'])} unread, "
      f"{sum(1 for r in ROWS if r['enriched'] is None)} awaiting enrichment")

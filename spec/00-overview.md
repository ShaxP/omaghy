# omaghy — overview

**omaghy is a full GitHub client as a terminal UI, written in Rust.**

It is not a notifications app, and not a desktop overlay. Both were explored
and rejected; §8 records why, so they are not re-proposed.

---

## 1. What it is for

Working with GitHub without leaving the terminal: triaging what needs your
attention, reading and acting on pull requests and issues, watching CI, and
finding things. It targets Omarchy first — theming, keybindings, and packaging
assume it — but it is a normal terminal program and must run anywhere with a
truecolor terminal and a `gh` token.

### Non-goals

- **Not a git client.** Local branches, staging, rebases, and merges belong to
  `git`, `lazygit`, and your editor. omaghy is the *remote* side.
- **Not a browser replacement.** Where GitHub's web UI is genuinely better —
  rich diffs on huge PRs, project boards, settings — omaghy's job is to get you
  there fast (`o` opens the current thing), not to reimplement it.
- **Not multi-forge.** GitHub only. No GitLab/Gitea abstraction layer; it would
  tax every type in §2 of `10-domain-model.md` for a user who does not exist.
- **Not a notification daemon** in v1. `omaghy watch` is planned (§6) but is a
  second mode of the same binary, not the product.

---

## 2. Surfaces

Seven, plus two cross-cutting.

| Surface | Contents |
|---|---|
| **Dashboard** | Configurable sections: my open PRs, review requests, assigned issues, recent mentions, failing CI. The landing screen. |
| **Notifications** | Inbox triage — read/unread, mute, open, bulk-mark. |
| **Pull requests** | List (repo- or query-scoped) → detail: conversation, files, checks. Then review actions. |
| **Issues** | List → detail: conversation, labels, assignees. Then comment/close. |
| **Actions** | Workflow runs, jobs, logs, re-run/cancel. |
| **Repositories** | Browse, README, metadata, clone/checkout handoff. |
| **Search** | GitHub search syntax across issues, PRs, repos, code. |
| *Command palette* | Cross-cutting. Every action reachable by name. |
| *Help* | Cross-cutting. Context-sensitive keymap. |

**Surfaces are registered, not hardcoded.** The router holds a registry; adding
one must not require editing the others. This is a structural requirement, not
a style preference — it is what keeps "which surface ships first" a scheduling
question instead of a product definition.

---

## 3. Milestones

| | Contents | Proves |
|---|---|---|
| **M1** | Skeleton, auth, cache, router, palette, help · Dashboard · Notifications | The whole vertical stack: token → GraphQL → SQLite → render → act |
| **M2** | PRs and Issues, read-only: list, detail, conversation, checks | The heaviest read models; timeline union flattening |
| **M3** | Write actions: review (approve/comment/request changes), merge, comment, label, close, mark-read | Mutations, optimistic update, conflict handling |
| **M4** | Actions, Repositories, Search · images (progressive) · `omaghy watch` | Breadth, log streaming, terminal graphics |

M1 is deliberately the widest slice vertically and the narrowest horizontally:
two surfaces, but every layer of the stack. Nothing later is architecture, only
more of it.

---

## 4. Architecture

A **single binary**. No daemon, no IPC, no wire protocol.

```
crates/
  omaghy-model    domain types + serde. depends on nothing.
  omaghy-store    the Store trait + FakeStore. the seam omaghy-tui sees.
  omaghy-api      reqwest · GraphQL · REST · ETag · retry · rate governor
  omaghy-cache    rusqlite — entities, etags, cursors, TTL
  omaghy-sync     scheduler, refresh policy, delta computation
  omaghy-tui      ratatui — router, surfaces, widgets, keymap, theme
  omaghy          the binary: arg parsing, wiring, `tui` / `watch` / `doctor`
```

Dependencies point strictly downward; `omaghy-model` is a leaf. `omaghy-tui`
never performs I/O — it consumes the `Store` trait (§5).

`omaghy-store` was added in P0.2. The trait needs a home that is neither the
vocabulary nor an implementation: `omaghy-model` must stay a leaf, and it would
otherwise need `async-trait` and `tokio`; putting the trait in `omaghy-cache`
would make the UI depend on a storage backend.

### 4.1 Reads are GraphQL, writes and oddities are REST

A PR list with review state, CI status, and labels is *one* GraphQL query
instead of N+3 REST round-trips. Measured against the live API, a combined
dashboard query (viewer + two searches + rate limit) costs **1 point of 5000**
and ~700ms. REST is retained for raw diffs (`Accept: vnd.github.v3.diff`),
Actions job logs, and mutations GraphQL does not expose.

**Queries are hand-written and deserialized with `serde_json`**, not generated.
An earlier draft specified `cynic`, whose compile-time query validation is
genuinely valuable — but it requires committing GitHub's ~5 MB schema and a
codegen step, and M1 has under a dozen queries. The translation boundary is
already mandatory (`10-domain-model.md` §1), so adopting `cynic` later is a
change contained entirely within `omaghy-api`.

### 4.2 Latency is the design problem, not throughput

~700ms per round trip dominates everything; the language choice is noise
against it. Therefore: **every read is answered from cache first and refreshed
in the background.** A surface must paint real content on its first frame, from
SQLite, offline if need be — then update in place. Spinners on open are a bug.

### 4.3 The `Store` seam

`omaghy-tui` sees one trait. Cache-first reads, background refresh, and change
notification live behind it. This is what lets UI work proceed against a fixture
store while API work proceeds against recorded fixtures, and it is where a
daemon would reattach if one is ever wanted (§8).

---

## 5. Cross-cutting decisions

**Auth.** Resolution order: `OMAGHY_TOKEN` → `GH_TOKEN` → `gh auth token`
(43ms, reads gh's keyring) → actionable error naming `gh auth login`. We never
persist a token ourselves and never write to gh's config. No OAuth app in v1.

**Terminal capability, not terminal choice.** Truecolor is assumed. Everything
beyond it is detected at runtime and degrades: images go kitty-protocol →
sixel → half-block `▀` → coloured initials; hyperlinks use OSC 8 when present;
clipboard uses OSC 52 with a shell fallback. **The app must never require a
specific terminal.** Omarchy's default is foot (sixel, no kitty protocol).

**Theming.** The terminal is already themed by Omarchy, so the 16 ANSI colours
and default fg/bg follow the active theme for free — that is the baseline and
it must remain sufficient. An optional `~/.config/omarchy/themed/omaghy.toml.tpl`
adds semantic roles (accent, urgent, muted, added/removed) for themes that want
them, re-rendered on `omarchy theme set`. **Never require the template**; a
design that is illegible on 16 colours is a broken design.

**Config.** `~/.config/omaghy/config.toml` — dashboard sections, default repo
scope, keymap overrides, refresh intervals. Absent config must produce a
working app.

**Keymap.** Vim-shaped by default: `j`/`k`, `g`/`G`, `/` search, `:` palette,
`?` help, `q` back, `o` open in browser. Every binding overridable; every
action nameable in the palette, so nothing is keyboard-only trivia.

---

## 6. Omarchy integration

Kept deliberately thin, and none of it load-bearing:

- **Terminal theming** — free, see above.
- **Hyprland scratchpad** — a shipped snippet binding a floating terminal
  running omaghy to a special workspace, for quake-style summoning.
- **`omaghy watch`** (M4) — a second mode sharing the SQLite cache, firing
  `omarchy-notification-send` on new review requests and failing CI. No IPC:
  two processes, one database.
- **Bar widget** (M4, optional) — an omarchy-shell plugin reading
  `omaghy status --json`. The one piece that is QML, and it is ~50 lines.

---

## 7. Testing, and why it shapes the plan

This project is built by parallel agents, so verification is a first-class
constraint rather than hygiene.

- **No test touches the network.** `omaghy-api` is tested against recorded
  fixtures. The notification corpus lives in `omaghy-store::fake::corpus()`,
  in Rust rather than as JSON, so it cannot drift from the model types or fail
  to parse at runtime. HTTP cassettes for `omaghy-api` are recorded in W1.1,
  when there are requests to record responses for.
- **Screens are snapshot-tested.** ratatui's `TestBackend` renders to an
  inspectable text buffer, so a surface's output is asserted like any value.
  Every surface ships snapshots for: populated, empty, filtered, loading-from-
  cache, offline-stale, and error.
- **The model crate is exhaustive.** Every GitHub union we flatten (§2 of
  `10-domain-model.md`) is a Rust enum, so adding a variant makes the compiler
  enumerate every site that must handle it.
- **Green means `cargo build && cargo clippy && cargo test` across the
  workspace.** An agent leaves it green or the work is not done.

---

## 8. Decisions already made

Recorded so they are not relitigated. Each was explored in depth.

**Rust, not Go.** GitHub's GraphQL schema is unusually polymorphic — a PR
timeline is a union of ~40 event types. Rust enums model that exhaustively;
Go's `interface{}` plus type switches never stops being ugly. Go's advantage
was `cli/go-gh` for auth, which evaporated once `gh auth token` proved to be a
43ms shell-out available from any language.

**A TUI, not a Quickshell/QML overlay.** Explored thoroughly — including a
working prototype run against real GitHub data and all 22 installed Omarchy
themes — and dropped. That prototype is gone, but several of its findings were
medium-independent and are carried into `30-ui.md` §7.1 and §9, marked there as
measured rather than reasoned. The
decisive constraint: a summoned overlay suits glance-and-dismiss triage, but a
full client involves sustained work, and `Esc` being one keystroke from
discarding a half-written review is the wrong shape. Secondary: QML has no
compiler and weak tests, and a six-surface frontend there would have been
~2,000 lines verifiable only by clicking.

**One binary, not core + frontend.** The daemon and JSONL socket protocol
existed solely because the frontend was a separate process. With the TUI
in-process they buy nothing. The `Store` trait preserves the seam if a daemon
is ever wanted.

**A client, not a notifications app.** Notifications is the first surface
built, not what omaghy is. Build order must never become product definition.

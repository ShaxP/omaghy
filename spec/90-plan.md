# omaghy — build plan

How omaghy gets built, and specifically how it gets built by several agents at
once without them destroying each other's work.

---

## 1. The binding constraint is review, not agents

Every PR carries a smoke test checklist that a human runs (`CONTRIBUTING.md`).
That is deliberate — it is how progress stays visible and how "the diff looks
fine" stops being the whole of review. It also means **throughput is bounded by
review capacity, not by how many agents can be launched.**

Two consequences, and they shape everything below:

- **Three to four concurrent agents, not ten.** More produces a queue of
  unreviewed PRs, which is indistinguishable from no progress and considerably
  more confusing.
- **Work units are sized to a reviewable PR** — roughly one that can be
  verified in ten minutes. A unit that cannot be smoke-tested in isolation is
  the wrong size, or the wrong shape.

---

## 2. Phase 0 — contracts, single-threaded

Nothing fans out until these land. Each is a PR in its own right, in order,
because everything downstream depends on all of them.

| | Lands | Why it must be first |
|---|---|---|
| **P0.1** | `omaghy-model`: every type in `10-domain-model.md`, plus the error taxonomy. Declares `serde`, `time`, `thiserror` — the three it cannot be written without. | The vocabulary. Every crate speaks it; two agents inventing it separately is the most expensive possible failure. |
| **P0.2** | **Every remaining M1 dependency**, declared in `[workspace.dependencies]` | See §2.1 — this is not optional bookkeeping. |
| **P0.3** | `Store` trait, `FakeStore`, the notification corpus | The seam. UI agents build against `FakeStore` while API agents build against cassettes; they meet here. **HTTP cassettes move to W1.1** — responses cannot be recorded for requests that do not exist yet. |
| **P0.4** | App shell: terminal setup, panic hook, event loop, router, `Surface` trait, **registry pre-wired with stub surfaces** | See §2.2. |

### 2.1 Declare every dependency up front

`Cargo.lock` is committed, so **two agents adding dependencies concurrently
conflict in `Cargo.lock` every single time.** It is a generated file: the
conflicts are ugly, and resolving them by hand is how a lockfile silently
acquires versions nobody tested.

So P0.2 declares the entire M1 dependency set in `[workspace.dependencies]` and
regenerates the lockfile once. Agents then write `reqwest.workspace = true` in
their own crate's manifest — a one-line, non-conflicting change to a file only
they own.

An agent needing a dependency P0.2 did not anticipate files a contract-change
request (§5). It does not add one.

### 2.2 Pre-wire the registry

Surfaces register (`30-ui.md` §3), and registration happens in one file. If
every surface agent must edit it, that file conflicts on every merge — the one
guaranteed collision in an otherwise disjoint design.

P0.4 therefore registers **all seven surfaces immediately**, each wired to a
stub that renders "not implemented". An agent implementing a surface replaces
its own stub module and touches nothing shared. The registry is written once
and never again.

---

## 3. Ownership

**One agent owns a path.** Not a file, not a crate — a path.

```
crates/omaghy-model/          ← contracts only; changed in Phase 0 or by request
crates/omaghy-store/          ← contracts only; likewise
crates/omaghy-api/            ← one owner
crates/omaghy-cache/          ← one owner
crates/omaghy-sync/           ← one owner
crates/omaghy-tui/src/widgets/    ← one owner
crates/omaghy-tui/src/surfaces/<name>.rs   ← one owner per surface
```

`omaghy-tui` is too big for a single owner once it holds seven surfaces, so
ownership there is **module-level**. Surfaces are genuinely independent: a
surface never renders or imports another (`30-ui.md` §3).

**An agent edits only its own path.** Not "tries to" — an agent that needs a
change elsewhere stops and files a request (§5). A cross-crate edit arriving
from two directions turns the workspace red for everyone simultaneously, and
the resulting bisect is miserable.

---

## 4. Waves

Within a milestone, work proceeds in waves. A wave's units are mutually
independent; the next wave starts when the previous merges.

### M1 — dashboard and notifications

```
Phase 0  ──►  Wave 1  ──►  Wave 2  ──►  Integration
```

**Wave 1** — three agents, no shared paths:

| | Owns | Delivers |
|---|---|---|
| W1.1 | `omaghy-api` | Auth chain, HTTP client, rate-limit governor, conditional requests |
| W1.2 | `omaghy-cache` | Schema, entity + list storage, validators, viewer keying |
| W1.3 | `omaghy-tui/src/widgets/` | List, header, footer, palette, help, toast, state-matrix renderers (`30-ui.md` §8) |

**Wave 2** — three agents:

| | Owns | Delivers |
|---|---|---|
| W2.1 | `omaghy-api` | Notifications fetch, `SubjectRef` parsing, batched enrichment |
| W2.2 | `omaghy-tui/src/surfaces/dashboard.rs` | Dashboard, configurable sections |
| W2.3 | `omaghy-tui/src/surfaces/notifications.rs` | Inbox, triage, filter, grouping |

W2.1 follows W1.1 because they share a crate. W2.2 and W2.3 follow W1.3 because
they consume its widgets — and both build against `FakeStore`, so neither waits
on the API at all.

**Integration** is single-threaded: replace `FakeStore` with `SqliteStore`, wire
`omaghy-sync`, and run against the real API for the first time. This is where
the specs get corrected, and it is deliberately not parallel.

Later milestones follow the same shape: surfaces are independent, so M2's PRs
and Issues surfaces parallelize cleanly; M3's write actions do not, because they
all touch mutation paths in `omaghy-api`.

---

## 5. Contract changes

Implementation will prove parts of `10-domain-model.md` and `20-store.md`
wrong. That is expected — those specs have never been compiled.

When an agent needs a change outside its path:

1. **Stop.** Do not edit it.
2. File a contract-change request: what is needed, why the current shape fails,
   the smallest change that resolves it.
3. The change lands as its **own PR**, by the owner, updating the spec and the
   code together.
4. Dependent work rebases onto it.

This is slower than editing across boundaries and it is the point. A shared type
mutating under four agents at once produces failures none of them can reproduce.

**`omaghy-model` changes are the expensive case** — every crate depends on it,
and `CONTRIBUTING.md`'s strict status check means every open PR must rebase.
Batch them where possible.

---

## 6. What an agent is given

Each work unit's brief carries, explicitly:

- **The path it owns**, and the instruction to touch nothing else
- **The spec sections** it implements — the spec is normative, not advisory
- **Definition of done** (§7)
- **What exists already**: `FakeStore`, fixtures, widgets, model types
- **What it must not do**: add dependencies, edit shared paths, change
  `omaghy-model`, weaken a lint, or mark a test `#[ignore]` to get green

That last one matters. An agent optimising for a passing check will delete the
failing test if nothing says otherwise.

---

## 7. Definition of done

A work unit is complete when **all** of these hold:

- `cargo build --workspace --locked` — clean
- `cargo clippy --workspace --all-targets` — clean under `-D warnings`
- `cargo fmt --all --check` — clean
- `cargo test --workspace` — passes, **no test ignored or deleted to achieve it**
- **No test opens a socket.** Fixtures only (`00-overview.md` §7)
- Every applicable row of the §8 state matrix is snapshot-tested, for surfaces
- The PR carries a smoke test checklist with an honest "Not covered"
- Only owned paths changed — verifiable by reading `git diff --stat`
- Where the spec turned out wrong, **the spec is updated in the same PR**

"It compiles" is not done. "Tests pass" is not done if the tests were the thing
that got adjusted.

---

## 8. Failure modes this plan exists to prevent

Named, because each has a specific mitigation and generic care would not
have produced any of them:

| Failure | Mitigation |
|---|---|
| Two agents inventing the same domain type differently | P0.1 lands the vocabulary first |
| `Cargo.lock` conflicts on every concurrent PR | P0.2 declares all dependencies up front |
| The surface registry conflicting on every merge | P0.4 pre-wires all seven |
| A shared type mutating under several agents | §5: contract changes are their own PR |
| Green achieved by deleting the failing test | §7 forbids it; §6 says so explicitly |
| Network-dependent tests failing on Tuesdays | Fixtures only; no test opens a socket |
| A queue of unreviewed PRs | §1: three to four agents, sized to ten-minute review |
| Specs quietly diverging from code | §7: the spec changes in the PR that disproves it |

---

## 9. Sequencing the whole thing

```
Phase 0   P0.1 model · P0.2 deps · P0.3 store · P0.4 shell     sequential
M1        Wave 1 (3) → Wave 2 (3) → integration                 parallel
M2        PRs + Issues, read-only                               parallel
M3        Write actions                                         mostly sequential
M4        Actions · Repos · Search · images · watch · bar widget parallel
```

Phase 0 is the whole bet. If the contracts are right, the waves are genuinely
independent and parallelism is real. If they are wrong, every agent discovers it
at once — which is the argument for keeping Phase 0 single-threaded and
unhurried, even though it is the least exciting part.

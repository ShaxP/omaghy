# Contributing to omaghy

Conventions for humans and agents alike. Everything here exists because it was
decided deliberately; if a rule is wrong, change it in a PR rather than routing
around it.

## Branches and PRs

`main` is protected: no direct pushes, no force-pushes, no deletion. Every
change arrives by pull request, including the first — `main` was bootstrapped
by GitHub's own initial commit so that no exception was ever needed.

Branch naming: `spec/…`, `feat/…`, `fix/…`, `chore/…`.

### Merge strategy is per-PR, deliberately

Both squash and merge commits are enabled. They are not interchangeable:

| Use | When |
|---|---|
| **Merge commit** | Curated branches whose individual commits are worth keeping — stacked spec PRs, anything where the sequence of work is the point. Also keeps stacked PRs working, since the parent's commits stay in `main`'s history. |
| **Squash** | Branches with throwaway intermediate commits — most agent work. Launders "wip" and "fix clippy" at the boundary instead of propagating them into `main` forever. |

The deciding question is whether the branch's commits are a record worth
reading. If in doubt, squash.

> Squashing does not destroy history — the individual commits remain on the PR
> page and at `refs/pull/N/head` permanently. What squashing removes is their
> presence in `git log main`.

### Stacking

PRs may target another PR's branch. When the parent merges **as a squash**, the
child is orphaned and must be re-parented:

```bash
git fetch origin
git rebase --onto origin/main <old-parent-sha> <your-branch>
git push --force-with-lease
```

Merging the parent with a merge commit avoids this entirely — which is most of
why merge commits are allowed.

## Every PR carries a smoke test

**If a PR changes anything a user can observe, it must include a smoke test
checklist** in its description: the commands to run and what should happen,
written so the change can be verified without reading the diff.

This is not ceremony. It is how the author states what they actually verified,
and it keeps review from collapsing into "the diff looks fine."

### Test behaviour, not the build

A smoke test asks *does this work*, never *does this compile*.

**Never put these in a checklist.** CI runs them on every PR as a required
check, and repeating them wastes the reviewer's time while looking like
diligence:

- `cargo build` · `cargo test` · `cargo clippy` · `cargo fmt --check`
- `grep`-ing `Cargo.lock`, `cargo tree`, or any other build-system introspection
- "the CI check is green" — visible on the PR already

**Put these in instead** — things only a human running the program can see:

- Launch it. Do the thing the PR is about. Describe what should appear.
- The unhappy paths: empty, offline, stale, forbidden, rate-limited, malformed
  input, a terminal too small, a missing token.
- Anything where the *feel* is the point — latency, flicker, whether the cursor
  lands where you left it.
- Side effects: a file written, a cache row created, a notification fired.

### "No smoke test" is a valid answer

Scaffolding, contracts, dependency wiring, and docs change nothing a user can
observe. **Say so plainly and move on:**

> **Smoke test:** none. This PR adds no observable behaviour — it declares
> dependencies. CI covers that the workspace still builds and its tests pass.

Inventing checklist items for such a PR is worse than omitting them: it trains
the reviewer to tick boxes without reading, which is exactly what the
convention exists to prevent.

### Shape

A good checklist:

- gives **exact commands**, copy-pasteable, no "and then poke around"
- states the **expected result** for each, specifically enough to be wrong
- covers the **unhappy paths**, not just the demo
- ends with **"Not covered"**, naming honestly what it does not prove

Spec- and docs-only PRs use a **Review guide** instead: where to look and which
decision to check. Point at what is worth arguing with, not at the whole diff.

## Dependencies

`PREREQUISITES.md` is the build-and-run contract. **Any PR adding a dependency
with a system requirement updates it in the same PR** — a prerequisites page
that is wrong at build time is worse than none, because it was trusted.

`Cargo.lock` is **committed**. omaghy ships a binary, so the lockfile is what
makes a local build, CI, and a packager's build resolve identical dependency
versions. Do not add it to `.gitignore` — that advice applies to libraries.

Two policies in `PREREQUISITES.md` §5 are load-bearing and must not be broken
casually: **no OpenSSL** (`rustls` with the `ring` provider, system root certs)
and **bundled SQLite**. Both keep the build free of system libraries; taking a
crate default silently reverses either one.

## Crate ownership

Dependencies point strictly downward; `omaghy-model` depends on nothing.

```
omaghy-model ← omaghy-api ← omaghy-sync ← omaghy
     ↑              ↑                        ↑
omaghy-cache ───────┘        omaghy-tui ─────┘
```

During parallel work, **one agent owns a crate.** An agent that needs a change
in a crate it does not own files a contract-change request rather than editing
it — a cross-crate edit landing from two directions is how a green workspace
turns red for everyone at once.

`omaghy-tui` performs no I/O. It consumes the `Store` trait. If a surface needs
data, that is a `Store` change, not an HTTP call.

## Specs are the contract

`spec/` is normative; code follows it. When implementation reveals a spec is
wrong — which it will — change the spec in the same PR and say so in the
description. A spec that silently drifts from the code is worse than no spec.

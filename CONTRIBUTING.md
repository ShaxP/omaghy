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

**If a PR changes anything observable, it must include a smoke test checklist**
in its description: the commands to run and what should happen, written so the
change can be verified without reading the diff.

This is not ceremony. It is how the author states what they actually verified,
and it keeps review from collapsing into "the diff looks fine."

A good checklist:

- gives **exact commands**, copy-pasteable, no "and then poke around"
- states the **expected result** for each, specifically enough to be wrong
- covers the **unhappy paths** — empty, offline, stale, error — not just the demo
- ends with **"Not covered"**, naming honestly what it does not prove

Spec- and docs-only PRs have nothing to run. They use a **Review guide**
instead: where to look and which decision to check. Point at what is worth
arguing with, not at the whole diff.

## CI

`build · clippy · test` is a required check and runs `cargo fmt --check`,
`cargo clippy --all-targets`, `cargo test`, and `cargo build` — all with
`-D warnings`. Branches must be up to date with `main` before merging.

That last rule is strict on purpose: `omaghy-model` is a shared dependency, and
a change there breaking another crate is precisely the parallel-work failure
mode worth catching at merge time.

Run it locally before pushing. An agent that cannot run `cargo test` burns CI
cycles discovering typos.

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

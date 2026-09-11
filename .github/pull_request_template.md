## What this changes

<!-- One paragraph. What moved, and why now. -->

## Smoke test

<!--
Required whenever this PR changes anything a USER CAN OBSERVE.

Test behaviour, not the build. Do NOT list cargo build / test / clippy / fmt,
Cargo.lock greps, or "CI is green" — CI runs those on every PR already, and
repeating them wastes review time while looking like diligence.

Do list: launch it, do the thing, what should appear. The unhappy paths —
empty, offline, stale, forbidden, malformed input, terminal too small. Side
effects. Anything where the feel is the point.

If this PR changes nothing observable — scaffolding, contracts, deps, docs —
delete the checkboxes and write plainly:

    none. This PR adds no observable behaviour; it <does X>. CI covers that
    the workspace still builds and its tests pass.

Inventing items for such a PR is worse than omitting them.
-->

```bash
gh pr checkout <n>    # the reviewer starts on main, not on your branch
```

- [ ] …

**Not covered:** <!-- what this checklist does NOT prove. Be honest. -->

## Review guide

<!--
For spec/docs PRs, or alongside the smoke test: where to look and what
decision to check. Point at the parts worth arguing with, not the whole diff.
-->

## Risks

<!-- What could this break? What did you not test? Say "none known" if so. -->

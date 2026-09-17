# omaghy

A full GitHub client for the terminal, built for [Omarchy](https://omarchy.org).
Rust, [ratatui](https://ratatui.rs).

> **Status: M1 is built and talks to GitHub.** The dashboard and the
> notifications inbox work against the real API, cache-first, with a poll
> loop, a settings panel, a command palette, and `o` to open things in a
> browser. Pull requests can be listed and read (M2, in progress); issues are
> next; the rest is stubbed. The milestones are in
> [`spec/00-overview.md`](spec/00-overview.md) §3.

## What it is

Working with GitHub without leaving the terminal: triaging what needs your
attention, reading and acting on pull requests and issues, watching CI, and
finding things.

| Surface | | |
|---|---|---|
| Dashboard | My PRs, review requests, assigned issues, mentions — configurable sections | built |
| Notifications | Inbox triage | built |
| Pull requests | List → detail (conversation, checks); review actions are M3 | built, read-only |
| Issues | List → detail → comment, label, close | M2 |
| Actions | Runs, jobs, logs | M4 |
| Repositories | Browse, README, clone handoff | M4 |
| Search | GitHub search syntax | M4 |

Cross-cutting, and already there: `,` settings, `:` command palette, `?` help.

## What it is not

- **Not a git client.** Local branches, staging, and rebases belong to `git`
  and `lazygit`. omaghy is the remote side.
- **Not a browser replacement.** Where the web UI is genuinely better, `o`
  takes you there.
- **Not multi-forge.** GitHub only.

## Design

`spec/` is normative; code follows it, and where building something disproved
a spec, the spec was corrected in the same PR and says so.

| | |
|---|---|
| [`spec/00-overview.md`](spec/00-overview.md) | Product, surfaces, milestones, architecture, decisions already made |
| [`spec/10-domain-model.md`](spec/10-domain-model.md) | The vocabulary `omaghy-model` owns |
| [`spec/20-store.md`](spec/20-store.md) | The `Store` seam: cache-first reads, freshness, mutations, the poll loop |
| [`spec/30-ui.md`](spec/30-ui.md) | Shell, router, keys, layout, the state matrix every surface renders |
| [`spec/40-config.md`](spec/40-config.md) | `config.toml`: every setting, and what is deliberately not one |
| [`spec/90-plan.md`](spec/90-plan.md) | How it is built — by several agents at once, in waves |

## Building

See [PREREQUISITES.md](PREREQUISITES.md). On Omarchy:

```bash
sudo pacman -S --needed rustup base-devel git github-cli
rustup default stable
cargo build --release
```

## Running

```bash
omaghy                       # opens `default-route` from config, else notifications
omaghy dashboard             # any route from spec/30-ui.md §3.2
omaghy --config ./alt.toml   # or OMAGHY_CONFIG
```

Vim-shaped keys: `j`/`k`, `g`/`G`, `r` refresh, `o` open in browser, `:`
palette, `,` settings, `?` help, `q` back. Every action is nameable in the
palette. Rebinding (`[keys]` in config) is specified but not built yet — the
file accepts the section and warns that it is not applied.

| | |
|---|---|
| Config | `~/.config/omaghy/config.toml` — absent is fine; unknown keys warn |
| Cache | `~/.cache/omaghy/cache.db` — SQLite, keyed by viewer, rebuilt if corrupt |
| Log | `~/.local/state/omaghy/omaghy.log` — `OMAGHY_LOG` moves it, `OMAGHY_LOG_LEVEL` filters it |

`OMAGHY_FAKE=` (empty) runs against the fixture corpus, no token or network
needed. Modes — `OMAGHY_FAKE=stale`, `cold`, `empty`, `forbidden`,
`ratelimited`, `error` and more — exist so every unhappy screen can be reached
by running the program, not only by a snapshot test.

## Auth

No OAuth app, no stored credentials. Token resolution is
`OMAGHY_TOKEN` → `GH_TOKEN` → `gh auth token`, so if you already use the
GitHub CLI there is nothing to set up.

## Terminal

Truecolor is assumed. Everything beyond it is detected and degrades — images go
kitty-protocol → sixel → half-block → coloured initials. **omaghy never
requires a particular terminal.**

## Licence

MIT

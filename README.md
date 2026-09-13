# omaghy

A full GitHub client for the terminal, built for [Omarchy](https://omarchy.org).
Rust, [ratatui](https://ratatui.rs).

> **Status: design.** No implementation yet — the specs land first. See
> [`spec/`](spec/).

## What it is

Working with GitHub without leaving the terminal: triaging what needs your
attention, reading and acting on pull requests and issues, watching CI, and
finding things.

| Surface | |
|---|---|
| Dashboard | My PRs, review requests, assigned issues, failing CI |
| Notifications | Inbox triage |
| Pull requests | List → detail → review |
| Issues | List → detail → comment, label, close |
| Actions | Runs, jobs, logs |
| Repositories | Browse, README, clone handoff |
| Search | GitHub search syntax |

## What it is not

- **Not a git client.** Local branches, staging, and rebases belong to `git`
  and `lazygit`. omaghy is the remote side.
- **Not a browser replacement.** Where the web UI is genuinely better, `o`
  takes you there.
- **Not multi-forge.** GitHub only.

## Design

| | |
|---|---|
| [`spec/00-overview.md`](spec/00-overview.md) | Product, surfaces, milestones, architecture, decisions already made |
| [`spec/10-domain-model.md`](spec/10-domain-model.md) | The vocabulary `omaghy-model` owns |
| [`spec/40-config.md`](spec/40-config.md) | `config.toml`: every setting, and what is deliberately not one |

## Building

See [PREREQUISITES.md](PREREQUISITES.md). On Omarchy:

```bash
sudo pacman -S --needed rustup base-devel git github-cli
rustup default stable
cargo build --release
```

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

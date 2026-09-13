# omaghy — configuration

`~/.config/omaghy/config.toml` (or `$XDG_CONFIG_HOME/omaghy/config.toml`).

---

## 1. Rules

**Absent config produces a working app.** Every key is optional. A user who
never writes a config file gets the defaults below, and those defaults are the
ones chosen by looking at real rows in a real terminal — see §5.

**Unknown keys warn, never fail.** A typo, or a key from a newer version, is
logged and ignored. Refusing to start because one line is unrecognised is the
wrong trade for a program someone opens to check whether CI passed.

**Invalid values warn and fall back to the default**, naming the key, the bad
value, and the accepted set. `reason = "glif"` must say so; it must not
silently render nothing.

**Config is read once, at startup.** Live reload is not a goal. `omaghy` starts
in milliseconds; restarting it is cheaper than watching a file.

---

## 2. The file

```toml
[general]
# Route opened when omaghy is run with no argument.
default-route = "notifications"        # any route from 30-ui.md §3.2

[notifications]
# How the reason for a notification is encoded.
#   glyph — an Octicon in its own column (default)
#   text  — an abbreviated word, at the cost of the kind icon
#   none  — nothing; the focused-row line still names it in full
reason = "glyph"

# How repository names are shown.
#   full              — owner/name on every row (default)
#   elide-owner       — drop the owner when it is yours
#   hide-when-shared  — drop the column when every visible row shares a repo
repo = "full"

# Row height.
#   two-line — title, then repo, reason and state beneath it (default)
#   one-line — everything on one line, columns dropping by width per §4.1
rows = "two-line"

# What happens to a row when you mark it read.
#   grey — it stays exactly where it is, dimmed (default)
#   sink — it drops below every unread row
#   hide — it leaves the list
triage = "grey"

# Row grouping.
#   by-repo — section headers per repository (default)
#   flat    — strict recency, no headers
group = "by-repo"

[dashboard]
# Sections, in order. Each is a title and GitHub search syntax.
sections = [
  { title = "Needs my review",   query = "is:open is:pr review-requested:@me", limit = 10 },
  { title = "My pull requests",  query = "is:open is:pr author:@me",           limit = 10 },
  { title = "Assigned to me",    query = "is:open assignee:@me",               limit = 10 },
  { title = "Recently mentioned", query = "is:open mentions:@me",              limit = 10 },
]

[refresh]
# Seconds. Floors apply: notifications never polls faster than GitHub's
# X-Poll-Interval, whatever this says (20-store.md §6).
notifications = 60
dashboard     = 300

[keys]
# Override any action named in the palette. The action names are stable;
# the keys are not sacred. An action may be bound to several keys.
"notification.next"         = ["j", "down"]
"notification.toggle-read"  = "enter"
"app.quit"                  = ["q", "ctrl-c"]
```

---

## 3. What is deliberately not configurable

**Icons are always Unicode.** There is no `icons = "ascii"` setting.

An ASCII fallback exists in the code and is chosen automatically when the
terminal or font cannot render the glyphs — that is capability detection, not
preference. But it is not a knob, because a user choosing ASCII on a terminal
that supports Octicons is choosing a worse rendering of the same information,
and every additional mode is another combination to keep tested.

The consequence is honest and belongs in `PREREQUISITES.md` §3: without a Nerd
Font, omaghy leans on that automatic fallback. It must therefore stay correct —
every icon role keeps an ASCII form of identical cell width, so layout never
shifts (`30-ui.md` §6).

**Colours are the terminal's.** Omarchy themes the terminal and omaghy
inherits it. There is no palette section; see `00-overview.md` §5.

---

## 4. Precedence

For any setting: **command-line flag → environment variable → config file →
default.** Only a few settings have flags or variables; the chain exists so
that adding one later does not change where anything else comes from.

`OMAGHY_TOKEN` and `GH_TOKEN` sit in this chain at the environment step, and
there is deliberately no `[auth]` section: omaghy never stores a credential
(`00-overview.md` §5).

---

## 5. Where the defaults came from

The five `[notifications]` defaults were **chosen by looking**, not specified in
advance. W2.3 shipped the surface with every alternative switchable at runtime
against the 29-row fixture corpus, and the project owner drove it in a real
terminal on 2026-09-13 and picked:

| Setting | Chosen | Notes |
|---|---|---|
| `reason` | `glyph` | Text mode is abbreviations either way — `Column::State` leaves six cells |
| `repo` | `full` | Chosen over `hide-when-shared`, which the implementer recommended |
| `rows` | `two-line` | Chosen over `one-line`, which `30-ui.md` §4.1 had assumed |
| `triage` | `grey` | `30-ui.md` §9's original answer, kept over the implementer's `sink` |
| `group` | `by-repo` | Chosen over `flat`, which the implementer recommended |

Three of the five overrule the implementer's recommendation, and two overrule
the spec. That is the argument for having built the comparator rather than
deciding on paper.

**`repo` and `group` interact.** The repo column drops automatically whenever
every visible row shares a repository — which inside a `by-repo` group is
always. So with the defaults above, the group header names the repository and
the column does not repeat it; `repo` governs ungrouped views.

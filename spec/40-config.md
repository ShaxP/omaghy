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

**A setting nobody applies says so.** `[keys]` is in §2's file and rebinding is
unbuilt, so the reader reports it as not yet applied rather than accepting it
silently. A config that appears to work and does not is worse than one that
refuses — and this is the only honest middle ground while the file documents
more than the code does.

**Config is read once, at startup.** Live reload is not a goal. `omaghy` starts
in milliseconds; restarting it is cheaper than watching a file. The one
exception is a change made from the settings surface (§6), which applies at
once — the change came from inside the program and there is nothing to detect.

**The file is not the only way in.** Everything in §2 is also editable from the
settings surface, and most people will never open the file. It stays the source
of truth: the surface reads and writes it, rather than keeping a second store
that could disagree with it.

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

# A section's number is how many **match**, not how many are fetched: `limit`
# caps the rows a section will hold once M2 renders them, and never the count.
#
# **A bad query does not fail.** Verified against the live API: search answers
# a nonexistent repository with 0, an invalid qualifier value by ignoring the
# qualifier, and an unbalanced quote with a number in the tens of thousands —
# all HTTP 200, no error. So a typo here produces a confident wrong number and
# nothing downstream can detect it. This is the one setting where "unknown
# keys warn" (§1) buys nothing, because the value is not ours to validate.

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

### 2.1 What building the reader found

The file above is normative and the code now matches it. It did not before:
three of the five `[notifications]` settings had internal names that disagreed
with the vocabulary this section documents — `rows` answered `2-line` where §2
says `two-line`, and `repo` answered `owner` and `shared` for `elide-owner` and
`hide-when-shared`. Nothing noticed because nothing read the file, so the
labels were only ever displayed, never matched.

They are now generated from one table alongside each enum, so §2's vocabulary,
what the settings surface offers, and what the reader accepts cannot drift
again. `the_documented_file_parses_to_the_documented_defaults` pins this
section's example to the code in both directions.

**The reader parses by hand rather than deriving `Deserialize`.** Serde offers
two behaviours for an unknown key — ignore it silently, or fail the file — and
§1 asks for the third.

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

Built: the route omaghy opens follows it, and the error names which step
supplied a bad value — `` `nonsense` (`default-route` in config.toml) is not a
route `` rather than a bare complaint about a word the user never typed. That
is the same problem §6.1 names for the settings surface, met first here.
`--config` / `OMAGHY_CONFIG` redirects which file is read, so the log line
names the file actually read and never the default one.

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

---

## 6. The settings surface

Editing a TOML file is a poor way to discover that an option exists. Every
setting in §2 is therefore reachable from a **settings surface** inside omaghy,
and that surface is how most people will change them.

**Not built.** The reader is (§2.1); this surface is not. Everything below is
still specification.

The one thing the reader settled for it: `parse` is a pure function of a
string, living in the `omaghy` binary. The surface will need to read *and
write* the file from inside `omaghy-tui`, which cannot see that module — so
moving it is a contract change for the PR that builds this, and deliberately a
cheap one.

**Reached by `,`, and from the command palette.** Deliberately *not* one of the
numbered surfaces: `1`–`7` address the seven content surfaces of
`30-ui.md` §2, and renumbering them to make room would break a keystroke people
have in their fingers. Settings is cross-cutting, like help and the palette.

### 6.1 What it shows

Sections mirroring §2, each setting on a row: its name, its current value, and
the alternatives. Moving the cursor and pressing `Enter` — or `h`/`l` — cycles
the value.

**Each row states where its value came from**: `default`, `config.toml`,
environment, or flag. This is the §4 precedence chain made visible, and it is
the thing a settings screen usually gets wrong — someone edits the file, sees
no change, and cannot tell that an environment variable is winning.

A one-line description per setting, in the same words as §2's comments. Both
come from one source, so they cannot drift.

### 6.2 Changes apply immediately

A setting takes effect on the frame after it changes, with no restart and no
save step. Changing `rows` to `one-line` should redraw the inbox behind the
surface if it is visible.

This is the one genuinely good thing about the W2.3 comparator, kept: seeing
the change is how you judge it. What the comparator got wrong was being *in*
the inbox, on undiscoverable single-letter keys, with no way to persist a
choice — so the switcher is deleted (`30-ui.md` §9) and this replaces it.

### 6.3 Writing the file

A change is written to `config.toml` immediately, creating it if absent.

**Comments and ordering in the file survive a write.** Someone who has
hand-edited and annotated their config must not have it reformatted because
they toggled one value in a UI. That means a format-preserving edit rather than
serialize-the-whole-struct — likely `toml_edit`, which is **not** in the M1
dependency set and needs adding under `90-plan.md` §2.1.

Only settings that differ from the default are written. A file listing every
value at its default is noise, and it silently freezes today's defaults against
future changes.

If the file cannot be written — read-only directory, no permission — the change
still applies for the session and the surface says it could not be saved. It
does not refuse the change, and it does not fail silently.

### 6.4 What it does not do

**No key rebinding.** `[keys]` stays file-only for now: capturing a chord
inside a TUI that is itself driven by keys is a surface of its own, and a
half-working one is worse than an honest "edit the file".

**No dashboard section editing.** Sections are a title plus GitHub search
syntax — a text-entry problem, and `30-ui.md` §5 records that a surface cannot
currently take free-typed input at all. File-only until that is fixed.

Both exclusions are in §2's file, so nothing is unreachable — only less
convenient than it will eventually be.

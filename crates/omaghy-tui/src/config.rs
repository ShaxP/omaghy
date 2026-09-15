//! `config.toml` — the settings, and the reader that turns text into them.
//!
//! Three rules from `spec/40-config.md` §1 shape everything here, and each is
//! a deliberate refusal to fail:
//!
//! **Absent config produces a working app.** No file is the ordinary case, not
//! an error. Every key is optional and every default is the one chosen by
//! looking at real rows in a real terminal (§5).
//!
//! **Unknown keys warn, never fail.** A typo, or a key from a version that has
//! not shipped yet, is reported and ignored. Refusing to start over one
//! unrecognised line is the wrong trade for a program someone opens to check
//! whether CI passed.
//!
//! **Invalid values warn and fall back**, naming the key, the bad value, and
//! the accepted set. `reason = "glif"` has to say so rather than silently
//! rendering nothing.
//!
//! That is why this parses a [`toml::Table`] by hand instead of deriving
//! `Deserialize` on a struct. Serde offers exactly two behaviours for a key it
//! does not know — ignore it silently, or fail the whole file — and §1 asks
//! for the third.
//!
//! # Where it lives, and what is missing from it
//!
//! It moved here from the binary when the settings surface was built (§6):
//! that surface renders these settings and must therefore name their types,
//! and it lives in `omaghy-tui`. Every field is a type this crate already
//! knew — [`Variants`] is its own, `DashboardConfig` is the store's, and an
//! interval is a `Duration`.
//!
//! **No file is opened here.** `omaghy-tui` performs no I/O (`CONTRIBUTING.md`)
//! and that does not get an exception for a small file. [`parse`] is a pure
//! function of a string, and persisting a change goes out through
//! [`ConfigWriter`] — the same shape as the `Store` seam, for the same reason.

use crate::surfaces::notifications::{
    GroupMode, ReasonMode, RepoMode, RowMode, TriageMode, Variants,
};
use omaghy_store::query::{DashboardConfig, DashboardSection};
use time::Duration;
use toml::Value;

/// Everything `config.toml` can say, resolved.
///
/// Intervals are plain [`Duration`]s rather than `omaghy_sync::PollConfig`:
/// this crate does not depend on the syncer and should not start to for two
/// numbers. The binary builds a `PollConfig` from them.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// `[general] default-route`. `None` means the built-in default; the
    /// command line still outranks it (§4).
    pub default_route: Option<String>,
    pub inbox: Variants,
    pub dashboard: DashboardConfig,
    pub refresh_notifications: Duration,
    pub refresh_dashboard: Duration,
    /// Where each setting's value came from (§4). The settings surface shows
    /// it per row, because "I edited the file and nothing changed" is the
    /// question a settings screen exists to answer (§6.1).
    pub sources: Sources,
}

/// `40-config.md` §2's `[refresh]` defaults, as the file documents them.
pub const DEFAULT_REFRESH_NOTIFICATIONS: Duration = Duration::seconds(60);
pub const DEFAULT_REFRESH_DASHBOARD: Duration = Duration::seconds(300);

impl Default for Config {
    fn default() -> Self {
        Self {
            default_route: None,
            inbox: Variants::default(),
            dashboard: DashboardConfig::default(),
            refresh_notifications: DEFAULT_REFRESH_NOTIFICATIONS,
            refresh_dashboard: DEFAULT_REFRESH_DASHBOARD,
            sources: Sources::default(),
        }
    }
}

// ------------------------------------------------------------- provenance

/// Where a setting's value came from — `40-config.md` §4's chain, made
/// visible.
///
/// The settings surface shows this per row because it is the thing a settings
/// screen usually gets wrong (§6.1): someone edits the file, sees no change,
/// and cannot tell that something further up the chain is winning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    Default,
    File,
    Environment,
    Flag,
    /// Changed from the settings surface this session. Distinct from `File`
    /// until it is written, and distinct *after* a failed write — which is
    /// exactly when a user needs to know it will not survive a restart (§6.3).
    Session,
    /// Changed here, and the file could not be written.
    Unsaved,
}

impl Source {
    /// The word the settings surface puts on the row.
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::File => "config.toml",
            Self::Environment => "environment",
            Self::Flag => "flag",
            Self::Session => "this session",
            Self::Unsaved => "unsaved!",
        }
    }
}

/// One [`Source`] per setting, named the same as [`Config`]'s fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sources {
    pub default_route: Source,
    pub reason: Source,
    pub repo: Source,
    pub rows: Source,
    pub triage: Source,
    pub group: Source,
    pub refresh_notifications: Source,
    pub refresh_dashboard: Source,
}

// -------------------------------------------------------------- catalogue

/// Every setting the settings surface can change.
///
/// One table, so that §2's vocabulary, the descriptions on screen, the values
/// offered, and what gets written to the file cannot disagree — §6.1 asks for
/// exactly that ("Both come from one source, so they cannot drift").
///
/// `[keys]` and `[dashboard] sections` are absent deliberately: §6.4 keeps
/// them file-only until a surface can take free-typed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingId {
    DefaultRoute,
    Reason,
    Repo,
    Rows,
    Triage,
    Group,
    RefreshNotifications,
    RefreshDashboard,
}

/// The routes `default-route` may name.
///
/// The seven content surfaces of `30-ui.md` §2, by the names `Route` parses.
/// Listed rather than derived because `SurfaceId::ALL` is an ordering for the
/// number keys, and this is a set of values for a setting; they happen to
/// coincide today and need not.
const ROUTES: &[&str] = &[
    "dashboard",
    "notifications",
    "pulls",
    "issues",
    "actions",
    "repos",
    "search",
];

/// The intervals `[refresh]` offers on screen.
///
/// A ladder rather than free entry: §6.1 cycles values, and a surface cannot
/// take typed input yet (§6.4, `30-ui.md` §5). The file still accepts any
/// positive number of seconds — a value from the file that is not on the
/// ladder shows as it is and cycles to the next one above it.
const INTERVALS: &[i64] = &[30, 60, 120, 300, 600, 1800];

impl SettingId {
    pub const ALL: &'static [Self] = &[
        Self::DefaultRoute,
        Self::Reason,
        Self::Repo,
        Self::Rows,
        Self::Triage,
        Self::Group,
        Self::RefreshNotifications,
        Self::RefreshDashboard,
    ];

    /// The `[section]` this setting lives under in the file.
    pub fn section(self) -> &'static str {
        match self {
            Self::DefaultRoute => "general",
            Self::Reason | Self::Repo | Self::Rows | Self::Triage | Self::Group => "notifications",
            Self::RefreshNotifications | Self::RefreshDashboard => "refresh",
        }
    }

    /// The key, as the file spells it.
    pub fn key(self) -> &'static str {
        match self {
            Self::DefaultRoute => "default-route",
            Self::Reason => "reason",
            Self::Repo => "repo",
            Self::Rows => "rows",
            Self::Triage => "triage",
            Self::Group => "group",
            Self::RefreshNotifications => "notifications",
            Self::RefreshDashboard => "dashboard",
        }
    }

    /// One line, in §2's own words. The screen and the file say the same thing
    /// because there is one copy of it.
    pub fn description(self) -> &'static str {
        match self {
            Self::DefaultRoute => "Route opened when omaghy is run with no argument",
            Self::Reason => "How the reason for a notification is encoded",
            Self::Repo => "How repository names are shown",
            Self::Rows => "Row height",
            Self::Triage => "What happens to a row when you mark it read",
            Self::Group => "Row grouping",
            Self::RefreshNotifications => "Seconds between inbox polls — GitHub's floor still wins",
            Self::RefreshDashboard => "Seconds between dashboard polls",
        }
    }

    /// The values this setting accepts, in cycling order.
    pub fn options(self) -> Vec<String> {
        fn owned(v: Vec<&'static str>) -> Vec<String> {
            v.into_iter().map(str::to_owned).collect()
        }
        match self {
            Self::DefaultRoute => owned(ROUTES.to_vec()),
            Self::Reason => owned(ReasonMode::labels()),
            Self::Repo => owned(RepoMode::labels()),
            Self::Rows => owned(RowMode::labels()),
            Self::Triage => owned(TriageMode::labels()),
            Self::Group => owned(GroupMode::labels()),
            Self::RefreshNotifications | Self::RefreshDashboard => {
                INTERVALS.iter().map(|s| format!("{s}s")).collect()
            }
        }
    }

    /// The value as it is now, spelled the way [`Self::options`] spells it.
    pub fn current(self, cfg: &Config) -> String {
        match self {
            Self::DefaultRoute => cfg
                .default_route
                .clone()
                .unwrap_or_else(|| "notifications".to_owned()),
            Self::Reason => cfg.inbox.reason.label().to_owned(),
            Self::Repo => cfg.inbox.repo.label().to_owned(),
            Self::Rows => cfg.inbox.rows.label().to_owned(),
            Self::Triage => cfg.inbox.triage.label().to_owned(),
            Self::Group => cfg.inbox.group.label().to_owned(),
            Self::RefreshNotifications => format!("{}s", cfg.refresh_notifications.whole_seconds()),
            Self::RefreshDashboard => format!("{}s", cfg.refresh_dashboard.whole_seconds()),
        }
    }

    pub fn source(self, cfg: &Config) -> Source {
        let s = &cfg.sources;
        match self {
            Self::DefaultRoute => s.default_route,
            Self::Reason => s.reason,
            Self::Repo => s.repo,
            Self::Rows => s.rows,
            Self::Triage => s.triage,
            Self::Group => s.group,
            Self::RefreshNotifications => s.refresh_notifications,
            Self::RefreshDashboard => s.refresh_dashboard,
        }
    }

    /// Record where this setting's value now comes from.
    ///
    /// Public because the app sets it after a write: the value became the
    /// file's, or — if the write failed — something the next restart will
    /// lose, which the row has to say (§6.3).
    pub fn set_source_public(self, cfg: &mut Config, source: Source) {
        self.set_source(cfg, source)
    }

    fn set_source(self, cfg: &mut Config, source: Source) {
        let s = &mut cfg.sources;
        match self {
            Self::DefaultRoute => s.default_route = source,
            Self::Reason => s.reason = source,
            Self::Repo => s.repo = source,
            Self::Rows => s.rows = source,
            Self::Triage => s.triage = source,
            Self::Group => s.group = source,
            Self::RefreshNotifications => s.refresh_notifications = source,
            Self::RefreshDashboard => s.refresh_dashboard = source,
        }
    }

    /// Move to the next value, wrapping. `forward` is `l`/`Enter`; `h` goes
    /// back.
    ///
    /// A current value that is not on the list — an interval from the file
    /// that is not on the ladder — lands on the first option above it rather
    /// than jumping to the start, so cycling from `45s` goes to `60s`.
    pub fn cycle(self, cfg: &mut Config, forward: bool) -> Scalar {
        let options = self.options();
        let current = self.current(cfg);
        let next = match options.iter().position(|o| *o == current) {
            Some(i) => {
                let n = options.len();
                let step = if forward { 1 } else { n - 1 };
                options[(i + step) % n].clone()
            }
            None => self.nearest_above(&options, &current),
        };
        self.apply(cfg, &next);
        self.set_source(cfg, Source::Session);
        self.scalar(&next)
    }

    fn nearest_above(self, options: &[String], current: &str) -> String {
        match self {
            Self::RefreshNotifications | Self::RefreshDashboard => {
                let now: i64 = current.trim_end_matches('s').parse().unwrap_or(0);
                INTERVALS
                    .iter()
                    .find(|s| **s > now)
                    .map(|s| format!("{s}s"))
                    .unwrap_or_else(|| format!("{}s", INTERVALS[0]))
            }
            // Every other setting's value came from its own enum, so it is
            // always on the list.
            _ => options[0].clone(),
        }
    }

    /// Set the value from one of [`Self::options`]. Anything else is ignored:
    /// the caller only ever passes what `options` produced.
    fn apply(self, cfg: &mut Config, value: &str) {
        match self {
            Self::DefaultRoute => cfg.default_route = Some(value.to_owned()),
            Self::Reason => {
                if let Some(v) = ReasonMode::from_label(value) {
                    cfg.inbox.reason = v;
                }
            }
            Self::Repo => {
                if let Some(v) = RepoMode::from_label(value) {
                    cfg.inbox.repo = v;
                }
            }
            Self::Rows => {
                if let Some(v) = RowMode::from_label(value) {
                    cfg.inbox.rows = v;
                }
            }
            Self::Triage => {
                if let Some(v) = TriageMode::from_label(value) {
                    cfg.inbox.triage = v;
                }
            }
            Self::Group => {
                if let Some(v) = GroupMode::from_label(value) {
                    cfg.inbox.group = v;
                }
            }
            Self::RefreshNotifications => {
                cfg.refresh_notifications = Duration::seconds(seconds_of(value))
            }
            Self::RefreshDashboard => cfg.refresh_dashboard = Duration::seconds(seconds_of(value)),
        }
    }

    /// What to write to the file for a value.
    fn scalar(self, value: &str) -> Scalar {
        match self {
            Self::RefreshNotifications | Self::RefreshDashboard => Scalar::Int(seconds_of(value)),
            _ => Scalar::Str(value.to_owned()),
        }
    }

    /// Whether the value now equals the default — §6.3 writes only the
    /// settings that differ from it, and removes the key when one returns.
    pub fn is_default(self, cfg: &Config) -> bool {
        self.current(cfg) == self.current(&Config::default())
    }
}

fn seconds_of(label: &str) -> i64 {
    label.trim_end_matches('s').parse().unwrap_or(60)
}

// ------------------------------------------------------------ persistence

/// A value as the file spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scalar {
    Str(String),
    Int(i64),
}

/// Persisting one setting.
///
/// A trait rather than a function because writing a file is I/O, and
/// `omaghy-tui` performs none (`CONTRIBUTING.md`) — the same seam as `Store`,
/// for the same reason. The binary implements it with a format-preserving
/// edit; tests implement it with a `Vec`.
///
/// `None` removes the key: the value is back to its default, and §6.3 writes
/// only what differs from one.
pub trait ConfigWriter: std::fmt::Debug + Send + Sync {
    /// `Err` is the message shown to the user. §6.3: a failed write does not
    /// refuse the change and does not fail silently.
    fn write(&self, section: &str, key: &str, value: Option<Scalar>) -> Result<(), String>;
}

/// The writer that keeps nothing — the fixture path, and tests that are not
/// about persistence.
#[derive(Debug, Clone, Copy)]
pub struct Discard;

impl ConfigWriter for Discard {
    fn write(&self, _section: &str, _key: &str, _value: Option<Scalar>) -> Result<(), String> {
        Ok(())
    }
}

/// Parse the file's text. Pure, so the rules above are testable without a
/// filesystem — and so this can move to wherever §6 needs it.
pub fn parse(src: &str) -> (Config, Vec<String>) {
    let mut cfg = Config::default();
    let mut warn = Vec::new();

    let table: toml::Table = match src.parse() {
        Ok(t) => t,
        Err(e) => {
            // The one thing that is not recoverable per-key: if it is not
            // TOML, there are no keys. Still not fatal — the defaults work.
            warn.push(format!("is not valid TOML and was ignored entirely: {e}"));
            return (cfg, warn);
        }
    };

    for (section, value) in &table {
        match section.as_str() {
            "general" => general(value, &mut cfg, &mut warn),
            "notifications" => notifications(value, &mut cfg, &mut warn),
            "dashboard" => dashboard(value, &mut cfg, &mut warn),
            "refresh" => refresh(value, &mut cfg, &mut warn),
            "keys" => warn.push(
                "[keys] is not applied yet — rebinding is unbuilt, and every key \
                 is still the one in `?`. The section is kept, not dropped."
                    .to_owned(),
            ),
            other => warn.push(format!("[{other}] is not a section omaghy knows; ignored")),
        }
    }
    (cfg, warn)
}

/// A section's key/value pairs, or a warning if it is not a table at all.
fn as_table<'a>(
    name: &str,
    value: &'a Value,
    warn: &mut Vec<String>,
) -> Option<&'a toml::map::Map<String, Value>> {
    match value.as_table() {
        Some(t) => Some(t),
        None => {
            warn.push(format!("[{name}] should be a table; ignored"));
            None
        }
    }
}

fn general(value: &Value, cfg: &mut Config, warn: &mut Vec<String>) {
    let Some(t) = as_table("general", value, warn) else {
        return;
    };
    for (k, v) in t {
        match k.as_str() {
            // Not validated here: a route is `omaghy-tui`'s vocabulary and it
            // already reports an unparseable one by name. Validating it twice
            // means two lists of route names that can disagree.
            "default-route" => match v.as_str() {
                Some(s) => {
                    cfg.default_route = Some(s.to_owned());
                    cfg.sources.default_route = Source::File;
                }
                None => warn.push(string_expected("general.default-route", v)),
            },
            other => warn.push(format!("general.{other} is not a setting; ignored")),
        }
    }
}

fn notifications(value: &Value, cfg: &mut Config, warn: &mut Vec<String>) {
    let Some(t) = as_table("notifications", value, warn) else {
        return;
    };
    for (k, v) in t {
        match k.as_str() {
            "reason" => {
                if enumerated(
                    "notifications.reason",
                    v,
                    ReasonMode::from_label,
                    ReasonMode::labels(),
                    &mut cfg.inbox.reason,
                    warn,
                ) {
                    cfg.sources.reason = Source::File;
                }
            }
            "repo" => {
                if enumerated(
                    "notifications.repo",
                    v,
                    RepoMode::from_label,
                    RepoMode::labels(),
                    &mut cfg.inbox.repo,
                    warn,
                ) {
                    cfg.sources.repo = Source::File;
                }
            }
            "rows" => {
                if enumerated(
                    "notifications.rows",
                    v,
                    RowMode::from_label,
                    RowMode::labels(),
                    &mut cfg.inbox.rows,
                    warn,
                ) {
                    cfg.sources.rows = Source::File;
                }
            }
            "triage" => {
                if enumerated(
                    "notifications.triage",
                    v,
                    TriageMode::from_label,
                    TriageMode::labels(),
                    &mut cfg.inbox.triage,
                    warn,
                ) {
                    cfg.sources.triage = Source::File;
                }
            }
            "group" => {
                if enumerated(
                    "notifications.group",
                    v,
                    GroupMode::from_label,
                    GroupMode::labels(),
                    &mut cfg.inbox.group,
                    warn,
                ) {
                    cfg.sources.group = Source::File;
                }
            }
            other => warn.push(format!("notifications.{other} is not a setting; ignored")),
        }
    }
}

/// One setting whose value is a word from a fixed set.
///
/// The accepted set comes from the enum itself, so the message cannot list
/// options the parser would reject.
fn enumerated<T: Copy>(
    key: &str,
    v: &Value,
    from_label: fn(&str) -> Option<T>,
    accepted: Vec<&'static str>,
    out: &mut T,
    warn: &mut Vec<String>,
) -> bool {
    let Some(s) = v.as_str() else {
        warn.push(string_expected(key, v));
        return false;
    };
    match from_label(s) {
        Some(parsed) => {
            *out = parsed;
            true
        }
        None => {
            warn.push(format!(
                "{key} = \"{s}\" is not one of {}; using the default",
                accepted.join(", ")
            ));
            false
        }
    }
}

fn dashboard(value: &Value, cfg: &mut Config, warn: &mut Vec<String>) {
    let Some(t) = as_table("dashboard", value, warn) else {
        return;
    };
    for (k, v) in t {
        match k.as_str() {
            "sections" => match v.clone().try_into::<Vec<DashboardSection>>() {
                // An empty list is a choice, not a mistake: it is how you turn
                // the dashboard off. `30-ui.md` §8.1 says so, and renders the
                // empty state for exactly this case.
                Ok(sections) => cfg.dashboard = DashboardConfig { sections },
                Err(e) => warn.push(format!(
                    "dashboard.sections is not a list of {{ title, query, limit }}: \
                     {e}; using the defaults"
                )),
            },
            other => warn.push(format!("dashboard.{other} is not a setting; ignored")),
        }
    }
}

fn refresh(value: &Value, cfg: &mut Config, warn: &mut Vec<String>) {
    let Some(t) = as_table("refresh", value, warn) else {
        return;
    };
    for (k, v) in t {
        match k.as_str() {
            "notifications" => {
                if seconds(
                    "refresh.notifications",
                    v,
                    &mut cfg.refresh_notifications,
                    warn,
                ) {
                    cfg.sources.refresh_notifications = Source::File;
                }
            }
            "dashboard" => {
                if seconds("refresh.dashboard", v, &mut cfg.refresh_dashboard, warn) {
                    cfg.sources.refresh_dashboard = Source::File;
                }
            }
            other => warn.push(format!("refresh.{other} is not a setting; ignored")),
        }
    }
}

/// A duration in whole seconds.
///
/// Floors are **not** applied here. `omaghy-sync` applies them at every tick
/// against GitHub's advertised interval, which this file cannot know — so
/// clamping now would only produce a stored value that disagrees with the one
/// actually used (`20-store.md` §6.1).
/// Returns whether the value was taken, so the caller can record that this
/// setting now comes from the file.
fn seconds(key: &str, v: &Value, out: &mut Duration, warn: &mut Vec<String>) -> bool {
    match v.as_integer() {
        Some(n) if n > 0 => {
            *out = Duration::seconds(n);
            true
        }
        Some(n) => {
            warn.push(format!(
                "{key} = {n} is not a positive number of seconds; using the default"
            ));
            false
        }
        None => {
            warn.push(format!(
                "{key} should be a whole number of seconds, not {}; using the default",
                kind_of(v)
            ));
            false
        }
    }
}

fn string_expected(key: &str, v: &Value) -> String {
    format!(
        "{key} should be a string, not {}; using the default",
        kind_of(v)
    )
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "a string",
        Value::Integer(_) => "a number",
        Value::Float(_) => "a number",
        Value::Boolean(_) => "a boolean",
        Value::Datetime(_) => "a date",
        Value::Array(_) => "a list",
        Value::Table(_) => "a table",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warnings(src: &str) -> Vec<String> {
        parse(src).1
    }

    #[test]
    fn an_empty_file_is_every_default() {
        assert_eq!(parse(""), (Config::default(), Vec::new()));
    }

    /// The file `40-config.md` §2 documents, parsed.
    ///
    /// Every value in it is the documented default, so this pins the spec to
    /// the code in both directions: a default that changes here without the
    /// spec changing fails, and so does a value the spec names that the
    /// parser does not accept. It is the test that would have caught `rows`
    /// answering `2-line` where §2 says `two-line`.
    #[test]
    fn the_documented_file_parses_to_the_documented_defaults() {
        let src = r#"
[general]
default-route = "notifications"

[notifications]
reason = "glyph"
repo = "full"
rows = "two-line"
triage = "grey"
group = "by-repo"

[dashboard]
sections = [
  { title = "Needs my review",   query = "is:open is:pr review-requested:@me", limit = 10 },
  { title = "My pull requests",  query = "is:open is:pr author:@me",           limit = 10 },
  { title = "Assigned to me",    query = "is:open assignee:@me",               limit = 10 },
  { title = "Recently mentioned", query = "is:open mentions:@me",              limit = 10 },
]

[refresh]
notifications = 60
dashboard     = 300
"#;
        let (cfg, warn) = parse(src);
        assert!(warn.is_empty(), "the documented file warns: {warn:?}");
        assert_eq!(cfg.inbox, Variants::default());
        assert_eq!(cfg.dashboard, DashboardConfig::default());
        assert_eq!(cfg.refresh_notifications, DEFAULT_REFRESH_NOTIFICATIONS);
        assert_eq!(cfg.refresh_dashboard, DEFAULT_REFRESH_DASHBOARD);
        assert_eq!(cfg.default_route.as_deref(), Some("notifications"));
    }

    #[test]
    fn every_alternative_is_accepted() {
        let (cfg, warn) = parse(
            r#"
[notifications]
reason = "none"
repo = "hide-when-shared"
rows = "one-line"
triage = "sink"
group = "flat"
"#,
        );
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(cfg.inbox.reason, ReasonMode::Hidden);
        assert_eq!(cfg.inbox.repo, RepoMode::HiddenWhenShared);
        assert_eq!(cfg.inbox.rows, RowMode::OneLine);
        assert_eq!(cfg.inbox.triage, TriageMode::Sink);
        assert_eq!(cfg.inbox.group, GroupMode::Flat);
    }

    /// §1: invalid values warn and fall back, naming the key, the bad value,
    /// **and the accepted set**. The spec's own example of the failure.
    #[test]
    fn a_misspelt_value_says_what_was_accepted() {
        let (cfg, warn) = parse("[notifications]\nreason = \"glif\"\n");
        assert_eq!(
            cfg.inbox.reason,
            ReasonMode::Glyph,
            "a bad value falls back; it does not render nothing"
        );
        assert_eq!(warn.len(), 1);
        let w = &warn[0];
        assert!(w.contains("notifications.reason"), "names the key: {w}");
        assert!(w.contains("glif"), "names the bad value: {w}");
        for accepted in ReasonMode::labels() {
            assert!(w.contains(accepted), "names `{accepted}` as accepted: {w}");
        }
    }

    /// §1: unknown keys warn, never fail. Both a section and a key.
    #[test]
    fn unknown_keys_warn_and_everything_else_still_applies() {
        let (cfg, warn) = parse(
            r#"
[colours]
background = "black"

[notifications]
reasn = "text"
group = "flat"
"#,
        );
        assert_eq!(
            cfg.inbox.group,
            GroupMode::Flat,
            "a typo on one line must not cost the next one"
        );
        assert_eq!(cfg.inbox.reason, ReasonMode::Glyph);
        assert_eq!(warn.len(), 2, "{warn:?}");
        assert!(warn.iter().any(|w| w.contains("[colours]")), "{warn:?}");
        assert!(warn.iter().any(|w| w.contains("reasn")), "{warn:?}");
    }

    /// There is no `icons` setting — `40-config.md` §3 says so deliberately,
    /// and a reader that quietly accepted one would undo that decision.
    #[test]
    fn icons_is_not_a_setting() {
        let warn = warnings("[general]\nicons = \"ascii\"\n");
        assert_eq!(warn.len(), 1);
        assert!(warn[0].contains("icons"), "{:?}", warn);
    }

    #[test]
    fn a_value_of_the_wrong_type_warns_rather_than_failing() {
        let (cfg, warn) =
            parse("[notifications]\nreason = 3\n[refresh]\nnotifications = \"fast\"\n");
        assert_eq!(cfg.inbox.reason, ReasonMode::Glyph);
        assert_eq!(cfg.refresh_notifications, DEFAULT_REFRESH_NOTIFICATIONS);
        assert_eq!(cfg.refresh_dashboard, DEFAULT_REFRESH_DASHBOARD);
        assert_eq!(warn.len(), 2, "{warn:?}");
        assert!(warn.iter().all(|w| w.contains("default")), "{warn:?}");
    }

    #[test]
    fn a_refresh_interval_must_be_a_positive_number_of_seconds() {
        let (cfg, warn) = parse("[refresh]\nnotifications = 0\ndashboard = -5\n");
        assert_eq!(cfg.refresh_notifications, DEFAULT_REFRESH_NOTIFICATIONS);
        assert_eq!(cfg.refresh_dashboard, DEFAULT_REFRESH_DASHBOARD);
        assert_eq!(warn.len(), 2, "{warn:?}");
    }

    /// The floors live in `omaghy-sync` and are applied per tick against
    /// GitHub's advertised interval. Storing a clamped value here would
    /// disagree with the one actually used.
    #[test]
    fn a_short_interval_is_stored_as_written_and_floored_elsewhere() {
        let (cfg, warn) = parse("[refresh]\nnotifications = 5\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(cfg.refresh_notifications, Duration::seconds(5));
    }

    #[test]
    fn dashboard_sections_replace_the_defaults_entirely() {
        let (cfg, warn) = parse(
            r#"
[dashboard]
sections = [{ title = "Mine", query = "is:open author:@me", limit = 5 }]
"#,
        );
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(cfg.dashboard.sections.len(), 1);
        assert_eq!(cfg.dashboard.sections[0].title, "Mine");
    }

    /// An empty list is how you turn the dashboard off, not a mistake.
    #[test]
    fn no_sections_is_a_choice() {
        let (cfg, warn) = parse("[dashboard]\nsections = []\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert!(cfg.dashboard.sections.is_empty());
    }

    #[test]
    fn a_malformed_section_keeps_the_defaults_and_says_why() {
        let (cfg, warn) = parse("[dashboard]\nsections = [\"needs my review\"]\n");
        assert_eq!(cfg.dashboard, DashboardConfig::default());
        assert_eq!(warn.len(), 1);
        assert!(warn[0].contains("sections"), "{:?}", warn);
    }

    /// Not TOML at all is still not fatal — but it must not be silent either.
    #[test]
    fn a_file_that_is_not_toml_warns_and_leaves_the_defaults() {
        let (cfg, warn) = parse("this is not toml {{{");
        assert_eq!(cfg, Config::default());
        assert_eq!(warn.len(), 1);
        assert!(warn[0].contains("not valid TOML"), "{:?}", warn);
    }

    /// `[keys]` is in §2's file but rebinding is unbuilt. Accepting it
    /// silently would be a config that appears to work and does not.
    #[test]
    fn keys_are_parsed_but_reported_as_not_yet_applied() {
        let warn = warnings("[keys]\n\"app.quit\" = [\"q\", \"ctrl-c\"]\n");
        assert_eq!(warn.len(), 1);
        assert!(warn[0].contains("not applied yet"), "{:?}", warn);
    }
}

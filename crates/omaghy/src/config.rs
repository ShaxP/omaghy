//! `config.toml` — read once, at startup.
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
//! # Where it lives
//!
//! In the binary, because the binary is what reads it: configuration arrives
//! once at startup and is handed to the crates that consume it — `Variants` to
//! `omaghy-tui`, `DashboardConfig` to the store and the syncer, `PollConfig`
//! to the poll loop. Each crate keeps owning its own type.
//!
//! The settings surface (§6) will need to *write* this file from inside
//! `omaghy-tui`, which cannot see this module. That is a contract change for
//! the PR that builds it, and the reason `parse` is a pure function of a
//! string: moving it costs nothing that a move should not cost.

use omaghy_store::query::{DashboardConfig, DashboardSection};
use omaghy_sync::PollConfig;
use omaghy_tui::surfaces::notifications::{
    GroupMode, ReasonMode, RepoMode, RowMode, TriageMode, Variants,
};
use std::path::{Path, PathBuf};
use time::Duration;
use toml::Value;

/// Everything `config.toml` can say, resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    /// `[general] default-route`. `None` means the built-in default; the
    /// command line still outranks it (§4).
    pub default_route: Option<String>,
    pub inbox: Variants,
    pub dashboard: DashboardConfig,
    pub refresh: PollConfig,
}

/// Where the file lives, per XDG. `None` if there is no home directory to
/// speak of, which is not an error — it means no config.
pub fn path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "omaghy").map(|d| d.config_dir().join("config.toml"))
}

/// Read the file at `p`, if it is there.
///
/// Every failure short of "the file is not TOML" is a warning: an unreadable
/// file, a missing one, a key nobody recognises. The returned `Config` is
/// always usable.
pub fn load_from(p: &Path) -> (Config, Vec<String>) {
    match std::fs::read_to_string(p) {
        Ok(src) => {
            let (cfg, mut warnings) = parse(&src);
            for w in &mut warnings {
                *w = format!("{}: {w}", p.display());
            }
            (cfg, warnings)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), Vec::new()),
        Err(e) => (
            Config::default(),
            vec![format!("{} could not be read: {e}", p.display())],
        ),
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
                Some(s) => cfg.default_route = Some(s.to_owned()),
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
            "reason" => enumerated(
                "notifications.reason",
                v,
                ReasonMode::from_label,
                ReasonMode::labels(),
                &mut cfg.inbox.reason,
                warn,
            ),
            "repo" => enumerated(
                "notifications.repo",
                v,
                RepoMode::from_label,
                RepoMode::labels(),
                &mut cfg.inbox.repo,
                warn,
            ),
            "rows" => enumerated(
                "notifications.rows",
                v,
                RowMode::from_label,
                RowMode::labels(),
                &mut cfg.inbox.rows,
                warn,
            ),
            "triage" => enumerated(
                "notifications.triage",
                v,
                TriageMode::from_label,
                TriageMode::labels(),
                &mut cfg.inbox.triage,
                warn,
            ),
            "group" => enumerated(
                "notifications.group",
                v,
                GroupMode::from_label,
                GroupMode::labels(),
                &mut cfg.inbox.group,
                warn,
            ),
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
) {
    let Some(s) = v.as_str() else {
        warn.push(string_expected(key, v));
        return;
    };
    match from_label(s) {
        Some(parsed) => *out = parsed,
        None => warn.push(format!(
            "{key} = \"{s}\" is not one of {}; using the default",
            accepted.join(", ")
        )),
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
            "notifications" => seconds(
                "refresh.notifications",
                v,
                &mut cfg.refresh.notifications,
                warn,
            ),
            "dashboard" => seconds("refresh.dashboard", v, &mut cfg.refresh.dashboard, warn),
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
fn seconds(key: &str, v: &Value, out: &mut Duration, warn: &mut Vec<String>) {
    match v.as_integer() {
        Some(n) if n > 0 => *out = Duration::seconds(n),
        Some(n) => warn.push(format!(
            "{key} = {n} is not a positive number of seconds; using the default"
        )),
        None => warn.push(format!(
            "{key} should be a whole number of seconds, not {}; using the default",
            kind_of(v)
        )),
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
    fn no_file_is_the_ordinary_case() {
        let (cfg, warn) = load_from(Path::new("/nonexistent/omaghy/config.toml"));
        assert_eq!(cfg, Config::default());
        assert!(
            warn.is_empty(),
            "an absent config is not a problem: {warn:?}"
        );
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
        assert_eq!(cfg.refresh, PollConfig::default());
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
        assert_eq!(cfg.refresh, PollConfig::default());
        assert_eq!(warn.len(), 2, "{warn:?}");
        assert!(warn.iter().all(|w| w.contains("default")), "{warn:?}");
    }

    #[test]
    fn a_refresh_interval_must_be_a_positive_number_of_seconds() {
        let (cfg, warn) = parse("[refresh]\nnotifications = 0\ndashboard = -5\n");
        assert_eq!(cfg.refresh, PollConfig::default());
        assert_eq!(warn.len(), 2, "{warn:?}");
    }

    /// The floors live in `omaghy-sync` and are applied per tick against
    /// GitHub's advertised interval. Storing a clamped value here would
    /// disagree with the one actually used.
    #[test]
    fn a_short_interval_is_stored_as_written_and_floored_elsewhere() {
        let (cfg, warn) = parse("[refresh]\nnotifications = 5\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(cfg.refresh.notifications, Duration::seconds(5));
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

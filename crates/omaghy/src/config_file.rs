//! Reading and writing `config.toml`.
//!
//! The settings themselves live in `omaghy_tui::config` — the settings surface
//! renders them and must name their types. What lives here is the half that
//! touches a disk, which `omaghy-tui` may not (`CONTRIBUTING.md`).
//!
//! # Writing preserves the file
//!
//! `40-config.md` §6.3: someone who has hand-edited and annotated their config
//! must not have it reformatted because they toggled one value in a UI. So
//! this edits with `toml_edit` rather than serialising the whole struct —
//! comments, ordering and spacing survive a write, and only the settings that
//! differ from their default are present at all.

use omaghy_tui::config::{Config, ConfigWriter, Scalar, parse};
use std::path::{Path, PathBuf};

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

/// Writes one setting at a time, in place.
#[derive(Debug, Clone)]
pub struct FileWriter {
    path: PathBuf,
}

impl FileWriter {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl ConfigWriter for FileWriter {
    fn write(&self, section: &str, key: &str, value: Option<Scalar>) -> Result<(), String> {
        let existing = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut doc = existing
            .parse::<toml_edit::DocumentMut>()
            // A file we cannot parse is one we must not rewrite: doing so
            // would silently discard whatever the user has in there. §6.3's
            // promise is that hand-editing survives, and that has to hold
            // when the hand-editing is mid-mistake.
            .map_err(|e| format!("config.toml could not be parsed, so it was left alone: {e}"))?;

        match value {
            Some(v) => {
                let item = match v {
                    Scalar::Str(s) => toml_edit::value(s),
                    Scalar::Int(n) => toml_edit::value(n),
                };
                // A missing section is created as a real table, so the file
                // gets `[notifications]` the way `40-config.md` §2 writes it.
                // Indexing straight into a missing key — `doc[s][k] = v` —
                // produces an *inline* table instead: `notifications = { … }`,
                // valid TOML that parses back fine, which is why the first
                // test of this missed it entirely.
                if !doc.as_table().contains_key(section) {
                    doc.insert(section, toml_edit::Item::Table(toml_edit::Table::new()));
                }
                if let Some(t) = doc[section].as_table_mut() {
                    t[key] = item;
                } else if let Some(t) = doc[section].as_inline_table_mut() {
                    // Someone wrote this section inline by hand. §6.3 says
                    // their formatting survives, so it stays inline.
                    if let Ok(value) = item.into_value() {
                        t.insert(key, value);
                    }
                }
            }
            None => {
                // Back to its default, so §6.3 wants the key gone rather than
                // written out at the default — a file listing every value
                // freezes today's defaults against tomorrow's.
                //
                // Both table kinds, because `as_table_mut` is `None` for an
                // inline one and this silently removed nothing at all.
                let emptied = match doc.get_mut(section) {
                    Some(item) => {
                        if let Some(t) = item.as_table_mut() {
                            t.remove(key);
                            t.is_empty()
                        } else if let Some(t) = item.as_inline_table_mut() {
                            t.remove(key);
                            t.is_empty()
                        } else {
                            false
                        }
                    }
                    None => false,
                };
                // An empty section left behind is noise the user did not
                // write, so it goes too — but only if it is empty.
                if emptied {
                    doc.remove(section);
                }
            }
        }

        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("{} could not be created: {e}", dir.display()))?;
        }
        std::fs::write(&self.path, doc.to_string())
            .map_err(|e| format!("{} could not be written: {e}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omaghy_tui::config::SettingId;

    fn temp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "omaghy-config-test-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
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

    /// Reported: "config.toml is not saved when the config changes."
    ///
    /// It was, but into an *inline* table — `notifications = { rows = "…" }`
    /// rather than `[notifications]`. Valid TOML that parses back fine, which
    /// is exactly why the first version of the test below missed it: it
    /// asserted the value survived a round trip and never looked at the shape.
    ///
    /// The shape then broke removal, which is the half a user notices: see
    /// `a_key_can_be_removed_from_a_section_written_inline`.
    #[test]
    fn a_new_section_is_written_as_a_section_not_an_inline_table() {
        let path = temp("shape.toml");
        std::fs::write(&path, "").unwrap();

        FileWriter::new(path.clone())
            .write(
                "notifications",
                "rows",
                Some(Scalar::Str("one-line".into())),
            )
            .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert!(
            after.contains("[notifications]"),
            "`40-config.md` §2 writes sections with headers:\n{after}"
        );
        assert!(
            !after.contains("notifications = {"),
            "an inline table is not what anyone hand-edits:\n{after}"
        );
        std::fs::remove_file(&path).ok();
    }

    /// The bug the report was actually about.
    ///
    /// `as_table_mut()` is `None` for an inline table, so removing a key from
    /// one silently did nothing — and returning a setting to its default is a
    /// removal. The change applied on screen and the file never moved.
    #[test]
    fn a_key_can_be_removed_from_a_section_written_inline() {
        let path = temp("inline.toml");
        std::fs::write(
            &path,
            "notifications = { rows = \"one-line\", group = \"flat\" }\n",
        )
        .unwrap();

        FileWriter::new(path.clone())
            .write("notifications", "rows", None)
            .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert!(!after.contains("rows"), "the key should be gone:\n{after}");
        assert!(after.contains("group"), "the others stay:\n{after}");
        std::fs::remove_file(&path).ok();
    }

    /// And a section written inline by hand stays inline: §6.3 promises that
    /// someone's formatting survives, and that includes formatting we would
    /// not have chosen.
    #[test]
    fn a_hand_written_inline_section_is_not_reformatted() {
        let path = temp("keep-inline.toml");
        std::fs::write(&path, "notifications = { group = \"flat\" }\n").unwrap();

        FileWriter::new(path.clone())
            .write(
                "notifications",
                "rows",
                Some(Scalar::Str("one-line".into())),
            )
            .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert!(after.contains("notifications = {"), "{after}");
        assert!(after.contains("one-line"), "{after}");
        std::fs::remove_file(&path).ok();
    }

    /// The whole flow, as it is actually used: change a setting, then change
    /// it back. The file must end where it started.
    #[test]
    fn changing_a_setting_and_changing_it_back_leaves_no_trace() {
        let path = temp("roundtrip.toml");
        std::fs::write(&path, "").unwrap();
        let w = FileWriter::new(path.clone());

        w.write(
            "notifications",
            "rows",
            Some(Scalar::Str("one-line".into())),
        )
        .unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("one-line"));

        w.write("notifications", "rows", None).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            !after.contains("rows") && !after.contains("[notifications]"),
            "back to where it started, section and all:\n{after}"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_write_creates_the_file_and_its_directory() {
        let dir = temp("new");
        let path = dir.join("config.toml");
        let w = FileWriter::new(path.clone());
        w.write(
            "notifications",
            "rows",
            Some(Scalar::Str("one-line".into())),
        )
        .expect("a config directory that does not exist yet is the normal first run");

        let back = std::fs::read_to_string(&path).unwrap();
        let (cfg, warn) = parse(&back);
        assert!(warn.is_empty(), "{warn:?}\n{back}");
        assert_eq!(SettingId::Rows.current(&cfg), "one-line");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// §6.3: someone who has hand-edited and annotated their config must not
    /// have it reformatted because they toggled one value in a UI.
    #[test]
    fn comments_and_ordering_survive_a_write() {
        let path = temp("annotated.toml");
        let original = "\
# My config. Do not reformat me.

[notifications]
# I prefer the glyph, most days.
reason = \"glyph\"
group  = \"flat\"   # trailing comment

[general]
default-route = \"dashboard\"
";
        std::fs::write(&path, original).unwrap();

        FileWriter::new(path.clone())
            .write(
                "notifications",
                "rows",
                Some(Scalar::Str("one-line".into())),
            )
            .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert!(
            after.contains("# My config. Do not reformat me."),
            "{after}"
        );
        assert!(
            after.contains("# I prefer the glyph, most days."),
            "{after}"
        );
        assert!(after.contains("# trailing comment"), "{after}");
        assert!(
            after.find("[notifications]") < after.find("[general]"),
            "section order moved:\n{after}"
        );
        assert!(after.contains("rows = \"one-line\""), "{after}");
        std::fs::remove_file(&path).ok();
    }

    /// §6.3: only settings that differ from the default are written, so a
    /// value returning to its default takes its key with it.
    #[test]
    fn returning_to_the_default_removes_the_key() {
        let path = temp("remove.toml");
        std::fs::write(
            &path,
            "[notifications]\nrows = \"one-line\"\ngroup = \"flat\"\n",
        )
        .unwrap();

        FileWriter::new(path.clone())
            .write("notifications", "rows", None)
            .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("rows"), "the key should be gone:\n{after}");
        assert!(after.contains("group"), "the others stay:\n{after}");
        std::fs::remove_file(&path).ok();
    }

    /// An emptied section is noise the user did not write.
    #[test]
    fn the_last_key_takes_its_section_with_it() {
        let path = temp("empty-section.toml");
        std::fs::write(&path, "[notifications]\nrows = \"one-line\"\n").unwrap();

        FileWriter::new(path.clone())
            .write("notifications", "rows", None)
            .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            !after.contains("[notifications]"),
            "an empty section should go:\n{after}"
        );
        std::fs::remove_file(&path).ok();
    }

    /// A file we cannot parse is one we must not rewrite — that would discard
    /// whatever the user was in the middle of typing.
    #[test]
    fn a_broken_file_is_reported_and_left_alone() {
        let path = temp("broken.toml");
        let original = "[notifications\nrows = oops";
        std::fs::write(&path, original).unwrap();

        let err = FileWriter::new(path.clone())
            .write(
                "notifications",
                "rows",
                Some(Scalar::Str("one-line".into())),
            )
            .expect_err("a file that is not TOML cannot be edited in place");
        assert!(err.contains("left alone"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "the file must be untouched"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn an_unwritable_path_reports_rather_than_panicking() {
        let err = FileWriter::new(PathBuf::from("/proc/version/config.toml"))
            .write(
                "notifications",
                "rows",
                Some(Scalar::Str("one-line".into())),
            )
            .expect_err("/proc is not writable");
        assert!(!err.is_empty());
    }
}

#[cfg(test)]
mod reported {
    use super::*;

    /// The file this was reported against, byte for byte.
    #[test]
    fn the_reported_file_can_be_edited_and_have_keys_removed() {
        let path = {
            let mut p = std::env::temp_dir();
            p.push(format!("omaghy-reported-{}.toml", std::process::id()));
            p
        };
        std::fs::write(
            &path,
            "general = { default-route = \"dashboard\" }\n\
             notifications = { repo = \"elide-owner\", reason = \"none\" , triage = \"sink\" , rows = \"one-line\" , group = \"flat\" }\n",
        )
        .unwrap();
        let w = FileWriter::new(path.clone());

        // Change one, and put another back to its default.
        w.write(
            "notifications",
            "rows",
            Some(Scalar::Str("two-line".into())),
        )
        .unwrap();
        w.write("notifications", "group", None).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        let (cfg, warn) = parse(&after);
        std::fs::remove_file(&path).ok();

        assert!(warn.is_empty(), "{warn:?}\n{after}");
        assert!(!after.contains("group"), "the removal landed:\n{after}");
        assert!(after.contains("two-line"), "and the change did:\n{after}");
        assert_eq!(cfg.inbox.reason.label(), "none", "the rest is untouched");
        assert_eq!(cfg.default_route.as_deref(), Some("dashboard"));
    }
}

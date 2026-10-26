//! The user's settings: the values, the tolerant JSON load and the atomic save.
//! The file is `%APPDATA%\Volatus\settings.json`. No test touches the real one.

// Unused until the settings window is wired up; Task 4 removes the allow.
#![allow(dead_code)]

use crate::model::Unit;
use std::path::{Path, PathBuf};

/// Which colour scheme the window uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];

    /// The choice at position `i` in `ALL`; an unknown index is `System`.
    pub fn from_index(_i: i32) -> ThemeChoice {
        ThemeChoice::System
    }

    /// The position of this choice in `ALL`.
    pub fn index(self) -> i32 {
        0
    }

    /// `None` follows Windows; `Some(dark)` forces the scheme.
    pub fn forced_dark(self) -> Option<bool> {
        None
    }
}

/// A part of the window that can be hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Header,
    Tree,
    Strip,
    Chips,
    Labels,
}

impl Part {
    pub const ALL: [Part; 5] = [Part::Header, Part::Tree, Part::Strip, Part::Chips, Part::Labels];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub unit: Unit,
    pub show_header: bool,
    pub show_tree: bool,
    pub show_strip: bool,
    pub show_chips: bool,
    pub show_labels: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::System,
            unit: Unit::Dynamic,
            show_header: false,
            show_tree: false,
            show_strip: false,
            show_chips: false,
            show_labels: false,
        }
    }
}

impl Settings {
    pub fn shown(&self, _p: Part) -> bool {
        false
    }

    pub fn set_shown(&mut self, _p: Part, _on: bool) {}
}

/// How a load went.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    /// No file yet.
    Fresh,
    /// Every field read cleanly (missing and unknown fields are fine).
    Ok,
    /// These fields were wrong and fell back to their defaults.
    Partial(Vec<&'static str>),
    /// Empty, not JSON, or not an object: all defaults.
    Damaged,
}

/// Reads settings text. Pure. A leading UTF-8 BOM is stripped.
pub fn parse(_text: &str) -> (Settings, Loaded) {
    (Settings::default(), Loaded::Fresh)
}

pub fn to_json(_s: &Settings) -> String {
    String::new()
}

/// Reads the file. On `Partial` and `Damaged` the file is first copied to `<name>.bad`.
pub fn load(_path: &Path) -> (Settings, Loaded) {
    (Settings::default(), Loaded::Fresh)
}

/// Writes `<name>.tmp` beside the file, then renames it over the file.
pub fn save(_path: &Path, _s: &Settings) -> std::io::Result<()> {
    Ok(())
}

/// `%APPDATA%\Volatus\settings.json`; `None` when `APPDATA` is not set.
pub fn settings_path() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A unique temp folder that is removed when it drops.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "volatus-settings-test-{}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed),
                name
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn file(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn custom() -> Settings {
        Settings {
            theme: ThemeChoice::Dark,
            unit: Unit::Gb,
            show_header: false,
            show_tree: true,
            show_strip: false,
            show_chips: true,
            show_labels: false,
        }
    }

    #[test]
    fn default_settings_are_todays_behaviour() {
        let s = Settings::default();
        assert_eq!(s.theme, ThemeChoice::System);
        assert_eq!(s.unit, Unit::Dynamic);
        for p in Part::ALL {
            assert!(s.shown(p), "{p:?} should be shown by default");
        }
        assert!(s.show_header && s.show_tree && s.show_strip && s.show_chips && s.show_labels);
    }

    #[test]
    fn parse_roundtrips_to_json() {
        let s = custom();
        assert_eq!(parse(&to_json(&s)), (s, Loaded::Ok));
    }

    #[test]
    fn parse_empty_text_is_damaged_and_default() {
        assert_eq!(parse(""), (Settings::default(), Loaded::Damaged));
        assert_eq!(parse("  \n"), (Settings::default(), Loaded::Damaged));
    }

    #[test]
    fn parse_not_json_is_damaged() {
        assert_eq!(parse("{ theme: dark"), (Settings::default(), Loaded::Damaged));
    }

    #[test]
    fn parse_json_that_is_not_an_object_is_damaged() {
        assert_eq!(parse("[1,2]"), (Settings::default(), Loaded::Damaged));
        assert_eq!(parse("\"dark\""), (Settings::default(), Loaded::Damaged));
    }

    #[test]
    fn parse_wrong_type_in_one_field_keeps_the_others() {
        let (s, loaded) = parse(r#"{"theme":"dark","unit":7,"show_tree":false}"#);
        assert_eq!(s.theme, ThemeChoice::Dark);
        assert_eq!(s.unit, Unit::Dynamic);
        assert!(!s.show_tree);
        assert!(s.show_header && s.show_strip && s.show_chips && s.show_labels);
        assert_eq!(loaded, Loaded::Partial(vec!["unit"]));
    }

    #[test]
    fn parse_unknown_value_names_the_field() {
        let (s, loaded) = parse(r#"{"theme":"purple"}"#);
        assert_eq!(s.theme, ThemeChoice::System);
        assert_eq!(loaded, Loaded::Partial(vec!["theme"]));
    }

    #[test]
    fn parse_unknown_field_is_ignored() {
        let (s, loaded) = parse(r#"{"theme":"light","future_thing":42}"#);
        assert_eq!(s.theme, ThemeChoice::Light);
        assert_eq!(loaded, Loaded::Ok);
    }

    #[test]
    fn parse_older_file_without_a_field_uses_its_default() {
        let (s, loaded) = parse(r#"{"theme":"dark"}"#);
        assert_eq!(s.theme, ThemeChoice::Dark);
        assert_eq!(s.unit, Unit::Dynamic);
        assert!(s.show_header && s.show_tree && s.show_strip && s.show_chips && s.show_labels);
        assert_eq!(loaded, Loaded::Ok);
    }

    #[test]
    fn parse_utf8_bom_is_stripped() {
        let (s, loaded) = parse("\u{feff}{\"theme\":\"dark\"}");
        assert_eq!(s.theme, ThemeChoice::Dark);
        assert_eq!(loaded, Loaded::Ok);
    }

    #[test]
    fn load_missing_file_is_fresh_and_default() {
        let dir = TempDir::new("missing");
        assert_eq!(load(&dir.file()), (Settings::default(), Loaded::Fresh));
        assert!(!dir.0.join("settings.json.bad").exists());
        assert!(Settings::default().show_header);
    }

    #[test]
    fn load_damaged_file_is_kept_as_bad() {
        let dir = TempDir::new("damaged");
        fs::write(dir.file(), "not json at all").unwrap();
        assert_eq!(load(&dir.file()), (Settings::default(), Loaded::Damaged));
        let bad = dir.0.join("settings.json.bad");
        assert!(bad.exists(), "settings.json.bad should exist");
        assert_eq!(fs::read_to_string(bad).unwrap(), "not json at all");
    }

    #[test]
    fn load_partial_file_is_kept_as_bad() {
        let dir = TempDir::new("partial");
        let text = r#"{"theme":"dark","unit":7}"#;
        fs::write(dir.file(), text).unwrap();
        let (s, loaded) = load(&dir.file());
        assert_eq!(s.theme, ThemeChoice::Dark);
        assert_eq!(loaded, Loaded::Partial(vec!["unit"]));
        let bad = dir.0.join("settings.json.bad");
        assert!(bad.exists(), "settings.json.bad should exist");
        assert_eq!(fs::read_to_string(bad).unwrap(), text);
    }

    #[test]
    fn save_then_load_returns_the_same() {
        let dir = TempDir::new("roundtrip");
        let s = custom();
        save(&dir.file(), &s).unwrap();
        assert_eq!(load(&dir.file()), (s, Loaded::Ok));
        assert!(!dir.0.join("settings.json.tmp").exists(), "the temp file should be gone");
    }

    #[test]
    fn save_creates_the_folder() {
        let dir = TempDir::new("mkdir");
        let path = dir.0.join("a").join("b").join("settings.json");
        save(&path, &custom()).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn save_replaces_an_existing_file() {
        let dir = TempDir::new("replace");
        save(&dir.file(), &Settings::default()).unwrap();
        save(&dir.file(), &custom()).unwrap();
        assert_eq!(load(&dir.file()), (custom(), Loaded::Ok));
    }

    #[test]
    fn save_failure_leaves_the_old_file() {
        let dir = TempDir::new("fail");
        let old = "{\"theme\":\"light\"}";
        fs::write(dir.file(), old).unwrap();
        // A directory where the temp file goes: the temp write fails.
        fs::create_dir(dir.0.join("settings.json.tmp")).unwrap();
        assert!(save(&dir.file(), &custom()).is_err());
        assert_eq!(fs::read_to_string(dir.file()).unwrap(), old);

        // A directory as the target: the rename fails and the folder content stays.
        let target = TempDir::new("fail-dir");
        fs::create_dir(target.file()).unwrap();
        fs::write(target.file().join("keep"), "x").unwrap();
        assert!(save(&target.file(), &custom()).is_err());
        assert!(target.file().join("keep").is_file());

        // A file where the folder should be: the folder cannot be made.
        let blocker = TempDir::new("fail-folder");
        fs::write(blocker.0.join("sub"), "x").unwrap();
        assert!(save(&blocker.0.join("sub").join("settings.json"), &custom()).is_err());
    }

    #[test]
    fn forced_dark_maps_each_choice() {
        assert_eq!(ThemeChoice::System.forced_dark(), None);
        assert_eq!(ThemeChoice::Light.forced_dark(), Some(false));
        assert_eq!(ThemeChoice::Dark.forced_dark(), Some(true));
    }

    #[test]
    fn theme_index_roundtrips_and_unknown_is_system() {
        for t in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::from_index(t.index()), t);
        }
        assert_eq!(ThemeChoice::Dark.index(), 2);
        assert_eq!(ThemeChoice::from_index(-1), ThemeChoice::System);
        assert_eq!(ThemeChoice::from_index(99), ThemeChoice::System);
    }

    #[test]
    fn set_shown_and_shown_agree_for_every_part() {
        for p in Part::ALL {
            let mut s = Settings::default();
            s.set_shown(p, false);
            assert!(!s.shown(p));
            for q in Part::ALL.into_iter().filter(|&q| q != p) {
                assert!(s.shown(q), "{q:?} should not change when {p:?} is hidden");
            }
            s.set_shown(p, true);
            assert!(s.shown(p));
        }
    }
}

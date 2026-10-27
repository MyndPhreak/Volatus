//! The user's settings: the values, the tolerant JSON load and the atomic save.
//! The file is `%APPDATA%\Volatus\settings.json`. No test touches the real one.

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
    pub fn from_index(i: i32) -> ThemeChoice {
        usize::try_from(i).ok().and_then(|i| ThemeChoice::ALL.get(i).copied()).unwrap_or_default()
    }

    /// The position of this choice in `ALL`.
    pub fn index(self) -> i32 {
        ThemeChoice::ALL.iter().position(|&t| t == self).unwrap_or(0) as i32
    }

    /// `None` follows Windows; `Some(dark)` forces the scheme.
    pub fn forced_dark(self) -> Option<bool> {
        match self {
            ThemeChoice::System => None,
            ThemeChoice::Light => Some(false),
            ThemeChoice::Dark => Some(true),
        }
    }

    fn key(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
    }

    fn from_key(s: &str) -> Option<ThemeChoice> {
        ThemeChoice::ALL.into_iter().find(|t| t.key() == s)
    }
}

/// Which view of the scan the window shows. The last choice is remembered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Treemap,
    Graph,
}

impl View {
    fn key(self) -> &'static str {
        match self {
            View::Treemap => "treemap",
            View::Graph => "graph",
        }
    }

    fn from_key(s: &str) -> Option<View> {
        [View::Treemap, View::Graph].into_iter().find(|v| v.key() == s)
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
    /// The order of the window's `show-*` properties and of `part-toggled`'s index.
    pub const ALL: [Part; 5] = [Part::Header, Part::Tree, Part::Strip, Part::Chips, Part::Labels];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub unit: Unit,
    pub view: View,
    pub show_header: bool,
    pub show_tree: bool,
    pub show_strip: bool,
    pub show_chips: bool,
    pub show_labels: bool,
    pub always_admin: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::System,
            unit: Unit::Dynamic,
            view: View::Treemap,
            show_header: true,
            show_tree: true,
            show_strip: true,
            show_chips: true,
            show_labels: true,
            always_admin: false,
        }
    }
}

impl Settings {
    pub fn shown(&self, p: Part) -> bool {
        match p {
            Part::Header => self.show_header,
            Part::Tree => self.show_tree,
            Part::Strip => self.show_strip,
            Part::Chips => self.show_chips,
            Part::Labels => self.show_labels,
        }
    }

    pub fn set_shown(&mut self, p: Part, on: bool) {
        match p {
            Part::Header => self.show_header = on,
            Part::Tree => self.show_tree = on,
            Part::Strip => self.show_strip = on,
            Part::Chips => self.show_chips = on,
            Part::Labels => self.show_labels = on,
        }
    }
}

fn unit_key(u: Unit) -> &'static str {
    match u {
        Unit::Dynamic => "dynamic",
        Unit::Byte => "byte",
        Unit::Kb => "kb",
        Unit::Mb => "mb",
        Unit::Gb => "gb",
        Unit::Tb => "tb",
    }
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
    /// The file exists but was not read cleanly, and no `.bad` copy of it exists (the copy
    /// failed, or the file could not be read at all). Settings are the defaults. The host must
    /// not save while this holds, because a save would overwrite the only copy of the file.
    NotKept,
}

/// Reads settings text. Pure. A leading UTF-8 BOM is stripped.
pub fn parse(text: &str) -> (Settings, Loaded) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text) else {
        return (Settings::default(), Loaded::Damaged);
    };
    let mut s = Settings::default();
    let mut bad: Vec<&'static str> = Vec::new();

    // A missing key keeps the default; a present key of the wrong type or value is listed.
    match map.get("theme") {
        None => {}
        Some(v) => match v.as_str().and_then(ThemeChoice::from_key) {
            Some(t) => s.theme = t,
            None => bad.push("theme"),
        },
    }
    match map.get("unit") {
        None => {}
        Some(v) => match v.as_str().and_then(|k| Unit::ALL.into_iter().find(|&u| unit_key(u) == k)) {
            Some(u) => s.unit = u,
            None => bad.push("unit"),
        },
    }
    for (key, part) in [
        ("show_header", Part::Header),
        ("show_tree", Part::Tree),
        ("show_strip", Part::Strip),
        ("show_chips", Part::Chips),
        ("show_labels", Part::Labels),
    ] {
        match map.get(key) {
            None => {}
            Some(v) => match v.as_bool() {
                Some(on) => s.set_shown(part, on),
                None => bad.push(key),
            },
        }
    }

    match map.get("always_admin") {
        None => {}
        Some(v) => match v.as_bool() {
            Some(on) => s.always_admin = on,
            None => bad.push("always_admin"),
        },
    }

    let loaded = if bad.is_empty() { Loaded::Ok } else { Loaded::Partial(bad) };
    (s, loaded)
}

pub fn to_json(s: &Settings) -> String {
    let v = serde_json::json!({
        "theme": s.theme.key(),
        "unit": unit_key(s.unit),
        "view": s.view.key(),
        "show_header": s.show_header,
        "show_tree": s.show_tree,
        "show_strip": s.show_strip,
        "show_chips": s.show_chips,
        "show_labels": s.show_labels,
        "always_admin": s.always_admin,
    });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

/// `<path>` with `suffix` added to the file name: `settings.json` -> `settings.json.bad`.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Reads the file. On `Partial` and `Damaged` the file is first copied to `<name>.bad`, so a
/// later save cannot destroy what the user wrote. `Partial` and `Damaged` are returned only
/// when that copy was written; if it fails, or the file cannot be read, the result is
/// `NotKept` with default settings.
pub fn load(path: &Path) -> (Settings, Loaded) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Settings::default(), Loaded::Fresh),
        // Present but unreadable (a folder, no access): there is nothing to copy.
        Err(_) => return (Settings::default(), Loaded::NotKept),
    };
    let (settings, loaded) = match std::str::from_utf8(&bytes) {
        Ok(text) => parse(text),
        Err(_) => (Settings::default(), Loaded::Damaged),
    };
    if matches!(loaded, Loaded::Partial(_) | Loaded::Damaged)
        && std::fs::write(with_suffix(path, ".bad"), &bytes).is_err()
    {
        return (Settings::default(), Loaded::NotKept);
    }
    (settings, loaded)
}

/// Writes `<name>.tmp` beside the file, then renames it over the file. On any failure the old
/// file is left as it was and the temp file is removed.
pub fn save(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = with_suffix(path, ".tmp");
    let result = std::fs::write(&tmp, to_json(s)).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `%APPDATA%\Volatus\settings.json`; `None` when `APPDATA` is not set.
pub fn settings_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(appdata).join("Volatus").join("settings.json"))
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
            view: View::Graph,
            show_header: false,
            show_tree: true,
            show_strip: false,
            show_chips: true,
            show_labels: false,
            always_admin: true,
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
        assert!(!s.always_admin);
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
        assert!(!s.always_admin);
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
        assert!(!s.always_admin);
        assert_eq!(loaded, Loaded::Ok);
    }

    #[test]
    fn parse_always_admin_roundtrips_and_validates_type() {
        let (s, loaded) = parse(r#"{"always_admin":true}"#);
        assert!(s.always_admin);
        assert_eq!(loaded, Loaded::Ok);

        let (s, loaded) = parse(r#"{"always_admin":"yes"}"#);
        assert!(!s.always_admin);
        assert_eq!(loaded, Loaded::Partial(vec!["always_admin"]));
    }

    #[test]
    fn view_defaults_to_treemap_when_missing() {
        let (s, loaded) = parse(r#"{"theme":"dark"}"#);
        assert_eq!(s.view, View::Treemap);
        assert_eq!(loaded, Loaded::Ok);
    }

    #[test]
    fn parse_reads_both_views() {
        assert_eq!(parse(r#"{"view":"graph"}"#), (Settings { view: View::Graph, ..Settings::default() }, Loaded::Ok));
        assert_eq!(parse(r#"{"view":"treemap"}"#), (Settings::default(), Loaded::Ok));
    }

    #[test]
    fn parse_bad_view_falls_back_and_names_the_field() {
        for text in [r#"{"view":"Graph"}"#, r#"{"view":1}"#, r#"{"view":null}"#] {
            let (s, loaded) = parse(text);
            assert_eq!(s.view, View::Treemap, "{text}");
            assert_eq!(loaded, Loaded::Partial(vec!["view"]), "{text}");
        }
    }

    #[test]
    fn view_roundtrips_to_json() {
        for view in [View::Treemap, View::Graph] {
            let s = Settings { view, ..Settings::default() };
            assert_eq!(parse(&to_json(&s)), (s, Loaded::Ok));
        }
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
    }

    #[test]
    fn load_damaged_file_whose_bad_copy_fails_is_not_kept() {
        let dir = TempDir::new("damaged-nokeep");
        fs::write(dir.file(), "not json at all").unwrap();
        fs::create_dir(dir.0.join("settings.json.bad")).unwrap();
        assert_eq!(load(&dir.file()), (Settings::default(), Loaded::NotKept));
        assert_eq!(fs::read_to_string(dir.file()).unwrap(), "not json at all");
    }

    #[test]
    fn load_partial_file_whose_bad_copy_fails_is_not_kept() {
        let dir = TempDir::new("partial-nokeep");
        let text = r#"{"theme":"dark","unit":7}"#;
        fs::write(dir.file(), text).unwrap();
        fs::create_dir(dir.0.join("settings.json.bad")).unwrap();
        assert_eq!(load(&dir.file()), (Settings::default(), Loaded::NotKept));
        assert_eq!(fs::read_to_string(dir.file()).unwrap(), text);
    }

    #[test]
    fn load_unreadable_existing_file_is_not_kept() {
        let dir = TempDir::new("unreadable");
        fs::create_dir(dir.file()).unwrap();
        assert_eq!(load(&dir.file()), (Settings::default(), Loaded::NotKept));
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
        assert!(!s.always_admin);
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

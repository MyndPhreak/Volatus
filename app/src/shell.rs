//! The Windows shell menu of an item: COM, `IContextMenu`/2/3, the popup and its owner window.
//! Exempt from test-first except the menu build (the tests below); the decisions it uses are in
//! `menu`. Choices from the spike (`.claude/work/storagetool-wiztree-clone/logs/CM-spike.md`).

use crate::menu::Item;
use std::path::Path;
use windows::Win32::Foundation::HWND;

/// A popup menu: the Volatus items above the shell items of one path.
pub struct Popup {}

impl Popup {
    /// The menu for `path` (`None`: no shell items) with the shell flags `flags`, owned by `owner`.
    /// `items` gets whether the shell menu was built and gives the Volatus items, put above it.
    pub fn new(owner: HWND, path: Option<&Path>, flags: u32, items: impl FnOnce(bool) -> Vec<Item>) -> windows::core::Result<Self> {
        let _ = (owner, path, flags, items);
        unimplemented!("Popup::new")
    }

    /// The verb of each shell item of the top level, in menu order (`None`: the shell gave none).
    pub fn verbs(&self) -> Vec<Option<String>> {
        unimplemented!("Popup::verbs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::query_flags;
    use std::fs;
    use std::path::PathBuf;

    /// A new empty folder under the target folder of the worktree, removed when dropped. The shell
    /// builds menus only for items made here: no verb runs.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/shell-tests").join(format!("{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The verbs of the shell menu of `path`, with Shift or not.
    fn verbs_of(path: &Path, shift: bool) -> Vec<Option<String>> {
        let popup = Popup::new(HWND::default(), Some(path), query_flags(shift), |_| Vec::new()).unwrap();
        popup.verbs()
    }

    fn has(verbs: &[Option<String>], verb: &str) -> bool {
        verbs.iter().flatten().any(|v| v.eq_ignore_ascii_case(verb))
    }

    #[test]
    fn popup_of_a_file_has_delete_and_properties_and_no_rename() {
        let dir = Scratch::new("file");
        let file = dir.0.join("f.txt");
        fs::write(&file, b"hello").unwrap();

        let verbs = verbs_of(&file, false);

        assert_eq!([has(&verbs, "delete"), has(&verbs, "properties"), has(&verbs, "rename")], [true, true, false], "{verbs:?}");
    }

    #[test]
    fn popup_of_a_folder_has_delete() {
        let dir = Scratch::new("folder");
        let folder = dir.0.join("sub");
        fs::create_dir(&folder).unwrap();

        let verbs = verbs_of(&folder, false);

        assert!(has(&verbs, "delete"), "{verbs:?}");
    }

    #[test]
    fn popup_of_a_folder_with_shift_has_at_least_the_items_without() {
        let dir = Scratch::new("folder-shift");
        let folder = dir.0.join("sub");
        fs::create_dir(&folder).unwrap();

        let (plain, extended) = (verbs_of(&folder, false), verbs_of(&folder, true));

        assert!(extended.len() >= plain.len() && !plain.is_empty(), "plain {plain:?}, shift {extended:?}");
    }
}

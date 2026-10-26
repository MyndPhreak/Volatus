//! File-type footer: the chips for the biggest file types. Plain functions, no Slint types.

use crate::model::{Unit, display_name, format_size};
use crate::tree_view::{format_percent, spoken_percent};
use std::collections::HashMap;
use treemap::filetype::{Category, TypeTotal, category_of_ext};

/// One chip of the footer: a file type, its share of the scan and its tile category.
#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    /// ".dll", "No extension" or "Other".
    pub label: String,
    /// Share of `total` as `format_percent` gives it: "10.3%".
    pub percent: String,
    /// Share of `total`, 0..=1: the width of the chip's part of the strip.
    pub share: f32,
    /// Bytes of the type as `format_size` gives them: "12.3 GB".
    pub size: String,
    /// Gives the colour, the same as the tiles of this type.
    pub category: Category,
}

/// Label of the chip of the files with no extension.
pub const NO_EXTENSION: &str = "No extension";
/// Label of the chip that holds every type after the first `max`.
pub const OTHER: &str = "Other";

/// The chips of `totals` out of `total` bytes, the size of the scanned folder: the `max` types
/// with the most bytes (ties: ext ascending, as `type_totals` sorts), then one "Other" chip for
/// what is left of `total`, if anything. So the shares add up to 1. Empty when `total` is 0.
pub fn chips(totals: &[TypeTotal], total: u64, max: usize) -> Vec<Chip> {
    if total == 0 {
        return Vec::new();
    }
    let chip = |label: String, bytes: u64, category| Chip {
        label,
        percent: format_percent(bytes, total),
        share: (bytes as f64 / total as f64) as f32,
        size: format_size(bytes, Unit::Dynamic),
        category,
    };
    let mut top: Vec<&TypeTotal> = totals.iter().filter(|t| t.bytes > 0).collect();
    top.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.ext.cmp(&b.ext)));
    top.truncate(max);
    let rest = total.saturating_sub(top.iter().map(|t| t.bytes).sum());
    let mut out: Vec<Chip> = top
        .iter()
        .map(|t| {
            let label = if t.ext.is_empty() { NO_EXTENSION.to_string() } else { format!(".{}", display_name(&t.ext)) };
            chip(label, t.bytes, category_of_ext(&t.ext))
        })
        .collect();
    if rest > 0 {
        out.push(chip(OTHER.to_string(), rest, Category::Other));
    }
    out
}

/// `totals` less the totals `gone` of a removed subtree, per extension: bytes and files go down;
/// an extension left with no file is dropped. Sorted as `type_totals` sorts: bytes descending,
/// then ext ascending.
pub fn subtract(totals: &[TypeTotal], gone: &[TypeTotal]) -> Vec<TypeTotal> {
    let gone: HashMap<&str, (u64, u64)> = gone.iter().map(|g| (g.ext.as_str(), (g.bytes, g.files))).collect();
    let mut left: Vec<TypeTotal> = totals
        .iter()
        .filter_map(|t| {
            let (bytes, files) = gone.get(t.ext.as_str()).copied().unwrap_or_default();
            let files = t.files.saturating_sub(files);
            (files > 0).then(|| TypeTotal { ext: t.ext.clone(), bytes: t.bytes.saturating_sub(bytes), files })
        })
        .collect();
    left.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.ext.cmp(&b.ext)));
    left
}

/// What a screen reader says for chip `c`: ".dll, 10.5 percent, 12.3 GB".
pub fn spoken(c: &Chip) -> String {
    format!("{}, {}, {}", c.label, spoken_percent(&c.percent), c.size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan::rollup::{RawEntry, rollup};
    use treemap::filetype::type_totals;

    fn total(ext: &str, bytes: u64) -> TypeTotal {
        TypeTotal { ext: ext.into(), bytes, files: 1 }
    }

    /// Ten types "t1" .. "t10" of 100, 90, .. 10 bytes: 550 in all.
    fn ten() -> Vec<TypeTotal> {
        (1..=10u64).map(|i| total(&format!("t{i}"), 110 - 10 * i)).collect()
    }

    fn labels(chips: &[Chip]) -> Vec<&str> {
        chips.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn chips_more_types_than_max_gives_max_chips_then_other() {
        let chips = chips(&ten(), 550, 8);

        assert_eq!(labels(&chips), vec![".t1", ".t2", ".t3", ".t4", ".t5", ".t6", ".t7", ".t8", OTHER]);
    }

    #[test]
    fn chips_other_holds_the_bytes_of_the_rest() {
        let chips = chips(&ten(), 550, 8);

        // t9 + t10 = 20 + 10 bytes of 550.
        assert_eq!(chips[8].percent, format_percent(30, 550));
    }

    #[test]
    fn chips_all_types_fit_gives_no_other() {
        let chips = chips(&ten(), 550, 10);

        assert_eq!(chips.len(), 10);
        assert!(chips.iter().all(|c| c.label != OTHER), "{:?}", labels(&chips));
    }

    #[test]
    fn chips_label_of_an_extension_with_a_hidden_character_shows_a_placeholder() {
        let chips = chips(&[total("a\u{202E}b", 10)], 10, 8);
        assert_eq!(labels(&chips), vec![".a\u{FFFD}b"]);
    }

    #[test]
    fn chips_label_is_the_extension_with_a_dot() {
        let chips = chips(&[total("dll", 5)], 5, 8);

        assert_eq!(labels(&chips), vec![".dll"]);
    }

    #[test]
    fn chips_empty_extension_is_no_extension() {
        let chips = chips(&[total("dll", 5), total("", 3)], 8, 8);

        assert_eq!(labels(&chips), vec![".dll", NO_EXTENSION]);
    }

    #[test]
    fn chips_percent_is_format_percent_of_the_total() {
        let chips = chips(&[total("dll", 103), total("exe", 897)], 1000, 8);

        let percents: Vec<&str> = chips.iter().map(|c| c.percent.as_str()).collect();

        assert_eq!(percents, vec!["89.7%", "10.3%"]);
    }

    #[test]
    fn chips_percent_rounds_half_up_to_one_decimal() {
        // 1 of 2000 = 0.05%.
        let chips = chips(&[total("big", 1999), total("small", 1)], 2000, 8);

        assert_eq!(chips[1].percent, "0.1%");
    }

    #[test]
    fn chips_three_equal_types_each_show_33_3_percent() {
        // Each chip is rounded on its own, so the shown values may add up to 99.9%.
        let chips = chips(&[total("a", 1), total("b", 1), total("c", 1)], 3, 8);

        let percents: Vec<&str> = chips.iter().map(|c| c.percent.as_str()).collect();

        assert_eq!(percents, vec!["33.3%"; 3]);
    }

    #[test]
    fn chips_shares_sum_to_one_with_other() {
        let chips = chips(&ten(), 550, 8);

        let sum: f64 = chips.iter().map(|c| f64::from(c.share)).sum();

        assert!((sum - 1.0).abs() <= 1e-6, "sum {sum}");
    }

    #[test]
    fn chips_shares_sum_to_one_without_other() {
        let chips = chips(&ten(), 550, 10);

        let sum: f64 = chips.iter().map(|c| f64::from(c.share)).sum();

        assert!((sum - 1.0).abs() <= 1e-6, "sum {sum}");
    }

    #[test]
    fn chips_total_zero_is_empty() {
        assert_eq!(chips(&[total("txt", 0)], 0, 8), vec![]);
    }

    #[test]
    fn chips_no_types_with_bytes_is_empty() {
        assert_eq!(chips(&[], 0, 8), vec![]);
    }

    #[test]
    fn chips_one_type_is_one_chip_of_100_percent() {
        let chips = chips(&[total("iso", 42)], 42, 8);

        assert_eq!(
            chips,
            vec![Chip { label: ".iso".into(), percent: "100.0%".into(), share: 1.0, size: "42 B".into(), category: Category::DiskImage }]
        );
    }

    #[test]
    fn chips_type_with_zero_bytes_gets_no_chip() {
        let chips = chips(&[total("exe", 9), total("txt", 0)], 9, 8);

        assert_eq!(labels(&chips), vec![".exe"]);
    }

    #[test]
    fn chips_max_zero_gives_only_other() {
        let chips = chips(&ten(), 550, 0);

        assert_eq!((labels(&chips), chips[0].percent.as_str()), (vec![OTHER], "100.0%"));
    }

    #[test]
    fn chips_category_follows_the_extension_and_other_is_grey() {
        let chips = chips(&[total("dll", 5), total("", 4), total("mp4", 3)], 14, 2);

        let cats: Vec<Category> = chips.iter().map(|c| c.category).collect();

        assert_eq!(cats, vec![Category::Program, Category::Other, Category::Other]);
    }

    #[test]
    fn chips_size_is_format_size_of_the_bytes_of_the_type() {
        let chips = chips(&[total("dll", 3 * 1024 * 1024), total("exe", 1024)], 3 * 1024 * 1024 + 1024, 8);

        let sizes: Vec<&str> = chips.iter().map(|c| c.size.as_str()).collect();

        assert_eq!(sizes, vec!["3.0 MB", "1.0 KB"]);
    }

    #[test]
    fn chips_size_of_other_is_the_bytes_of_the_rest() {
        let chips = chips(&ten(), 550, 8);

        assert_eq!(chips[8].size, "30 B");
    }

    fn counted(ext: &str, bytes: u64, files: u64) -> TypeTotal {
        TypeTotal { ext: ext.into(), bytes, files }
    }

    #[test]
    fn subtract_lowers_bytes_and_files_of_each_extension_and_drops_one_left_with_no_file() {
        let totals = [counted("dll", 100, 3), counted("exe", 50, 2), counted("txt", 10, 1)];
        let gone = [counted("dll", 40, 1), counted("txt", 10, 1)];

        let left = subtract(&totals, &gone);

        assert_eq!(left, vec![counted("dll", 60, 2), counted("exe", 50, 2)]);
    }

    #[test]
    fn subtract_extension_left_with_files_of_no_size_is_kept() {
        let left = subtract(&[counted("log", 5, 2)], &[counted("log", 5, 1)]);

        assert_eq!(left, vec![counted("log", 0, 1)]);
    }

    #[test]
    fn subtract_result_keeps_bytes_descending_then_ext_ascending() {
        let totals = [counted("a", 100, 2), counted("c", 90, 1), counted("b", 90, 2)];

        let left = subtract(&totals, &[counted("a", 20, 1)]);

        assert_eq!(left, vec![counted("b", 90, 2), counted("c", 90, 1), counted("a", 80, 1)]);
    }

    #[test]
    fn spoken_chip_gives_label_percent_in_words_and_size() {
        let chips = chips(&[total("dll", 103), total("exe", 897)], 1000, 8);

        assert_eq!(spoken(&chips[1]), ".dll, 10.3 percent, 103 B");
    }

    #[test]
    fn spoken_other_and_no_extension_chips_use_their_labels() {
        let chips = chips(&[total("dll", 5), total("", 3), total("exe", 2)], 10, 2);

        let said: Vec<String> = chips.iter().map(spoken).collect();

        assert_eq!(said, vec![".dll, 50.0 percent, 5 B", "No extension, 30.0 percent, 3 B", "Other, 20.0 percent, 2 B"]);
    }

    /// Files `(name, size)` directly under a root folder.
    fn files(list: &[(&str, u64)]) -> scan::Tree {
        let mut entries = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true }];
        for (i, &(name, size)) in list.iter().enumerate() {
            entries.push(RawEntry { id: i as u64 + 1, parent: 0, name: name.into(), size, allocated: size, is_dir: false });
        }
        rollup(0, entries).unwrap().tree
    }

    #[test]
    fn chips_of_unusual_names_group_by_their_last_extension_in_lower_case() {
        let tree = files(&[
            ("Makefile", 1),
            (".gitignore", 1),
            ("x.tar.gz", 40),
            ("SETUP.EXE", 20),
            ("setup.exe", 10),
            ("a.abcdefghijklm", 1),
            ("日本.txt", 8),
            ("x.ÄBC", 1),
            ("trailing.txt ", 1),
            ("trailing.txt.", 1),
        ]);
        let root = tree.root();

        let chips = chips(&type_totals(&tree, root), tree.node(root).size, 8);

        let rows: Vec<(&str, &str)> = chips.iter().map(|c| (c.label.as_str(), c.percent.as_str())).collect();
        assert_eq!(rows, vec![(".gz", "47.6%"), (".exe", "35.7%"), (".txt", "9.5%"), (NO_EXTENSION, "7.1%")]);
    }
}

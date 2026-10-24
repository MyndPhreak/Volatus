use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};
use treemap::filetype::{Category, category_of, colour, extension, type_totals};
use treemap::{Chrome, Rect, Theme, layout_framed, render_framed};

/// Files `(name, size)` directly under a root folder.
fn files(list: &[(&str, u64)]) -> Tree {
    let mut entries = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true }];
    for (i, &(name, size)) in list.iter().enumerate() {
        entries.push(RawEntry { id: i as u64 + 1, parent: 0, name: name.into(), size, allocated: size, is_dir: false });
    }
    rollup(0, entries).unwrap().tree
}

fn named(tree: &Tree, name: &str) -> NodeId {
    tree.children(tree.root()).find(|&c| tree.name(c) == name).unwrap_or_else(|| panic!("no node {name}"))
}

// ---- extension ----

#[test]
fn extension_table_of_names_gives_the_expected_extension() {
    let cases: [(&str, Option<&str>); 21] = [
        ("a.TXT", Some("TXT")),
        ("x.tar.gz", Some("gz")),
        ("noext", None),
        (".gitignore", None),
        ("file.", None),
        ("a.abcdefghijklm", None),
        ("a.abcdefghijkl", Some("abcdefghijkl")),
        ("p.jp g", None),
        ("日本.txt", Some("txt")),
        ("x.ÄBC", None),
        ("a.b-c", None),
        ("a.txt ", None),
        ("a.txt.", None),
        ("", None),
        (".", None),
        ("..", None),
        (".config.json", Some("json")),
        ("a.7z", Some("7z")),
        ("a.x", Some("x")),
        ("a..b", Some("b")),
        ("archive.TAR.GZ", Some("GZ")),
    ];

    let wrong: Vec<String> = cases
        .iter()
        .filter(|&&(name, want)| extension(name) != want)
        .map(|&(name, want)| format!("{name:?}: got {:?}, want {want:?}", extension(name)))
        .collect();

    assert!(wrong.is_empty(), "{wrong:#?}");
}

// ---- category_of ----

/// One known extension per category, and one name per category with no known extension.
const KNOWN: [(&str, Category); 10] = [
    ("movie.mp4", Category::Video),
    ("photo.jpg", Category::Image),
    ("song.mp3", Category::Audio),
    ("backup.zip", Category::Archive),
    ("setup.exe", Category::Program),
    ("report.pdf", Category::Document),
    ("main.rs", Category::Code),
    ("windows.iso", Category::DiskImage),
    ("pakchunk0.pak", Category::GameData),
    ("index.db", Category::Data),
];

#[test]
fn category_of_one_known_extension_per_category_gives_that_category() {
    let wrong: Vec<String> = KNOWN
        .iter()
        .filter(|&&(name, want)| category_of(name) != want)
        .map(|&(name, want)| format!("{name}: got {:?}, want {want:?}", category_of(name)))
        .collect();

    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn category_of_upper_case_name_is_the_same_as_lower_case() {
    let wrong: Vec<String> = KNOWN
        .iter()
        .map(|&(name, _)| name.to_uppercase())
        .filter(|upper| category_of(upper) != category_of(&upper.to_lowercase()))
        .collect();

    assert!(wrong.is_empty(), "{wrong:?}");
}

#[test]
fn category_of_unknown_extension_is_other() {
    assert_eq!(category_of("notes.qqqzz"), Category::Other);
}

#[test]
fn category_of_name_with_no_extension_is_other() {
    let names = ["Makefile", ".gitignore", "file.", "a.txt ", "x.ÄBC"];

    let cats: Vec<Category> = names.iter().map(|n| category_of(n)).collect();

    assert_eq!(cats, vec![Category::Other; names.len()]);
}

// ---- colour ----

fn distinct_colours(theme: Theme) -> Vec<String> {
    let mut clashes = Vec::new();
    for (i, &a) in Category::ALL.iter().enumerate() {
        for &b in &Category::ALL[i + 1..] {
            if colour(a, theme) == colour(b, theme) {
                clashes.push(format!("{a:?} and {b:?}: {:?}", colour(a, theme)));
            }
        }
    }
    clashes
}

#[test]
fn colour_every_category_is_distinct_in_dark() {
    assert!(distinct_colours(Theme::Dark).is_empty(), "{:?}", distinct_colours(Theme::Dark));
}

#[test]
fn colour_every_category_is_distinct_in_light() {
    assert!(distinct_colours(Theme::Light).is_empty(), "{:?}", distinct_colours(Theme::Light));
}

#[test]
fn colour_other_is_grey_in_both_themes() {
    let greys: Vec<bool> = [Theme::Dark, Theme::Light]
        .iter()
        .map(|&t| {
            let [r, g, b] = colour(Category::Other, t);
            r == g && g == b
        })
        .collect();

    assert_eq!(greys, vec![true, true]);
}

#[test]
fn colour_dark_program_is_the_plan_blue() {
    assert_eq!(colour(Category::Program, Theme::Dark), [0x3b, 0x8f, 0xe6]);
}

/// WCAG relative luminance of an sRGB colour.
fn luminance(c: [u8; 3]) -> f64 {
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Categories other than `Other` whose colour has less than 3:1 contrast with `backdrop`.
fn low_contrast(theme: Theme, backdrop: [u8; 3]) -> Vec<String> {
    Category::ALL
        .iter()
        .filter(|&&c| c != Category::Other)
        .map(|&c| (c, contrast(colour(c, theme), backdrop)))
        .filter(|&(_, k)| k < 3.0)
        .map(|(c, k)| format!("{c:?} {k:.2}:1"))
        .collect()
}

#[test]
fn colour_dark_has_3_to_1_contrast_with_the_dark_backdrop() {
    let low = low_contrast(Theme::Dark, [0x20, 0x20, 0x20]);

    assert!(low.is_empty(), "{low:?}");
}

#[test]
fn colour_light_has_3_to_1_contrast_with_the_light_backdrop() {
    let low = low_contrast(Theme::Light, [0xf3, 0xf3, 0xf3]);

    assert!(low.is_empty(), "{low:?}");
}

// ---- render_framed tile colour ----

const W: u32 = 400;
const H: u32 = 300;
const ONE: Chrome = Chrome { header: 16.0, pad: 2.0 };

/// The plain body colour of file `name`'s tile: a pixel at 3/4 of its height, under the highlight.
fn tile_body(theme: Theme, name: &str) -> [u8; 3] {
    let tree = files(&[("clip.MP4", 600), ("setup.exe", 300), ("README", 100)]);
    let rects = layout_framed(&tree, tree.root(), W, H, ONE);
    let px = render_framed(&tree, &rects, W, H, ONE, theme);
    let n = named(&tree, name);
    let r: Rect = *rects.iter().find(|r| r.node == n).unwrap();
    let (x, y) = ((r.x + r.w / 2.0) as usize, (r.y + r.h * 0.75) as usize);
    let i = (y * W as usize + x) * 4;
    [px[i], px[i + 1], px[i + 2]]
}

#[test]
fn render_framed_video_tile_has_the_video_colour_in_dark() {
    assert_eq!(tile_body(Theme::Dark, "clip.MP4"), [0xd9, 0x4b, 0x8c]);
}

#[test]
fn render_framed_program_tile_has_the_program_colour_in_dark() {
    assert_eq!(tile_body(Theme::Dark, "setup.exe"), [0x3b, 0x8f, 0xe6]);
}

#[test]
fn render_framed_program_tile_has_the_light_program_colour_in_light() {
    assert_eq!(tile_body(Theme::Light, "setup.exe"), [0x38, 0x88, 0xdb]);
}

#[test]
fn render_framed_tile_with_no_extension_is_other_grey() {
    assert_eq!(tile_body(Theme::Dark, "README"), [0x6e, 0x6e, 0x6e]);
}

// ---- type_totals ----

/// `(ext, bytes, files)` of each total, in order.
fn totals_of(tree: &Tree, root: NodeId) -> Vec<(String, u64, u64)> {
    type_totals(tree, root).into_iter().map(|t| (t.ext, t.bytes, t.files)).collect()
}

fn row(ext: &str, bytes: u64, files: u64) -> (String, u64, u64) {
    (ext.to_string(), bytes, files)
}

#[test]
fn type_totals_sums_bytes_and_files_per_extension() {
    let tree = files(&[("a.exe", 7), ("b.dll", 10), ("c.dll", 5)]);

    assert_eq!(totals_of(&tree, tree.root()), vec![row("dll", 15, 2), row("exe", 7, 1)]);
}

#[test]
fn type_totals_upper_and_lower_case_extension_are_one_lower_case_entry() {
    let tree = files(&[("x.DLL", 4), ("y.dll", 6), ("z.Dll", 1)]);

    assert_eq!(totals_of(&tree, tree.root()), vec![row("dll", 11, 3)]);
}

#[test]
fn type_totals_names_with_no_extension_are_one_empty_entry() {
    let tree = files(&[("Makefile", 3), (".gitignore", 2), ("file.", 1), ("a.txt ", 1), ("x.ÄBC", 1), ("a.b-c", 1)]);

    assert_eq!(totals_of(&tree, tree.root()), vec![row("", 9, 6)]);
}

#[test]
fn type_totals_unusual_names_use_the_last_extension_in_lower_case() {
    let tree = files(&[("x.tar.gz", 8), ("日本.TXT", 4), ("a.abcdefghijkl", 2), ("a.abcdefghijklm", 1)]);

    assert_eq!(totals_of(&tree, tree.root()), vec![row("gz", 8, 1), row("txt", 4, 1), row("abcdefghijkl", 2, 1), row("", 1, 1)]);
}

#[test]
fn type_totals_zero_size_files_count_with_zero_bytes() {
    let tree = files(&[("a.exe", 7), ("empty.txt", 0)]);

    assert_eq!(totals_of(&tree, tree.root()), vec![row("exe", 7, 1), row("txt", 0, 1)]);
}

/// root{ pkg.zip (folder){ data.bin=5, inner.7z (folder){} }, top.exe=1 }
fn with_dotted_folders() -> Tree {
    let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
        id,
        parent,
        name: name.into(),
        size,
        allocated: size,
        is_dir,
    };
    let entries = vec![
        e(0, 0, "root", 0, true),
        e(1, 0, "pkg.zip", 0, true),
        e(2, 1, "data.bin", 5, false),
        e(3, 1, "inner.7z", 0, true),
        e(4, 0, "top.exe", 1, false),
    ];
    rollup(0, entries).unwrap().tree
}

#[test]
fn type_totals_folders_are_never_counted() {
    let tree = with_dotted_folders();

    assert_eq!(totals_of(&tree, tree.root()), vec![row("bin", 5, 1), row("exe", 1, 1)]);
}

#[test]
fn type_totals_of_a_folder_counts_only_the_files_under_it() {
    let tree = with_dotted_folders();
    let pkg = named(&tree, "pkg.zip");

    assert_eq!(totals_of(&tree, pkg), vec![row("bin", 5, 1)]);
}

#[test]
fn type_totals_sorted_by_bytes_desc_then_ext_asc() {
    let tree = files(&[("a.zz", 5), ("b.yy", 5), ("c.big", 9), ("d.aa", 1), ("e", 5)]);

    let exts: Vec<String> = type_totals(&tree, tree.root()).into_iter().map(|t| t.ext).collect();

    assert_eq!(exts, vec!["big", "", "yy", "zz", "aa"]);
}

#[test]
fn type_totals_folder_with_no_files_is_empty() {
    let tree = files(&[]);

    assert_eq!(totals_of(&tree, tree.root()), vec![]);
}

/// 6,000 folders x 1,000 files = 6,000,000 files over 2,000 extensions ("e0" to "e1999").
fn six_million_files() -> Tree {
    let mut entries = Vec::with_capacity(6_006_001);
    entries.push(RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true });
    let mut id = 1u64;
    for _ in 0..6_000 {
        let folder = id;
        entries.push(RawEntry { id, parent: 0, name: format!("folder{id}"), size: 0, allocated: 0, is_dir: true });
        id += 1;
        for _ in 0..1_000 {
            let name = format!("file{id}.E{}", id % 2_000);
            entries.push(RawEntry { id, parent: folder, name, size: id % 4_096, allocated: 0, is_dir: false });
            id += 1;
        }
    }
    rollup(0, entries).unwrap().tree
}

#[test]
#[ignore = "release timing: cargo test --release -p treemap --test filetype -- --ignored --nocapture"]
fn type_totals_six_million_files_take_under_two_seconds() {
    let tree = six_million_files();

    let started = std::time::Instant::now();
    let totals = type_totals(&tree, tree.root());
    let took = started.elapsed();
    eprintln!("type_totals of 6,000,000 files, {} extensions: {took:?}", totals.len());

    let files: u64 = totals.iter().map(|t| t.files).sum();
    assert_eq!((totals.len(), files), (2_000, 6_000_000));
    assert!(took.as_secs_f64() < 2.0, "took {took:?}");
}

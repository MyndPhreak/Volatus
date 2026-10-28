//! The folder flow graph's window-free logic: its state, clicks and keys, colours, texts and
//! the choice of the view to show. Pure; the window code lives elsewhere.
// Most of this is wired in later tasks (P3.T3, P4.T1, P4.T2).
#![cfg_attr(not(test), expect(dead_code, reason = "wired in a later task"))]
#![cfg_attr(test, allow(dead_code))]

use crate::model::{ScanState, Unit, display_name, format_size, group_digits};
use crate::tree_view::{format_percent, frame_label};
use crate::nav::{Dir, neighbour_rect};
use crate::settings::View;
use flow::age::{Age, age_colour, age_of};
use flow::{Band, Expander, Graph, Part, Theme, VISIBLE_COLUMNS, folder_rects};
use scan::{NodeId, Tree};
use std::collections::{HashMap, HashSet};
use treemap::filetype::Category;

/// What colours the bands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColourMode {
    #[default]
    Age,
    FileType,
}

/// The graph's state in one session.
#[derive(Default)]
pub struct GraphState {
    pub expanded: HashSet<NodeId>,
    pub open_other: HashSet<NodeId>,
    pub first_column: u16,
    pub colour: ColourMode,
    pub shown: Option<Graph>,
    pub types: HashMap<Part, Option<Category>>,
}

impl GraphState {
    /// Expands `n`, or collapses it when it is expanded.
    pub fn toggle(&mut self, n: NodeId) {
        if !self.expanded.remove(&n) {
            self.expanded.insert(n);
        }
    }

    /// Shows the members of `parent`'s "other" band as their own bands.
    pub fn show_other(&mut self, parent: NodeId) {
        self.open_other.insert(parent);
    }

    /// A new view folder: nothing expanded or opened, no scroll.
    pub fn reroot(&mut self) {
        self.expanded.clear();
        self.open_other.clear();
        self.first_column = 0;
    }

    /// After a removal: drops every id the removed subtree holds (`inside`), the shown graph
    /// (its bands may name removed folders) and the file-type cache (its `Other` keys count
    /// members of the old tree).
    pub fn after_detach(&mut self, inside: impl Fn(NodeId) -> bool) {
        self.expanded.retain(|&n| !inside(n));
        self.open_other.retain(|&n| !inside(n));
        self.shown = None;
        self.types.clear();
    }

    /// Scrolls the least that puts `column` in view, within the `columns` the graph has.
    pub fn scroll_to(&mut self, column: u16, columns: u16) {
        let last = VISIBLE_COLUMNS - 1;
        let first = if column < self.first_column {
            column
        } else if column - self.first_column > last {
            column - last
        } else {
            self.first_column
        };
        self.first_column = first.min(last_first_column(columns));
    }

    /// Scrolls by `delta` whole columns, stopped at the first column and the last full view.
    pub fn scroll_by(&mut self, delta: i32, columns: u16) {
        let max = last_first_column(columns);
        let first = (i64::from(self.first_column) + i64::from(delta)).clamp(0, i64::from(max));
        self.first_column = u16::try_from(first).unwrap_or(max);
    }
}

/// The largest `first_column` that still fills the view, as `flow::layout` clamps it.
fn last_first_column(columns: u16) -> u16 {
    columns.saturating_sub(VISIBLE_COLUMNS)
}

/// What a click on the graph does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Select(NodeId),
    ShowOther(NodeId),
    Nothing,
}

/// The column of the band that shows `selected` in the graph of folder `view` with `expanded` open:
/// its own folder band, or the band of the nearest folder above it that has one (a file's folder, a
/// folder under a collapsed one). `None` when `selected` is not in `view`. Merges are not known
/// here; a merged folder is in the "other" band of the same column.
pub fn drawn_column(tree: &Tree, view: NodeId, expanded: &HashSet<NodeId>, selected: NodeId) -> Option<u16> {
    // From `selected` up to `view`.
    let mut chain = Vec::new();
    let mut n = Some(selected);
    while let Some(c) = n {
        chain.push(c);
        if c == view {
            break;
        }
        n = tree.node(c).parent;
    }
    if chain.last() != Some(&view) {
        return None;
    }
    chain.reverse();
    // Column 0 is the view band, always open; a deeper folder shows when its parent is expanded.
    let mut column = 0;
    for (i, pair) in chain.windows(2).enumerate() {
        let (parent, c) = (pair[0], pair[1]);
        if !tree.node(c).is_dir || (i > 0 && !expanded.contains(&parent)) {
            break;
        }
        column = i + 1;
    }
    u16::try_from(column).ok()
}

/// What a key in the graph does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAct {
    Select(NodeId),
    Toggle(NodeId),
    Open(NodeId),
    Up,
    Delete { node: NodeId, shift: bool },
    Nothing,
}

/// The part of band `hit` (an index from `flow::hit`), if there is such a band.
fn part_at(g: &Graph, hit: Option<usize>) -> Option<Part> {
    hit.and_then(|i| g.bands.get(i)).map(|b| b.part)
}

/// The drawn band of folder `n`.
fn folder_band(g: &Graph, n: NodeId) -> Option<&Band> {
    g.bands.iter().find(|b| b.part == Part::Folder(n))
}

/// A click on band `hit`: a folder band selects its folder, a "files" band selects the folder
/// that holds the files, an "other" band shows its members.
pub fn click(g: &Graph, hit: Option<usize>) -> Act {
    match part_at(g, hit) {
        Some(Part::Folder(n) | Part::Files(n)) => Act::Select(n),
        Some(Part::Other { parent, .. }) => Act::ShowOther(parent),
        None => Act::Nothing,
    }
}

/// The folder a right-click on band `hit` opens the menu for: only a folder band has one.
pub fn menu_target(g: &Graph, hit: Option<usize>) -> Option<NodeId> {
    match part_at(g, hit) {
        Some(Part::Folder(n)) => Some(n),
        _ => None,
    }
}

/// The folder a double-click on band `hit` makes the view folder: only a folder band.
pub fn open_target(g: &Graph, hit: Option<usize>) -> Option<NodeId> {
    menu_target(g, hit)
}

/// The folder the Menu key, Shift+F10, Enter, Delete and Shift+Delete act on: the selection,
/// only when it is drawn as its own folder band. A file, a merged folder, a folder under a
/// collapsed one or in a scrolled-out column, or no selection: `None`.
pub fn key_target(g: &Graph, selected: Option<NodeId>) -> Option<NodeId> {
    selected.filter(|&n| folder_band(g, n).is_some())
}

/// What key `key` (as the markup names it) does with `selected` in graph `g` of `tree`.
pub fn key(g: &Graph, tree: &Tree, selected: Option<NodeId>, key: &str) -> KeyAct {
    let select = |n: Option<NodeId>| n.map_or(KeyAct::Nothing, KeyAct::Select);
    let target = key_target(g, selected);
    match key {
        "up" | "down" => {
            let dir = if key == "up" { Dir::Up } else { Dir::Down };
            select(neighbour_rect(&folder_rects(g), selected, dir))
        }
        "left" | "right" => {
            // The first folder band whose folder passes `keep`; bands come column by column,
            // each group largest first.
            let first_folder = |keep: &dyn Fn(NodeId) -> bool| {
                select(g.bands.iter().find_map(|b| match b.part {
                    Part::Folder(c) if keep(c) => Some(c),
                    _ => None,
                }))
            };
            // No selection, or one not drawn as a folder band: the first folder band.
            let Some((n, band)) = target.and_then(|n| folder_band(g, n).map(|b| (n, b))) else {
                return first_folder(&|_| true);
            };
            if key == "right" {
                // The view band (column 0) has no expander but is always open.
                match band.expander {
                    Expander::Collapsed => KeyAct::Toggle(n),
                    _ if band.column == 0 || band.expander == Expander::Expanded => {
                        first_folder(&|c| tree.node(c).parent == Some(n))
                    }
                    _ => KeyAct::Nothing,
                }
            } else if band.expander == Expander::Expanded {
                KeyAct::Toggle(n)
            } else if band.column == 0 {
                KeyAct::Nothing
            } else {
                // A band of column `c + 1` is in the group of its tree parent in column `c`.
                select(tree.node(n).parent)
            }
        }
        "enter" => target.map_or(KeyAct::Nothing, KeyAct::Open),
        "backspace" => KeyAct::Up,
        "delete" | "shift-delete" => {
            target.map_or(KeyAct::Nothing, |node| KeyAct::Delete { node, shift: key == "shift-delete" })
        }
        _ => KeyAct::Nothing,
    }
}

/// `key`, with Right and Left decided by the graph state `expanded`, not by the expander of the
/// shown band: a layout may not have landed since the last key. Right expands a folder that is not
/// expanded (it never collapses); Left collapses one that is expanded (it never expands).
pub fn key_with(g: &Graph, tree: &Tree, expanded: &HashSet<NodeId>, selected: Option<NodeId>, name: &str) -> KeyAct {
    let _ = expanded;
    key(g, tree, selected, name)
}

/// The colour of each band of `g`, in band order. Age: the age class of the band's newest change
/// at `now`. File type: the tile colour of the band's cached dominant type; a band not counted
/// yet (`missing_types`), or with no bytes, gets the `Category::Other` colour.
pub fn colours(
    g: &Graph,
    mode: ColourMode,
    now: u32,
    theme: Theme,
    types: &HashMap<Part, Option<Category>>,
) -> Vec<[u8; 3]> {
    g.bands
        .iter()
        .map(|b| match mode {
            ColourMode::Age => age_colour(age_of(b.newest, now), theme),
            ColourMode::FileType => {
                let c = types.get(&b.part).copied().flatten().unwrap_or(Category::Other);
                treemap::filetype::colour(c, theme)
            }
        })
        .collect()
}

/// The parts of `g`'s bands whose file type is not in the cache `types`, in band order.
pub fn missing_types(g: &Graph, types: &HashMap<Part, Option<Category>>) -> Vec<Part> {
    g.bands.iter().map(|b| b.part).filter(|p| !types.contains_key(p)).collect()
}

/// The label of band `b` on the graph: (name, size). A folder band is labelled as a frame of the
/// treemap; "files" is a folder's own files; "other (N)" stands for N merged folders (and the
/// files when they merged too). Names go through `display_name`.
pub fn band_label(tree: &Tree, b: &Band, unit: Unit) -> (String, String) {
    let size = format_size(b.size, unit);
    match b.part {
        Part::Folder(n) => frame_label(tree, n, unit),
        Part::Files(_) => ("files".into(), size),
        Part::Other { folders: 0, .. } => ("other files".into(), size),
        Part::Other { folders, .. } => (format!("other ({folders})"), size),
    }
}

/// The tooltip of band `b`: (title, detail). The title is the path of the folder, "Files in
/// <path>" or "Other folders in <path>". The detail is the size, the share of the parent folder,
/// what is inside and the newest change. `counts` is the file count of each node
/// (`tree_view::file_counts`).
pub fn tooltip(tree: &Tree, counts: &[u64], b: &Band, unit: Unit) -> (String, String) {
    let path = |n: NodeId| display_name(&tree.path(n).to_string_lossy()).into_owned();
    // The share is of the folder above the band: for a folder band its parent (the view folder
    // is all of itself), for "files" and "other" the folder that holds them.
    let (title, parent, inside) = match b.part {
        Part::Folder(n) => {
            let files = counts.get(n.0 as usize).copied().unwrap_or(0);
            (path(n), tree.node(n).parent.unwrap_or(n), count_text(files, "file", "files"))
        }
        Part::Files(f) => {
            let files = flow::members(tree, &b.part).len() as u64;
            (format!("Files in {}", path(f)), f, count_text(files, "file", "files"))
        }
        Part::Other { parent, folders: 0, .. } => {
            let files = flow::members(tree, &b.part).len() as u64;
            (format!("Other files in {}", path(parent)), parent, count_text(files, "file", "files"))
        }
        Part::Other { parent, folders, files } => {
            let mut inside = count_text(u64::from(folders), "folder", "folders");
            if files {
                inside.push_str(" and files");
            }
            (format!("Other folders in {}", path(parent)), parent, inside)
        }
    };
    let share = format_percent(b.size, tree.node(parent).size);
    let of = display_name(tree.name(parent));
    let detail = format!("{}. {share} of {of}. {inside}. Last change: {}.", format_size(b.size, unit), last_change(b.newest));
    (title, detail)
}

/// "1 file" or "2 files", with the digits grouped.
fn count_text(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", group_digits(n), if n == 1 { one } else { many })
}

/// A newest change as the tooltip says it: the day, or "not known".
fn last_change(t: u32) -> String {
    if t == scan::time::UNKNOWN { "not known".into() } else { format_day(t) }
}

/// What a screen reader says of graph `g` of folder `view`: the view folder, the bands drawn after
/// it and the largest of them. The view folder is given: a scrolled graph does not draw it.
pub fn summary(tree: &Tree, g: &Graph, view: NodeId, unit: Unit) -> String {
    let name = |n: NodeId| display_name(tree.name(n)).into_owned();
    let of = format!(" of {}", name(view));
    // The view band (column 0) is the graph's root, not one of its bands.
    let mut bands = g.bands.iter().filter(|b| b.part != Part::Folder(view));
    let Some(mut largest) = bands.next() else { return format!("Folder graph{of}: 0 bands.") };
    let mut count = 1;
    for b in bands {
        count += 1;
        if b.size > largest.size {
            largest = b;
        }
    }
    let largest_name = match largest.part {
        Part::Folder(n) => name(n),
        Part::Files(_) => "files".into(),
        Part::Other { folders: 0, .. } => "other files".into(),
        Part::Other { .. } => "other".into(),
    };
    let noun = if count == 1 { "band" } else { "bands" };
    format!("Folder graph{of}: {count} {noun}, largest {largest_name}, {}.", format_size(largest.size, unit))
}

/// The text the graph shows over `g`, the layout of folder `view`, or "" for none: the treemap's
/// `NO_SIZES` when the folder has no file with a size. A graph with no area shows no text.
pub fn graph_note(tree: &Tree, view: NodeId, g: &Graph, has_area: bool) -> &'static str {
    if has_area && g.bands.is_empty() && tree.node(view).size == 0 { crate::tree_view::NO_SIZES } else { "" }
}

/// A day as `2026-10-26`: `t` is seconds since 2000-01-01 UTC. Civil-from-days (Howard Hinnant).
pub fn format_day(t: u32) -> String {
    // Days from 0000-03-01 to 2000-01-01.
    const DAYS_TO_2000: i64 = 730_425;
    let z = i64::from(t / 86_400) + DAYS_TO_2000;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The legend of the age colours, newest first.
pub const AGE_LEGEND: [(Age, &str); 4] = [
    (Age::Recent, "Changed in the last 6 months"),
    (Age::Middle, "Changed 6 to 24 months ago"),
    (Age::Old, "Changed more than 24 months ago"),
    (Age::Unknown, "Date not known"),
];

/// The view to show: the graph only for a finished scan with data and when it is chosen.
pub fn shown_view(state: &ScanState, loaded: bool, chosen: View) -> View {
    if loaded && matches!(state, ScanState::Done(_)) { chosen } else { View::Treemap }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Summary;
    use scan::rollup::{RawEntry, rollup};
    use scan::{Method, Phases};
    use std::time::Duration;
    use treemap::filetype;

    const NOW: u32 = 846_288_000;
    const D: u32 = 86_400;

    /// root { a { a1.mp4 600 at NOW }, b { b1.zip 300 at NOW - 400 D }, c.txt 100 unknown, e {} }
    fn tree() -> Tree {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool, modified: u32| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
            modified,
        };
        let entries = vec![
            e(0, 0, "root", 0, true, 0),
            e(1, 0, "a", 0, true, 0),
            e(2, 0, "b", 0, true, 0),
            e(3, 0, "c.txt", 100, false, 0),
            e(4, 1, "a1.mp4", 600, false, NOW),
            e(5, 2, "b1.zip", 300, false, NOW - 400 * D),
            e(6, 0, "e", 0, true, 0),
        ];
        rollup(0, entries).unwrap().tree
    }

    fn layout_of(t: &Tree, view: NodeId) -> Graph {
        let (expanded, open_other) = (HashSet::new(), HashSet::new());
        flow::layout(&flow::Input {
            tree: t,
            view,
            expanded: &expanded,
            open_other: &open_other,
            w: 800.0,
            h: 600.0,
            scale: 1.0,
            first_column: 0,
        })
    }

    #[test]
    fn colours_age_mode_is_the_age_colour_of_each_band() {
        let t = tree();
        let g = layout_of(&t, t.root());
        assert!(g.bands.len() >= 3, "{g:?}");
        for theme in [Theme::Light, Theme::Dark] {
            let want: Vec<[u8; 3]> = g.bands.iter().map(|b| age_colour(age_of(b.newest, NOW), theme)).collect();
            assert_eq!(colours(&g, ColourMode::Age, NOW, theme, &HashMap::new()), want, "{theme:?}");
        }
    }

    #[test]
    fn colours_file_type_with_a_cached_type_is_its_tile_colour_and_a_miss_is_other() {
        let t = tree();
        let g = layout_of(&t, t.root());
        let first = g.bands[0].part;
        let types = HashMap::from([(first, Some(Category::Video))]);
        let theme = Theme::Dark;

        let got = colours(&g, ColourMode::FileType, NOW, theme, &types);

        let mut want = vec![filetype::colour(Category::Other, theme); g.bands.len()];
        want[0] = filetype::colour(Category::Video, theme);
        assert_eq!(got, want);
        let missing: Vec<Part> = g.bands[1..].iter().map(|b| b.part).collect();
        assert_eq!(missing_types(&g, &types), missing);
    }

    #[test]
    fn colours_file_type_known_to_have_no_bytes_is_other_and_not_missing() {
        let t = tree();
        let g = layout_of(&t, t.root());
        let types: HashMap<Part, Option<Category>> = g.bands.iter().map(|b| (b.part, None)).collect();
        let got = colours(&g, ColourMode::FileType, NOW, Theme::Light, &types);
        assert_eq!(got, vec![filetype::colour(Category::Other, Theme::Light); g.bands.len()]);
        assert!(missing_types(&g, &types).is_empty());
    }

    #[test]
    fn graph_note_zero_size_view_is_no_sizes() {
        let t = tree();
        let empty = t.children(t.root()).find(|&n| t.name(n) == "e").unwrap();
        let g = layout_of(&t, empty);
        assert!(g.bands.is_empty());
        assert_eq!(graph_note(&t, empty, &g, true), crate::tree_view::NO_SIZES);
        // A graph with bands, or with no area, shows no text.
        let full = layout_of(&t, t.root());
        assert_eq!(graph_note(&t, t.root(), &full, true), "");
        assert_eq!(graph_note(&t, empty, &g, false), "");
    }

    fn done() -> ScanState {
        ScanState::Done(Summary {
            method: Method::Walk,
            files: 1,
            dirs: 1,
            skipped: 0,
            elapsed: Duration::ZERO,
            phases: Phases::default(),
            total: 1,
        })
    }

    #[test]
    fn shown_view_table() {
        let states = [
            ScanState::Idle,
            ScanState::Scanning { files: 1, dirs: 1 },
            done(),
            ScanState::Error("x".into()),
            ScanState::Cancelled,
        ];
        for state in &states {
            for chosen in [View::Treemap, View::Graph] {
                let want = if matches!(state, ScanState::Done(_)) && chosen == View::Graph {
                    View::Graph
                } else {
                    View::Treemap
                };
                assert_eq!(shown_view(state, true, chosen), want, "{state:?} loaded {chosen:?}");
            }
        }
        for chosen in [View::Treemap, View::Graph] {
            assert_eq!(shown_view(&done(), false, chosen), View::Treemap, "done without data {chosen:?}");
        }
    }

    // ---- actions, keys and graph state (TP4.1) ----

    /// root 970 {
    ///   a 500 { a1 300 { a1x 300 { a1y 300 { f.bin 300 } } }, a2 140 { g.bin }, a.txt 50, a3 10 { h.bin } },
    ///   b 300 { b.bin },  c.txt 150,  d 20 { d1 20 { i.bin } }
    /// }
    /// Root's split: folders [a, b], files 150, other { d } (2 %). a's: [a1, a2], files 50,
    /// other { a3 } (2 %). `b` and `a2` hold only files: no expander.
    fn deep_tree() -> Tree {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
            modified: NOW,
        };
        let entries = vec![
            e(0, 0, "root", 0, true),
            e(1, 0, "a", 0, true),
            e(2, 0, "b", 0, true),
            e(3, 0, "c.txt", 150, false),
            e(4, 0, "d", 0, true),
            e(5, 1, "a1", 0, true),
            e(6, 1, "a2", 0, true),
            e(7, 1, "a.txt", 50, false),
            e(8, 1, "a3", 0, true),
            e(9, 5, "a1x", 0, true),
            e(10, 9, "a1y", 0, true),
            e(11, 10, "f.bin", 300, false),
            e(12, 6, "g.bin", 140, false),
            e(13, 8, "h.bin", 10, false),
            e(14, 2, "b.bin", 300, false),
            e(15, 4, "d1", 0, true),
            e(16, 15, "i.bin", 20, false),
        ];
        rollup(0, entries).unwrap().tree
    }

    /// The node named `name` (names are unique in the fixtures).
    fn id(t: &Tree, name: &str) -> NodeId {
        let mut stack = vec![t.root()];
        while let Some(n) = stack.pop() {
            if t.name(n) == name {
                return n;
            }
            stack.extend(t.children(n));
        }
        panic!("no node {name}")
    }

    fn graph_of(t: &Tree, expanded: &[&str], first_column: u16) -> Graph {
        graph_with(t, t.root(), expanded, &[], first_column)
    }

    /// The layout of folder `view` with these folders expanded and these "other" bands opened.
    fn graph_with(t: &Tree, view: NodeId, expanded: &[&str], open_other: &[&str], first_column: u16) -> Graph {
        let expanded: HashSet<NodeId> = expanded.iter().map(|n| id(t, n)).collect();
        let open_other: HashSet<NodeId> = open_other.iter().map(|n| id(t, n)).collect();
        flow::layout(&flow::Input {
            tree: t,
            view,
            expanded: &expanded,
            open_other: &open_other,
            w: 800.0,
            h: 600.0,
            scale: 1.0,
            first_column,
        })
    }

    fn index_of(g: &Graph, part: Part) -> usize {
        g.bands.iter().position(|b| b.part == part).unwrap_or_else(|| panic!("no band {part:?} in {g:?}"))
    }

    #[test]
    fn fixture_has_the_bands_the_action_tests_need() {
        let t = deep_tree();
        let g = graph_of(&t, &[], 0);
        let (root, a, b, d) = (t.root(), id(&t, "a"), id(&t, "b"), id(&t, "d"));
        let parts: Vec<Part> = g.bands.iter().map(|b| b.part).collect();
        let other = Part::Other { parent: root, folders: 1, files: false };
        assert_eq!(parts, vec![Part::Folder(root), Part::Folder(a), Part::Folder(b), Part::Files(root), other]);
        assert_eq!(g.bands[1].expander, flow::Expander::Collapsed);
        assert_eq!(g.bands[2].expander, flow::Expander::None);
        assert!(!g.bands.iter().any(|band| band.part == Part::Folder(d)), "d is merged");
    }

    #[test]
    fn click_menu_and_open_table() {
        let t = deep_tree();
        let g = graph_of(&t, &[], 0);
        let (root, a) = (t.root(), id(&t, "a"));
        let other = Part::Other { parent: root, folders: 1, files: false };
        let cases = [
            (Some(Part::Folder(a)), Act::Select(a), Some(a), Some(a)),
            (Some(Part::Files(root)), Act::Select(root), None, None),
            (Some(other), Act::ShowOther(root), None, None),
            (None, Act::Nothing, None, None),
        ];
        for (part, want_click, want_menu, want_open) in cases {
            let hit = part.map(|p| index_of(&g, p));
            assert_eq!(click(&g, hit), want_click, "click {part:?}");
            assert_eq!(menu_target(&g, hit), want_menu, "menu {part:?}");
            assert_eq!(open_target(&g, hit), want_open, "open {part:?}");
        }
    }

    #[test]
    fn click_hit_past_the_last_band_is_nothing() {
        let t = deep_tree();
        let g = graph_of(&t, &[], 0);
        let past = Some(g.bands.len());
        assert_eq!(click(&g, past), Act::Nothing);
        assert_eq!(menu_target(&g, past), None);
        assert_eq!(open_target(&g, past), None);
    }

    #[test]
    fn key_target_only_a_drawn_folder_band() {
        let t = deep_tree();
        let g = graph_of(&t, &[], 0);
        let a = id(&t, "a");
        assert_eq!(key_target(&g, Some(a)), Some(a), "drawn folder");
        assert_eq!(key_target(&g, Some(t.root())), Some(t.root()), "the view folder band");
        assert_eq!(key_target(&g, Some(id(&t, "c.txt"))), None, "a file");
        assert_eq!(key_target(&g, Some(id(&t, "d"))), None, "a merged folder");
        assert_eq!(key_target(&g, Some(id(&t, "a1"))), None, "a folder under a collapsed one");
        assert_eq!(key_target(&g, None), None, "no selection");

        // Five columns, scrolled by one: column 0 (the view folder) is not drawn.
        let scrolled = graph_of(&t, &["a", "a1", "a1x"], 1);
        assert_eq!((scrolled.columns, scrolled.first_column), (5, 1));
        assert_eq!(key_target(&scrolled, Some(t.root())), None, "a folder in a scrolled-out column");
        assert_eq!(key_target(&scrolled, Some(a)), Some(a));
    }

    #[test]
    fn key_table() {
        let t = deep_tree();
        let closed = graph_of(&t, &[], 0);
        let open = graph_of(&t, &["a"], 0);
        let (root, a, b, a1) = (t.root(), id(&t, "a"), id(&t, "b"), id(&t, "a1"));
        let (file, merged) = (id(&t, "c.txt"), id(&t, "d"));
        let cases: [(&Graph, Option<NodeId>, &str, KeyAct); 30] = [
            // Up and Down: the neighbour folder band.
            (&closed, Some(a), "down", KeyAct::Select(b)),
            (&closed, Some(b), "up", KeyAct::Select(a)),
            (&open, Some(a1), "down", KeyAct::Select(id(&t, "a2"))),
            // Right: expand, go to the first child folder band, or nothing.
            (&closed, Some(a), "right", KeyAct::Toggle(a)),
            (&open, Some(a), "right", KeyAct::Select(a1)),
            (&closed, Some(b), "right", KeyAct::Nothing),
            // The view band (column 0) is always open: Right goes to its largest folder band.
            (&closed, Some(root), "right", KeyAct::Select(a)),
            // Left: collapse, go to the parent band, or nothing on column 0.
            (&open, Some(a), "left", KeyAct::Toggle(a)),
            (&closed, Some(a), "left", KeyAct::Select(root)),
            (&open, Some(a1), "left", KeyAct::Select(a)),
            (&closed, Some(b), "left", KeyAct::Select(root)),
            (&closed, Some(root), "left", KeyAct::Nothing),
            // An arrow with no selection, or a selection not drawn as a folder band: the first.
            (&closed, None, "up", KeyAct::Select(root)),
            (&closed, None, "down", KeyAct::Select(root)),
            (&closed, None, "left", KeyAct::Select(root)),
            (&closed, None, "right", KeyAct::Select(root)),
            (&closed, Some(file), "right", KeyAct::Select(root)),
            (&closed, Some(merged), "down", KeyAct::Select(root)),
            // Enter opens a drawn folder band; Backspace goes up.
            (&closed, Some(a), "enter", KeyAct::Open(a)),
            (&closed, Some(file), "enter", KeyAct::Nothing),
            (&closed, Some(a), "backspace", KeyAct::Up),
            (&closed, None, "backspace", KeyAct::Up),
            // Delete only through `key_target`.
            (&closed, Some(a), "delete", KeyAct::Delete { node: a, shift: false }),
            (&closed, Some(a), "shift-delete", KeyAct::Delete { node: a, shift: true }),
            (&closed, Some(file), "delete", KeyAct::Nothing),
            (&closed, Some(merged), "shift-delete", KeyAct::Nothing),
            (&closed, None, "delete", KeyAct::Nothing),
            // Other keys do nothing.
            (&closed, Some(a), "space", KeyAct::Nothing),
            (&closed, Some(a), "", KeyAct::Nothing),
            (&closed, Some(a), "Delete", KeyAct::Nothing),
        ];
        for (g, selected, k, want) in cases {
            assert_eq!(key(g, &t, selected, k), want, "{k:?} on {selected:?}");
        }
    }

    /// root { 40 folders of one 1-byte file each }: every folder is 2.5 % of root, so all merge.
    fn all_merged_tree() -> Tree {
        let e = |id: u64, parent: u64, name: String, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name,
            size,
            allocated: size,
            is_dir,
            modified: NOW,
        };
        let mut entries = vec![e(0, 0, "root".into(), 0, true)];
        for i in 1..=40 {
            entries.push(e(i, 0, format!("d{i}"), 0, true));
            entries.push(e(100 + i, i, format!("f{i}.bin"), 1, false));
        }
        rollup(0, entries).unwrap().tree
    }

    #[test]
    fn key_right_on_the_view_band_enters_the_graph() {
        let t = deep_tree();
        // From no selection: the first arrow selects the view band, Right goes to its largest child.
        let g = graph_of(&t, &[], 0);
        let KeyAct::Select(first) = key(&g, &t, None, "right") else { panic!("no first band") };
        assert_eq!(first, t.root());
        assert_eq!(key(&g, &t, Some(first), "right"), KeyAct::Select(id(&t, "a")));

        // A view folder that holds only files: no folder band to go to.
        let a2 = id(&t, "a2");
        let files_only = graph_with(&t, a2, &[], &[], 0);
        assert_eq!(files_only.bands.iter().map(|b| b.part).collect::<Vec<_>>(), vec![Part::Folder(a2), Part::Files(a2)]);
        assert_eq!(key(&files_only, &t, Some(a2), "right"), KeyAct::Nothing);

        // A view folder whose subfolders all merge into "other": nothing to go to.
        let m = all_merged_tree();
        let merged = graph_with(&m, m.root(), &[], &[], 0);
        let parts: Vec<Part> = merged.bands.iter().map(|b| b.part).collect();
        assert_eq!(parts, vec![Part::Folder(m.root()), Part::Other { parent: m.root(), folders: 40, files: false }]);
        assert_eq!(key(&merged, &m, Some(m.root()), "right"), KeyAct::Nothing);
    }

    #[test]
    fn key_delete_on_a_scrolled_out_folder_does_nothing() {
        let t = deep_tree();
        let g = graph_of(&t, &["a", "a1", "a1x"], 1);
        assert_eq!(key(&g, &t, Some(t.root()), "delete"), KeyAct::Nothing);
        assert_eq!(key(&g, &t, Some(t.root()), "enter"), KeyAct::Nothing);
    }

    #[test]
    fn key_up_and_down_agree_with_the_neighbour_folder_rect() {
        let t = deep_tree();
        let g = graph_of(&t, &["a", "a1"], 0);
        let rects = flow::folder_rects(&g);
        for selected in rects.iter().map(|r| Some(r.node)).chain([None]) {
            for (k, dir) in [("up", crate::nav::Dir::Up), ("down", crate::nav::Dir::Down)] {
                let want = crate::nav::neighbour_rect(&rects, selected, dir).map_or(KeyAct::Nothing, KeyAct::Select);
                assert_eq!(key(&g, &t, selected, k), want, "{k} on {selected:?}");
            }
        }
    }

    #[test]
    fn graph_state_toggle_adds_then_removes() {
        let mut s = GraphState::default();
        s.toggle(NodeId(3));
        assert_eq!(s.expanded, HashSet::from([NodeId(3)]));
        s.toggle(NodeId(4));
        s.toggle(NodeId(3));
        assert_eq!(s.expanded, HashSet::from([NodeId(4)]));
    }

    #[test]
    fn graph_state_show_other_adds_the_parent() {
        let mut s = GraphState::default();
        s.show_other(NodeId(2));
        s.show_other(NodeId(2));
        assert_eq!(s.open_other, HashSet::from([NodeId(2)]));
    }

    #[test]
    fn graph_state_reroot_clears_expanded_open_other_and_first_column() {
        let mut s = GraphState {
            expanded: HashSet::from([NodeId(1)]),
            open_other: HashSet::from([NodeId(2)]),
            first_column: 3,
            colour: ColourMode::FileType,
            ..GraphState::default()
        };
        s.reroot();
        assert!(s.expanded.is_empty());
        assert!(s.open_other.is_empty());
        assert_eq!(s.first_column, 0);
        assert_eq!(s.colour, ColourMode::FileType, "the colour mode is kept");
    }

    #[test]
    fn graph_state_after_detach_drops_ids_inside_and_the_shown_graph() {
        let t = deep_tree();
        let mut s = GraphState {
            expanded: HashSet::from([NodeId(1), NodeId(5), NodeId(7)]),
            open_other: HashSet::from([NodeId(5), NodeId(9)]),
            first_column: 1,
            shown: Some(graph_of(&t, &[], 0)),
            types: HashMap::from([(Part::Files(NodeId(9)), Some(Category::Video))]),
            ..GraphState::default()
        };
        s.after_detach(|n| n == NodeId(5) || n == NodeId(7));
        assert_eq!(s.expanded, HashSet::from([NodeId(1)]));
        assert_eq!(s.open_other, HashSet::from([NodeId(9)]));
        assert!(s.shown.is_none());
        assert!(s.types.is_empty());
    }

    #[test]
    fn drawn_column_table() {
        let t = deep_tree();
        let root = t.root();
        let none = HashSet::new();
        let a_open: HashSet<NodeId> = HashSet::from([id(&t, "a")]);
        // (view, expanded, selected, want)
        let cases: [(NodeId, &HashSet<NodeId>, &str, Option<u16>); 9] = [
            (root, &none, "root", Some(0)),
            (root, &none, "a", Some(1)),
            (root, &none, "c.txt", Some(0)),
            // A folder under a collapsed one: its parent's column.
            (root, &none, "a1", Some(1)),
            (root, &none, "f.bin", Some(1)),
            // A merged folder: its own column (the "other" band of that column).
            (root, &none, "d", Some(1)),
            (root, &a_open, "a1", Some(2)),
            (root, &a_open, "f.bin", Some(2)),
            // A node outside the view.
            (id(&t, "a"), &none, "b", None),
        ];
        for (view, expanded, name, want) in cases {
            assert_eq!(drawn_column(&t, view, expanded, id(&t, name)), want, "{name} in view {}", t.name(view));
        }
    }

    #[test]
    fn key_with_a_state_that_matches_the_shown_graph_is_key() {
        let t = deep_tree();
        let a = id(&t, "a");
        let (closed, open) = (graph_of(&t, &[], 0), graph_of(&t, &["a"], 0));
        let names = ["root", "a", "b", "a1", "c.txt", "d"];
        for (g, expanded) in [(&closed, HashSet::new()), (&open, HashSet::from([a]))] {
            for selected in names.iter().map(|n| Some(id(&t, n))).chain([None]) {
                for k in ["up", "down", "left", "right", "enter", "backspace", "delete", "shift-delete", "space"] {
                    assert_eq!(key_with(g, &t, &expanded, selected, k), key(g, &t, selected, k), "{k} on {selected:?}");
                }
            }
        }
    }

    #[test]
    fn key_with_right_and_left_follow_the_state_before_the_layout_lands() {
        let t = deep_tree();
        let (root, a, b, a1) = (t.root(), id(&t, "a"), id(&t, "b"), id(&t, "a1"));
        let (closed, open) = (graph_of(&t, &[], 0), graph_of(&t, &["a"], 0));
        let (none, a_open) = (HashSet::new(), HashSet::from([a]));
        let cases: [(&Graph, &HashSet<NodeId>, NodeId, &str, KeyAct); 10] = [
            // Right was pressed, its layout has not landed: Right again does not collapse.
            (&closed, &a_open, a, "right", KeyAct::Nothing),
            (&closed, &none, a, "right", KeyAct::Toggle(a)),
            // Left was pressed, its layout has not landed: Left again does not expand.
            (&open, &none, a, "left", KeyAct::Select(root)),
            (&open, &none, a, "right", KeyAct::Toggle(a)),
            (&open, &a_open, a, "right", KeyAct::Select(a1)),
            (&open, &a_open, a, "left", KeyAct::Toggle(a)),
            (&closed, &a_open, a, "left", KeyAct::Toggle(a)),
            // No expander: as `key`.
            (&closed, &none, b, "right", KeyAct::Nothing),
            (&closed, &none, b, "left", KeyAct::Select(root)),
            (&closed, &none, root, "right", KeyAct::Select(a)),
        ];
        for (g, expanded, selected, k, want) in cases {
            assert_eq!(key_with(g, &t, expanded, Some(selected), k), want, "{k} on {} with {expanded:?}", t.name(selected));
        }
    }

    #[test]
    fn graph_state_after_detach_of_nothing_inside_keeps_the_sets() {
        let mut s = GraphState { expanded: HashSet::from([NodeId(1)]), ..GraphState::default() };
        s.after_detach(|_| false);
        assert_eq!(s.expanded, HashSet::from([NodeId(1)]));
    }

    #[test]
    fn graph_state_scroll_to_keeps_the_column_in_view_with_the_least_move() {
        // (first_column before, column, columns, first_column after)
        let cases = [
            (0, 2, 10, 0),
            (0, 3, 10, 0),
            (0, 4, 10, 1),
            (0, 9, 10, 6),
            (6, 5, 10, 5),
            (6, 0, 10, 0),
            (3, 5, 10, 3),
            (0, 2, 3, 0),
            (0, 9, 6, 2),
            (5, 1, 6, 1),
            (5, 4, 6, 2),
            (0, 0, 0, 0),
        ];
        for (before, column, columns, want) in cases {
            let mut s = GraphState { first_column: before, ..GraphState::default() };
            s.scroll_to(column, columns);
            assert_eq!(s.first_column, want, "from {before} to column {column} of {columns}");
        }
    }

    #[test]
    fn graph_state_scroll_by_clamps_to_the_last_full_view() {
        // (first_column before, delta, columns, first_column after)
        let cases = [
            (0, 3, 10, 3),
            (3, -5, 10, 0),
            (0, 100, 10, 6),
            (2, 1, 10, 3),
            (0, 1, 3, 0),
            (0, 1, 4, 0),
            (0, 1, 5, 1),
            (6, i32::MIN, 10, 0),
            (0, i32::MAX, 10, 6),
        ];
        for (before, delta, columns, want) in cases {
            let mut s = GraphState { first_column: before, ..GraphState::default() };
            s.scroll_by(delta, columns);
            assert_eq!(s.first_column, want, "from {before} by {delta} of {columns}");
        }
    }

    /// The pointer path (`menu_target` of the band under the pointer) and the key path (`click`
    /// selects, then `key_target` of the selection) name the same folder on every band. A
    /// "files" band selects its folder (design): the key path then names that folder's own band,
    /// never the files band. An "other" band selects nothing.
    #[test]
    fn guard_paths_agree_on_every_band() {
        let t = deep_tree();
        let root = t.root();
        let graphs = [
            graph_of(&t, &[], 0),
            graph_of(&t, &["a"], 0),
            graph_of(&t, &["a", "a1", "a1x"], 1),
            // Opened "other" bands: `d` (2 % of root) and `a3` (2 % of a) are drawn as folders.
            graph_with(&t, root, &["a"], &["root", "a"], 0),
            graph_with(&t, root, &["a", "a1", "a1x"], &["root", "a"], 1),
        ];
        let opened = &graphs[3];
        for name in ["d", "a3"] {
            assert!(opened.bands.iter().any(|b| b.part == Part::Folder(id(&t, name))), "{name} opened in {opened:?}");
        }
        for g in &graphs {
            assert!(!g.bands.is_empty());
            for (i, band) in g.bands.iter().enumerate() {
                let menu = menu_target(g, Some(i));
                assert_eq!(open_target(g, Some(i)), menu, "open and menu agree on {:?}", band.part);
                match (band.part, click(g, Some(i))) {
                    (Part::Folder(n), Act::Select(s)) => {
                        assert_eq!(s, n);
                        assert_eq!(menu, Some(n), "{:?}", band.part);
                        assert_eq!(key_target(g, Some(s)), menu, "{:?}", band.part);
                    }
                    (Part::Files(f), Act::Select(s)) => {
                        assert_eq!(s, f);
                        assert_eq!(menu, None, "no menu on {:?}", band.part);
                        let folder_band = g.bands.iter().position(|b| b.part == Part::Folder(f));
                        assert_eq!(key_target(g, Some(s)), menu_target(g, folder_band), "{:?}", band.part);
                    }
                    (Part::Other { parent, .. }, Act::ShowOther(p)) => {
                        assert_eq!(p, parent);
                        assert_eq!(menu, None, "no menu on {:?}", band.part);
                    }
                    (part, act) => panic!("{part:?} clicked gives {act:?}"),
                }
            }
        }
    }

    // ---- texts (TP4.2) ----

    const T: u32 = NOW;

    /// root 1000 { a 600 { a1.bin 400 at T, s { x.mp4 200 at 1 } }, b 300 { y.zip 300 unknown },
    /// c.txt 100 at T - 200 D }
    fn text_tree() -> Tree {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool, modified: u32| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
            modified,
        };
        let entries = vec![
            e(0, 0, "root", 0, true, 0),
            e(1, 0, "a", 0, true, 0),
            e(2, 0, "b", 0, true, 0),
            e(3, 0, "c.txt", 100, false, T - 200 * D),
            e(4, 1, "a1.bin", 400, false, T),
            e(5, 1, "s", 0, true, 0),
            e(6, 5, "x.mp4", 200, false, 1),
            e(7, 2, "y.zip", 300, false, 0),
        ];
        rollup(0, entries).unwrap().tree
    }

    fn band_of(g: &Graph, part: Part) -> Band {
        *g.bands.iter().find(|b| b.part == part).unwrap_or_else(|| panic!("no band {part:?} in {g:?}"))
    }

    fn counts_of(t: &Tree) -> Vec<u64> {
        crate::tree_view::file_counts(t)
    }

    fn plain_band(part: Part, size: u64, newest: u32) -> Band {
        Band { part, column: 1, x: 0.0, y: 0.0, w: 10.0, h: 10.0, size, newest, expander: Expander::None }
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn format_day_known_days() {
        assert_eq!(format_day(1), "2000-01-01");
        assert_eq!(format_day(5_097_600), "2000-02-29");
        assert_eq!(format_day(3_160_857_600), "2100-03-01");
        assert_eq!(format_day(3_160_857_600 - D), "2100-02-28");
        assert_eq!(format_day(T), "2026-10-26");
        assert_eq!(format_day(T - 200 * D), "2026-04-09");
        assert_eq!(format_day(u32::MAX), "2136-02-07");
    }

    #[test]
    fn tooltip_folder_with_two_files_and_a_known_time() {
        let t = text_tree();
        let g = layout_of(&t, t.root());
        let a = Part::Folder(id(&t, "a"));
        let got = tooltip(&t, &counts_of(&t), &band_of(&g, a), Unit::Dynamic);
        assert_eq!(got, pair("root\\a", "600 B. 60.0% of root. 2 files. Last change: 2026-10-26."));
    }

    #[test]
    fn tooltip_folder_with_one_file_and_an_unknown_time() {
        let t = text_tree();
        let g = layout_of(&t, t.root());
        let b = Part::Folder(id(&t, "b"));
        let got = tooltip(&t, &counts_of(&t), &band_of(&g, b), Unit::Dynamic);
        assert_eq!(got, pair("root\\b", "300 B. 30.0% of root. 1 file. Last change: not known."));
    }

    #[test]
    fn tooltip_view_folder_is_all_of_itself_and_uses_the_unit() {
        let t = text_tree();
        let g = layout_of(&t, t.root());
        let got = tooltip(&t, &counts_of(&t), &band_of(&g, Part::Folder(t.root())), Unit::Byte);
        assert_eq!(got, pair("root", "1,000 B. 100.0% of root. 4 files. Last change: 2026-10-26."));
    }

    #[test]
    fn tooltip_files_band() {
        let t = text_tree();
        let g = layout_of(&t, t.root());
        let got = tooltip(&t, &counts_of(&t), &band_of(&g, Part::Files(t.root())), Unit::Dynamic);
        assert_eq!(got, pair("Files in root", "100 B. 10.0% of root. 1 file. Last change: 2026-04-09."));
    }

    #[test]
    fn tooltip_other_band_with_folders_and_files() {
        let t = text_tree();
        let root = t.root();
        let one = plain_band(Part::Other { parent: root, folders: 1, files: true }, 400, T - 200 * D);
        assert_eq!(
            tooltip(&t, &counts_of(&t), &one, Unit::Dynamic),
            pair("Other folders in root", "400 B. 40.0% of root. 1 folder and files. Last change: 2026-04-09.")
        );
        let many = plain_band(Part::Other { parent: root, folders: 12, files: false }, 90, 0);
        assert_eq!(
            tooltip(&t, &counts_of(&t), &many, Unit::Dynamic),
            pair("Other folders in root", "90 B. 9.0% of root. 12 folders. Last change: not known.")
        );
    }

    #[test]
    fn tooltip_other_band_of_files_only_counts_the_files() {
        let t = text_tree();
        let only_files = plain_band(Part::Other { parent: t.root(), folders: 0, files: true }, 100, T - 200 * D);
        assert_eq!(
            tooltip(&t, &counts_of(&t), &only_files, Unit::Dynamic),
            pair("Other files in root", "100 B. 10.0% of root. 1 file. Last change: 2026-04-09.")
        );
        assert_eq!(band_label(&t, &only_files, Unit::Dynamic), pair("other files", "100 B"));
    }

    #[test]
    fn band_label_folder_files_and_other() {
        let t = text_tree();
        let g = layout_of(&t, t.root());
        let a = band_of(&g, Part::Folder(id(&t, "a")));
        assert_eq!(band_label(&t, &a, Unit::Dynamic), crate::tree_view::frame_label(&t, id(&t, "a"), Unit::Dynamic));
        assert_eq!(band_label(&t, &a, Unit::Dynamic), pair("a\\", "600 B"));
        let files = band_of(&g, Part::Files(t.root()));
        assert_eq!(band_label(&t, &files, Unit::Dynamic), pair("files", "100 B"));
        let other = plain_band(Part::Other { parent: t.root(), folders: 7, files: true }, 1536, 0);
        assert_eq!(band_label(&t, &other, Unit::Dynamic), pair("other (7)", "1.5 KB"));
        assert_eq!(band_label(&t, &other, Unit::Byte), pair("other (7)", "1,536 B"));
    }

    #[test]
    fn summary_names_the_view_folder_the_bands_and_the_largest() {
        let t = text_tree();
        let g = layout_of(&t, t.root());
        assert_eq!(summary(&t, &g, t.root(), Unit::Dynamic), "Folder graph of root: 3 bands, largest a, 600 B.");
    }

    #[test]
    fn summary_one_band_is_singular_and_an_empty_graph_has_no_bands() {
        let t = text_tree();
        let g = layout_of(&t, id(&t, "b"));
        assert_eq!(summary(&t, &g, id(&t, "b"), Unit::Dynamic), "Folder graph of b: 1 band, largest files, 300 B.");
        assert_eq!(summary(&t, &Graph::default(), t.root(), Unit::Dynamic), "Folder graph of root: 0 bands.");
    }

    #[test]
    fn summary_of_a_scrolled_graph_names_the_view_folder() {
        // root { l1 { l2 { l3 { l4 { l5 { f.bin 100 } } } } } }: six columns with l1 to l4 expanded.
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
            modified: T,
        };
        let mut entries = vec![e(0, 0, "root", 0, true)];
        for i in 1..=5 {
            entries.push(e(i, i - 1, &format!("l{i}"), 0, true));
        }
        entries.push(e(6, 5, "f.bin", 100, false));
        let t = rollup(0, entries).unwrap().tree;

        let g = graph_with(&t, t.root(), &["l1", "l2", "l3", "l4"], &[], 2);

        assert_eq!((g.columns, g.first_column), (6, 2));
        // The first drawn band is l2, whose parent is l1: the view folder is still the root.
        assert_eq!(summary(&t, &g, t.root(), Unit::Dynamic), "Folder graph of root: 4 bands, largest l2, 100 B.");
    }

    #[test]
    fn graph_texts_spoofed_name_shows_a_placeholder() {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
            modified: T,
        };
        let raw = "evil\u{202E}gnp.exe.";
        let entries = vec![
            e(0, 0, "root", 0, true),
            e(1, 0, raw, 0, true),
            e(2, 1, "f.bin", 900, false),
            e(3, 0, "z.bin", 100, false),
        ];
        let t = rollup(0, entries).unwrap().tree;
        let g = layout_of(&t, t.root());
        let counts = counts_of(&t);
        let evil = band_of(&g, Part::Folder(id(&t, raw)));
        let shown = "evil\u{FFFD}gnp.exe.";

        assert_eq!(band_label(&t, &evil, Unit::Dynamic), pair(&format!("{shown}\\"), "900 B"));
        assert_eq!(
            tooltip(&t, &counts, &evil, Unit::Dynamic),
            pair(&format!("root\\{shown}"), "900 B. 90.0% of root. 1 file. Last change: 2026-10-26.")
        );
        assert_eq!(summary(&t, &g, t.root(), Unit::Dynamic), format!("Folder graph of root: 2 bands, largest {shown}, 900 B."));
        // The spoofed folder as the view folder, and as the folder of a files band.
        let inner = layout_of(&t, id(&t, raw));
        assert_eq!(summary(&t, &inner, id(&t, raw), Unit::Dynamic), format!("Folder graph of {shown}: 1 band, largest files, 900 B."));
        let files = band_of(&inner, Part::Files(id(&t, raw)));
        assert_eq!(tooltip(&t, &counts, &files, Unit::Dynamic).0, format!("Files in root\\{shown}"));
        for text in [band_label(&t, &evil, Unit::Dynamic).0, tooltip(&t, &counts, &evil, Unit::Dynamic).0, summary(&t, &g, t.root(), Unit::Dynamic)] {
            assert!(!text.contains('\u{202E}'), "{text:?}");
        }
    }
}

//! Tree and treemap view logic: file counts, row flattening, sorting, expansion, selection
//! helpers and treemap hit-testing. Plain functions, no Slint types, so it is unit tested.

use crate::model::display_name;
use scan::{NodeId, Tree};
use std::cmp::Ordering;
use std::collections::HashSet;
use treemap::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Percent,
    Files,
}

/// One visible line of the file tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub node: NodeId,
    pub depth: u32,
    pub expanded: bool,
    pub has_children: bool,
}

/// Expansion and sort state of the file tree.
#[derive(Debug, Clone)]
pub struct ViewState {
    pub expanded: HashSet<NodeId>,
    pub key: SortKey,
    pub descending: bool,
}

impl ViewState {
    /// Root expanded, sorted by size, largest first.
    pub fn new(root: NodeId) -> Self {
    let mut expanded = HashSet::new();
    expanded.insert(root);
    Self { expanded, key: SortKey::Size, descending: true }
    }

    /// Same key flips the direction. A new key starts ascending for Name, descending otherwise.
    pub fn sort_by(&mut self, key: SortKey) {
    if self.key == key {
        self.descending = !self.descending;
    } else {
        self.key = key;
        self.descending = key != SortKey::Name;
    }
    }

    /// Expands a collapsed folder, collapses an expanded one. Files are ignored.
    pub fn toggle(&mut self, tree: &Tree, node: NodeId) {
    if tree.node(node).first_child.is_none() {
        return;
    }
    if !self.expanded.remove(&node) {
        self.expanded.insert(node);
    }
    }

    /// Expands every ancestor of `node` so its row is visible.
    pub fn reveal(&mut self, tree: &Tree, node: NodeId) {
    let mut cur = tree.node(node).parent;
    while let Some(p) = cur {
        self.expanded.insert(p);
        cur = tree.node(p).parent;
    }
    }
}

/// Number of files under each node, indexed by `NodeId.0`. A file counts as 1 for itself.
pub fn file_counts(tree: &Tree) -> Vec<u64> {
// Breadth-first order lists every parent before its children; folding it backwards
// rolls counts up without recursion.
let mut order = vec![tree.root()];
let mut i = 0;
while i < order.len() {
    let n = order[i];
    order.extend(tree.children(n));
    i += 1;
}
let len = order.iter().map(|n| n.0 as usize + 1).max().unwrap_or(0);
let mut counts = vec![0u64; len];
for &n in order.iter().rev() {
    let node = tree.node(n);
    if !node.is_dir {
        counts[n.0 as usize] = 1;
    }
    if let Some(p) = node.parent {
        counts[p.0 as usize] += counts[n.0 as usize];
    }
}
counts
}

/// Visible rows, depth first, starting at the tree root. Only expanded folders are walked.
pub fn flatten(tree: &Tree, counts: &[u64], view: &ViewState) -> Vec<Row> {
let mut rows = Vec::new();
let mut stack = vec![(tree.root(), 0u32)];
while let Some((node, depth)) = stack.pop() {
    let has_children = tree.node(node).first_child.is_some();
    let expanded = has_children && view.expanded.contains(&node);
    rows.push(Row { node, depth, expanded, has_children });
    if expanded {
        let mut kids: Vec<NodeId> = tree.children(node).collect();
        kids.sort_by(|&a, &b| compare(tree, counts, view, a, b));
        // Pushed in reverse so the first child pops first.
        stack.extend(kids.into_iter().rev().map(|k| (k, depth + 1)));
    }
}
rows
}

/// Position of `node` in `rows`.
pub fn row_index(rows: &[Row], node: NodeId) -> Option<usize> {
rows.iter().position(|r| r.node == node)
}

/// `part` as a share of `total`, one decimal: `33.3%`. A zero total gives `0.0%`.
pub fn format_percent(part: u64, total: u64) -> String {
if total == 0 {
    return "0.0%".into();
}
// Tenths of a percent, rounded half up, in u128 so sizes near u64::MAX cannot overflow.
let tenths = (u128::from(part) * 1000 + u128::from(total) / 2) / u128::from(total);
format!("{}.{}%", tenths / 10, tenths % 10)
}

/// The deepest rect under the point, if any: the last match, as the layout lists a folder
/// before its descendants. Left and top edges are inside, right and bottom are not.
pub fn hit_test(rects: &[Rect], x: f64, y: f64) -> Option<NodeId> {
rects.iter().rfind(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|r| r.node)
}

/// The selected node's own rect `(x, y, w, h)` in the nested layout. `None` if it is not drawn.
pub fn selection_bounds(rects: &[Rect], selected: NodeId) -> Option<(f64, f64, f64, f64)> {
rects.iter().find(|r| r.node == selected).map(|r| (r.x, r.y, r.w, r.h))
}

/// What opening `node` shows: `(folder to show, node to select)`. A folder with content opens
/// itself; a file opens its folder with the file selected; an empty folder opens nothing.
/// Does not depend on the current view, so a node from an older layout still opens its own folder.
pub fn open_target(tree: &Tree, node: NodeId) -> Option<(NodeId, NodeId)> {
    let n = tree.node(node);
    if n.is_dir { n.first_child.map(|_| (node, node)) } else { n.parent.map(|p| (p, node)) }
}

/// The folder the treemap must show so that `selected` is drawn in it: the current view if
/// `selected` is the view or under it, otherwise the folder of `selected` (the scan root shows itself).
pub fn view_for_selection(tree: &Tree, view_root: NodeId, selected: NodeId) -> NodeId {
    let mut n = Some(selected);
    while let Some(c) = n {
        if c == view_root {
            return view_root;
        }
        n = tree.node(c).parent;
    }
    tree.node(selected).parent.unwrap_or(selected)
}

/// The node whose rect outlines `selected`: itself if drawn, else its nearest drawn folder.
/// `None` if nothing on its chain of folders is drawn.
pub fn outline_node(tree: &Tree, rects: &[Rect], selected: NodeId) -> Option<NodeId> {
    let mut n = Some(selected);
    while let Some(c) = n {
        if rects.iter().any(|r| r.node == c) {
            return Some(c);
        }
        n = tree.node(c).parent;
    }
    None
}

/// How the treemap moves when it shows folder `to` in place of the picture of `shown`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ZoomWay {
    /// `to` is in the picture: the picture zooms into its rect `(x, y, w, h)`.
    In((f64, f64, f64, f64)),
    /// `to` holds the shown folder: when its layout lands, the old picture shrinks into the rect
    /// of this folder (the one the picture showed).
    Out(NodeId),
    /// No motion: the same folder, a folder in another branch, a folder too small to draw, or no picture.
    Still,
}

/// How the treemap moves from the picture of `shown`, drawn as `rects`, to folder `to`: in, when
/// `to` is under `shown` and drawn; out, when `to` is above `shown`; else not at all.
pub fn zoom_way(tree: &Tree, shown: Option<NodeId>, rects: &[Rect], to: NodeId) -> ZoomWay {
    let Some(shown) = shown.filter(|&s| s != to) else {
        return ZoomWay::Still;
    };
    let above = |low: NodeId, high: NodeId| std::iter::successors(tree.node(low).parent, |&p| tree.node(p).parent).any(|p| p == high);
    if above(to, shown) {
        selection_bounds(rects, to).map_or(ZoomWay::Still, ZoomWay::In)
    } else if above(shown, to) {
        ZoomWay::Out(shown)
    } else {
        ZoomWay::Still
    }
}

/// Where the selection and the treemap go after a removal (`after_removal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), expect(dead_code, reason = "made by remove_node, which the context menu calls (P4)"))]
pub struct AfterRemoval {
    /// The node to select: the folder the removed node was in.
    pub select: NodeId,
    /// The folder the treemap must show now, or `None` to keep the shown one.
    pub view: Option<NodeId>,
}

/// Where the selection and the treemap go after `r` was removed from `tree` (call it after
/// `Tree::remove`) while the treemap shows `view_root`: the parent is selected; the treemap
/// shows the parent only when the removal took the view root with it.
#[cfg_attr(not(test), expect(dead_code, reason = "called by remove_node, which the context menu calls (P4)"))]
pub fn after_removal(tree: &Tree, view_root: NodeId, r: &scan::Removed) -> AfterRemoval {
    // The view root is detached only when it or one of its ancestors was removed.
    AfterRemoval { select: r.parent, view: (!tree.is_attached(view_root)).then_some(r.parent) }
}

/// Map text for a folder with no file that has a size.
pub const NO_SIZES: &str = "No files with a size in this folder.";
/// Map text for a folder whose files are all too small to draw at this map size.
pub const TOO_SMALL: &str = "The files in this folder are too small to show.";

/// The text the treemap shows over the layout `rects` of `root`, or "" for none. A map with no
/// area (`has_area` false) shows no text.
pub fn map_note(tree: &Tree, root: NodeId, rects: &[Rect], has_area: bool) -> &'static str {
    match (has_area, rects.is_empty(), tree.node(root).size) {
        (false, _, _) | (true, false, _) => "",
        (true, true, 0) => NO_SIZES,
        (true, true, _) => TOO_SMALL,
    }
}

/// The label of folder `node` on its frame: (name with a trailing `\`, size), as
/// `("Local\\", "306.0 GB")`.
pub fn frame_label(tree: &Tree, node: NodeId) -> (String, String) {
    (format!("{}\\", display_name(tree.name(node))), crate::model::format_size(tree.node(node).size))
}

/// The label of file `node` on its tile: (name, size), as `("pagefile.sys", "64.0 GB")`. The
/// name is kept unchanged; the window cuts it with "…" when it does not fit.
pub fn file_label(tree: &Tree, node: NodeId) -> (String, String) {
    (display_name(tree.name(node)).into_owned(), crate::model::format_size(tree.node(node).size))
}

/// Text of the crumb that stands for the folders `crumbs` leaves out.
pub const CRUMB_GAP: &str = "…";
/// Most crumbs shown with no gap.
const CRUMB_MAX: usize = 6;
/// Folders shown after the gap, down to the view.
const CRUMB_TAIL: usize = 4;

/// The crumbs above the treemap for `view_root`: its folders from the scan root down, each with
/// the node it opens. The scan root shows its path without a trailing `\` (`"C:\"` -> `"C:"`).
/// A chain of more than 6 shows the first, a `CRUMB_GAP` crumb (`None`, opens nothing) and the
/// last 4. Names are kept unchanged; the window elides them.
pub fn crumbs(tree: &Tree, view_root: NodeId) -> Vec<(Option<NodeId>, String)> {
    let mut chain: Vec<NodeId> = std::iter::successors(Some(view_root), |&n| tree.node(n).parent).collect();
    chain.reverse();
    let text = |n: NodeId| {
        let name = tree.name(n);
        let root_text = name.trim_end_matches(['\\', '/']);
        display_name(if n == chain[0] && !root_text.is_empty() { root_text } else { name }).into_owned()
    };
    let mut out: Vec<(Option<NodeId>, String)> = Vec::new();
    if chain.len() > CRUMB_MAX {
        out.push((Some(chain[0]), text(chain[0])));
        out.push((None, CRUMB_GAP.to_string()));
        out.extend(chain[chain.len() - CRUMB_TAIL..].iter().map(|&n| (Some(n), text(n))));
    } else {
        out.extend(chain.iter().map(|&n| (Some(n), text(n))));
    }
    out
}

/// The two lines of the tooltip of `node`: its full path, then its size and its file type (a
/// file) or the number of files under it (a folder), as `("C:\\Windows\\a.dll", "12.0 MB. DLL
/// file.")` or `("C:\\Users", "306.0 GB. 1,234 files.")`. Every name of the path goes through
/// `display_name`; the file type comes from the stored name, so a hidden character cannot change it.
pub fn tooltip_lines(tree: &Tree, counts: &[u64], node: NodeId) -> (String, String) {
    let n = tree.node(node);
    let what = if n.is_dir {
        let files = counts[node.0 as usize];
        format!("{} {}.", crate::model::group_digits(files), if files == 1 { "file" } else { "files" })
    } else {
        match treemap::filetype::extension(tree.name(node)) {
            Some(ext) => format!("{} file.", ext.to_ascii_uppercase()),
            None => "File with no extension.".to_string(),
        }
    };
    let path = display_name(&tree.path(node).to_string_lossy()).into_owned();
    (path, format!("{}. {what}", crate::model::format_size(n.size)))
}

/// Where a tooltip `size` long starts on one axis of a map `extent` long, for the pointer at
/// `pointer` (all in logical pixels): `gap` after the pointer when it fits, else `TIP_FLIP_GAP`
/// before it, else as far as it fits inside the map; 0 when it is longer than the map.
pub fn tip_position(pointer: f32, size: f32, extent: f32, gap: f32) -> f32 {
    let (after, before) = (pointer + gap, pointer - TIP_FLIP_GAP - size);
    if after + size <= extent {
        after
    } else if before >= 0.0 {
        before
    } else {
        (extent - size).max(0.0)
    }
}

/// Space between the pointer and a tooltip shown before it (left or above), in logical pixels.
pub const TIP_FLIP_GAP: f32 = 8.0;

/// A share as `format_percent` gives it, in words for a screen reader: `"10.5%"` -> `"10.5 percent"`.
pub fn spoken_percent(percent: &str) -> String {
    format!("{} percent", percent.trim_end_matches('%'))
}

/// What a screen reader says for one row of the file tree, from the row's shown text and its file
/// count: `"target, 18.3 GB, 82.9 percent, 34,140 files"`.
pub fn row_spoken(name: &str, size: &str, percent: &str, files: u64) -> String {
    let noun = if files == 1 { "file" } else { "files" };
    format!("{name}, {size}, {}, {} {noun}", spoken_percent(percent), crate::model::group_digits(files))
}

/// The value of the treemap for a screen reader: the name and size of `node`, the selected node,
/// as `"kernel32.dll, 12.0 MB"`. The name goes through `display_name`.
pub fn map_value(tree: &Tree, node: NodeId) -> String {
    format!("{}, {}", display_name(tree.name(node)), crate::model::format_size(tree.node(node).size))
}

/// Order of two siblings: the chosen column, flipped if descending, then name, then id.
fn compare(tree: &Tree, counts: &[u64], view: &ViewState, a: NodeId, b: NodeId) -> Ordering {
    let name = |n: NodeId| tree.name(n).chars().flat_map(char::to_lowercase);
    let primary = match view.key {
        SortKey::Name => name(a).cmp(name(b)),
        // Siblings share a parent, so share of parent orders like size.
        SortKey::Size | SortKey::Percent => tree.node(a).size.cmp(&tree.node(b).size),
        SortKey::Files => counts[a.0 as usize].cmp(&counts[b.0 as usize]),
    };
    let primary = if view.descending { primary.reverse() } else { primary };
    primary.then_with(|| name(a).cmp(name(b))).then(a.cmp(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan::rollup::{RawEntry, rollup};

    // root(0)
    //   a(1)    a1(4)=100  a2(5)=50  sub(6){ s1(7)=30 }
    //   b(2)    b1(8)=400
    //   c(3)=10 (file)
    //   z.txt(9)=5 (file)
    fn sample() -> Tree {
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
            e(1, 0, "a", 0, true),
            e(2, 0, "b", 0, true),
            e(3, 0, "c", 10, false),
            e(4, 1, "a1", 100, false),
            e(5, 1, "a2", 50, false),
            e(6, 1, "sub", 0, true),
            e(7, 6, "s1", 30, false),
            e(8, 2, "b1", 400, false),
            e(9, 0, "z.txt", 5, false),
        ];
        rollup(0, entries).unwrap().tree
    }

    fn find(tree: &Tree, from: NodeId, name: &str) -> NodeId {
        tree.children(from).find(|&c| tree.name(c) == name).unwrap_or_else(|| panic!("no {name}"))
    }

    fn names(tree: &Tree, rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}", " ".repeat(r.depth as usize), tree.name(r.node))).collect()
    }

    fn rows_with(tree: &Tree, view: &ViewState) -> Vec<String> {
        names(tree, &flatten(tree, &file_counts(tree), view))
    }

    // ---- file_counts ----
    #[test]
    fn file_counts_sample_counts_files_per_node() {
        let t = sample();
        let counts = file_counts(&t);
        let a = find(&t, t.root(), "a");
        let sub = find(&t, a, "sub");
        let c = find(&t, t.root(), "c");
        assert_eq!(counts[t.root().0 as usize], 6);
        assert_eq!(counts[a.0 as usize], 3);
        assert_eq!(counts[sub.0 as usize], 1);
        assert_eq!(counts[c.0 as usize], 1);
    }

    // ---- format_percent ----
    #[test]
    fn format_percent_known_values_rounds_to_one_decimal() {
        let cases: [(u64, u64, &str); 8] = [
            (0, 0, "0.0%"),
            (5, 0, "0.0%"),
            (0, 10, "0.0%"),
            (1, 3, "33.3%"),
            (2, 3, "66.7%"),
            (1, 1, "100.0%"),
            (1, 2000, "0.1%"),
            (1, 3000, "0.0%"),
        ];
        for (p, t, want) in cases {
            assert_eq!(format_percent(p, t), want, "{p}/{t}");
        }
    }

    #[test]
    fn format_percent_huge_values_do_not_overflow() {
        assert_eq!(format_percent(u64::MAX, u64::MAX), "100.0%");
    }

    // ---- ViewState ----
    #[test]
    fn new_view_expands_root_and_sorts_by_size_descending() {
        let t = sample();
        let v = ViewState::new(t.root());
        assert!(v.expanded.contains(&t.root()));
        assert_eq!((v.key, v.descending), (SortKey::Size, true));
    }

    #[test]
    fn sort_by_new_key_uses_default_direction() {
        let t = sample();
        let mut v = ViewState::new(t.root());
        v.sort_by(SortKey::Name);
        assert_eq!((v.key, v.descending), (SortKey::Name, false));
        v.sort_by(SortKey::Files);
        assert_eq!((v.key, v.descending), (SortKey::Files, true));
    }

    #[test]
    fn sort_by_same_key_flips_direction() {
        let t = sample();
        let mut v = ViewState::new(t.root());
        v.sort_by(SortKey::Size);
        assert_eq!((v.key, v.descending), (SortKey::Size, false));
        v.sort_by(SortKey::Size);
        assert!(v.descending);
    }

    #[test]
    fn toggle_folder_expands_then_collapses() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let mut v = ViewState::new(t.root());
        v.toggle(&t, a);
        assert!(v.expanded.contains(&a));
        v.toggle(&t, a);
        assert!(!v.expanded.contains(&a));
    }

    #[test]
    fn toggle_file_does_nothing() {
        let t = sample();
        let c = find(&t, t.root(), "c");
        let mut v = ViewState::new(t.root());
        v.toggle(&t, c);
        assert!(!v.expanded.contains(&c));
    }

    #[test]
    fn reveal_deep_node_expands_all_ancestors() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let sub = find(&t, a, "sub");
        let s1 = find(&t, sub, "s1");
        let mut v = ViewState::new(t.root());
        v.reveal(&t, s1);
        assert!(v.expanded.contains(&a) && v.expanded.contains(&sub));
        assert!(!v.expanded.contains(&s1));
        let rows = flatten(&t, &file_counts(&t), &v);
        assert!(row_index(&rows, s1).is_some());
    }

    // ---- flatten ----
    #[test]
    fn flatten_initial_view_shows_root_and_children_by_size_descending() {
        let t = sample();
        let v = ViewState::new(t.root());
        assert_eq!(rows_with(&t, &v), ["root", " b", " a", " c", " z.txt"]);
    }

    #[test]
    fn flatten_expanded_folder_lists_children_below_it_one_level_deeper() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let mut v = ViewState::new(t.root());
        v.toggle(&t, a);
        assert_eq!(rows_with(&t, &v), ["root", " b", " a", "  a1", "  a2", "  sub", " c", " z.txt"]);
    }

    #[test]
    fn flatten_collapsed_parent_hides_expanded_descendants() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let sub = find(&t, a, "sub");
        let mut v = ViewState::new(t.root());
        v.expanded.insert(sub); // sub expanded, but a collapsed
        assert_eq!(rows_with(&t, &v), ["root", " b", " a", " c", " z.txt"]);
    }

    #[test]
    fn flatten_marks_has_children_and_expanded() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let mut v = ViewState::new(t.root());
        v.toggle(&t, a);
        let rows = flatten(&t, &file_counts(&t), &v);
        let ra = rows[row_index(&rows, a).unwrap()];
        assert!(ra.has_children && ra.expanded);
        let c = find(&t, t.root(), "c");
        let rc = rows[row_index(&rows, c).unwrap()];
        assert!(!rc.has_children && !rc.expanded);
    }

    #[test]
    fn flatten_sort_by_name_ascending_ignores_case_and_flips() {
        let t = sample();
        let mut v = ViewState::new(t.root());
        v.sort_by(SortKey::Name);
        assert_eq!(rows_with(&t, &v), ["root", " a", " b", " c", " z.txt"]);
        v.sort_by(SortKey::Name);
        assert_eq!(rows_with(&t, &v), ["root", " z.txt", " c", " b", " a"]);
    }

    #[test]
    fn flatten_sort_by_size_ascending_puts_smallest_first() {
        let t = sample();
        let mut v = ViewState::new(t.root());
        v.sort_by(SortKey::Size);
        assert_eq!(rows_with(&t, &v), ["root", " z.txt", " c", " a", " b"]);
    }

    #[test]
    fn flatten_sort_by_percent_matches_size_order_among_siblings() {
        let t = sample();
        let mut v = ViewState::new(t.root());
        v.sort_by(SortKey::Percent);
        assert_eq!(rows_with(&t, &v), ["root", " b", " a", " c", " z.txt"]);
    }

    #[test]
    fn flatten_sort_by_files_descending_breaks_ties_by_name() {
        let t = sample();
        let mut v = ViewState::new(t.root());
        v.sort_by(SortKey::Files);
        // a=3 files, then b, c, z.txt with 1 each (name order)
        assert_eq!(rows_with(&t, &v), ["root", " a", " b", " c", " z.txt"]);
    }

    #[test]
    fn flatten_sort_applies_inside_expanded_folders() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let mut v = ViewState::new(t.root());
        v.toggle(&t, a);
        v.sort_by(SortKey::Name);
        let got = rows_with(&t, &v);
        assert_eq!(&got[1..5], [" a", "  a1", "  a2", "  sub"]);
    }

    #[test]
    fn row_index_missing_node_is_none() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let a1 = find(&t, a, "a1");
        let rows = flatten(&t, &file_counts(&t), &ViewState::new(t.root()));
        assert_eq!(row_index(&rows, a1), None);
        assert_eq!(row_index(&rows, t.root()), Some(0));
    }

    // ---- hit_test ----
    fn rect(n: u32, x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { node: NodeId(n), x, y, w, h }
    }

    #[test]
    fn hit_test_point_inside_returns_that_node() {
        let rects = [rect(1, 0.0, 0.0, 50.0, 100.0), rect(2, 50.0, 0.0, 50.0, 100.0)];
        assert_eq!(hit_test(&rects, 10.0, 10.0), Some(NodeId(1)));
        assert_eq!(hit_test(&rects, 75.0, 99.0), Some(NodeId(2)));
    }

    #[test]
    fn hit_test_shared_edge_belongs_to_the_right_or_lower_rect() {
        let rects = [rect(1, 0.0, 0.0, 50.0, 100.0), rect(2, 50.0, 0.0, 50.0, 100.0)];
        assert_eq!(hit_test(&rects, 50.0, 0.0), Some(NodeId(2)));
        assert_eq!(hit_test(&rects, 0.0, 0.0), Some(NodeId(1)));
    }

    #[test]
    fn hit_test_outside_all_rects_is_none() {
        let rects = [rect(1, 0.0, 0.0, 50.0, 100.0)];
        assert_eq!(hit_test(&rects, 50.0, 10.0), None);
        assert_eq!(hit_test(&rects, 10.0, 100.0), None);
        assert_eq!(hit_test(&rects, -1.0, 10.0), None);
        assert_eq!(hit_test(&[], 1.0, 1.0), None);
    }

    #[test]
    fn hit_test_nested_rects_returns_the_deepest_rect() {
        // Layout order: a folder before its descendants.
        let rects = [rect(1, 0.0, 0.0, 100.0, 100.0), rect(4, 0.0, 0.0, 50.0, 100.0), rect(6, 50.0, 0.0, 50.0, 50.0)];
        assert_eq!(hit_test(&rects, 10.0, 10.0), Some(NodeId(4)));
        assert_eq!(hit_test(&rects, 60.0, 10.0), Some(NodeId(6)));
        assert_eq!(hit_test(&rects, 60.0, 60.0), Some(NodeId(1)));
    }

    // ---- selection_bounds ----
    #[test]
    fn selection_bounds_file_is_its_own_rect() {
        let t = sample();
        let c = find(&t, t.root(), "c");
        let rects = [rect(c.0, 5.0, 6.0, 7.0, 8.0)];
        assert_eq!(selection_bounds(&rects, c), Some((5.0, 6.0, 7.0, 8.0)));
    }

    #[test]
    fn selection_bounds_folder_is_its_own_rect() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let a1 = find(&t, a, "a1");
        let c = find(&t, t.root(), "c");
        let rects =
            [rect(a.0, 0.0, 0.0, 20.0, 30.0), rect(a1.0, 0.0, 0.0, 10.0, 10.0), rect(c.0, 100.0, 100.0, 5.0, 5.0)];
        assert_eq!(selection_bounds(&rects, a), Some((0.0, 0.0, 20.0, 30.0)));
    }

    #[test]
    fn selection_bounds_node_not_drawn_is_none() {
        let t = sample();
        let b = find(&t, t.root(), "b");
        let c = find(&t, t.root(), "c");
        assert_eq!(selection_bounds(&[rect(c.0, 0.0, 0.0, 1.0, 1.0)], b), None);
    }

    fn is_under(tree: &Tree, mut n: NodeId, anc: NodeId) -> bool {
        loop {
            if n == anc {
                return true;
            }
            match tree.node(n).parent {
                Some(p) => n = p,
                None => return false,
            }
        }
    }

    /// From the P3 verifier probe (F2): the outline of a selected folder, over the real layout,
    /// must not cover a rect of another folder.
    #[test]
    fn selection_bounds_folder_outline_covers_only_its_own_rects() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let rects = treemap::layout(&t, t.root(), 400, 300);

        let (x, y, w, h) = selection_bounds(&rects, a).expect("folder a is drawn");

        let foreign: Vec<String> = rects
            .iter()
            .filter(|r| !is_under(&t, r.node, a) && !is_under(&t, a, r.node))
            .filter(|r| r.x < x + w - 0.5 && r.x + r.w > x + 0.5 && r.y < y + h - 0.5 && r.y + r.h > y + 0.5)
            .map(|r| format!("{} ({:.0}x{:.0} at {:.0},{:.0})", t.name(r.node), r.w, r.h, r.x, r.y))
            .collect();
        assert!(foreign.is_empty(), "outline of folder 'a' ({x:.0},{y:.0} {w:.0}x{h:.0}) covers {foreign:?}");
    }

    // ---- framed layout: the existing hit, outline and neighbour rules ----
    fn framed_sample() -> (Tree, Vec<Rect>) {
        let t = sample();
        let rects = treemap::layout_framed(&t, t.root(), 400, 300, treemap::Chrome::scaled(1.0));
        (t, rects)
    }

    fn rect_of(rects: &[Rect], n: NodeId) -> Rect {
        *rects.iter().find(|r| r.node == n).unwrap_or_else(|| panic!("{n:?} is not drawn"))
    }

    #[test]
    fn hit_test_framed_header_band_is_the_folder() {
        let (t, rects) = framed_sample();
        let b = rect_of(&rects, find(&t, t.root(), "b"));
        assert!(treemap::Chrome::scaled(1.0).has_header(&b), "{b:?}");

        assert_eq!(hit_test(&rects, b.x + b.w / 2.0, b.y + 8.0), Some(b.node));
    }

    #[test]
    fn hit_test_framed_inset_gap_is_the_folder() {
        let (t, rects) = framed_sample();
        let b = rect_of(&rects, find(&t, t.root(), "b"));

        assert_eq!(hit_test(&rects, b.x + 1.0, b.y + b.h / 2.0), Some(b.node));
    }

    #[test]
    fn hit_test_framed_tile_is_the_file() {
        let (t, rects) = framed_sample();
        let b1 = rect_of(&rects, find(&t, find(&t, t.root(), "b"), "b1"));

        assert_eq!(hit_test(&rects, b1.x + b1.w / 2.0, b1.y + b1.h / 2.0), Some(b1.node));
    }

    #[test]
    fn selection_bounds_framed_folder_outline_covers_only_its_own_rects() {
        let (t, rects) = framed_sample();
        let a = find(&t, t.root(), "a");

        let (x, y, w, h) = selection_bounds(&rects, a).expect("folder a is drawn");

        let foreign = rects
            .iter()
            .filter(|r| !is_under(&t, r.node, a) && !is_under(&t, a, r.node))
            .filter(|r| r.x < x + w - 0.5 && r.x + r.w > x + 0.5 && r.y < y + h - 0.5 && r.y + r.h > y + 0.5)
            .count();
        assert_eq!(foreign, 0);
    }

    #[test]
    fn neighbour_rect_framed_grid_gives_the_same_moves_as_the_plain_grid() {
        // Four equal files: a 2 x 2 grid. Framed at 104 x 104, the inset leaves the plain 100 x 100 grid.
        let e = |id: u64, size: u64| RawEntry { id, parent: 0, name: format!("f{id}"), size, allocated: size, is_dir: id == 0 };
        let t = rollup(0, vec![e(0, 0), e(1, 5), e(2, 5), e(3, 5), e(4, 5)]).unwrap().tree;
        let plain = treemap::layout(&t, t.root(), 100, 100);
        let framed = treemap::layout_framed(&t, t.root(), 104, 104, treemap::Chrome::scaled(1.0));
        let dirs = [crate::nav::Dir::Left, crate::nav::Dir::Right, crate::nav::Dir::Up, crate::nav::Dir::Down];

        let moves = |rects: &[Rect]| -> Vec<Option<NodeId>> {
            t.children(t.root())
                .flat_map(|n| dirs.iter().map(move |&d| (n, d)))
                .map(|(n, d)| crate::nav::neighbour_rect(rects, Some(n), d))
                .collect()
        };

        assert_eq!(plain.len(), 4);
        assert_eq!(moves(&framed), moves(&plain));
    }

    // ---- open_target ----
    // root(0): a(1){ a1(3) }, e(2) empty folder, c(4) file
    fn with_empty_folder() -> Tree {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
        };
        rollup(
            0,
            vec![e(0, 0, "root", 0, true), e(1, 0, "a", 0, true), e(2, 0, "e", 0, true), e(3, 1, "a1", 10, false), e(4, 0, "c", 5, false)],
        )
        .unwrap()
        .tree
    }

    #[test]
    fn open_target_folder_with_content_opens_and_selects_itself() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        assert_eq!(open_target(&t, a), Some((a, a)));
    }

    #[test]
    fn open_target_empty_folder_is_none() {
        let t = with_empty_folder();
        assert_eq!(open_target(&t, find(&t, t.root(), "e")), None);
    }

    #[test]
    fn open_target_file_in_a_subfolder_opens_its_folder_and_selects_it() {
        let t = sample();
        let sub = find(&t, find(&t, t.root(), "a"), "sub");
        let s1 = find(&t, sub, "s1");
        assert_eq!(open_target(&t, s1), Some((sub, s1)));
    }

    #[test]
    fn open_target_file_under_the_root_opens_the_root_and_selects_it() {
        let t = sample();
        let c = find(&t, t.root(), "c");
        assert_eq!(open_target(&t, c), Some((t.root(), c)));
    }

    #[test]
    fn open_target_root_opens_the_root() {
        let t = sample();
        assert_eq!(open_target(&t, t.root()), Some((t.root(), t.root())));
    }

    #[test]
    fn open_target_node_outside_the_shown_folder_opens_its_own_folder() {
        // The map shows `a`, but the rects of an older layout hit `b1` in `b`.
        let t = sample();
        let b = find(&t, t.root(), "b");
        let b1 = find(&t, b, "b1");
        assert_eq!(open_target(&t, b1), Some((b, b1)));
    }

    // ---- after_removal ----
    /// `after_removal` for the treemap at `view` after `gone` was removed from `sample()`.
    fn removal(view: &[&str], gone: &[&str]) -> (AfterRemoval, Tree, NodeId) {
        let mut t = sample();
        let path = |names: &[&str]| names.iter().fold(t.root(), |n, name| find(&t, n, name));
        let (view, gone) = (path(view), path(gone));
        let r = t.remove(gone).unwrap();
        (after_removal(&t, view, &r), t, gone)
    }

    #[test]
    fn after_removal_inside_the_view_selects_the_parent_and_keeps_the_view() {
        for gone in [&["a", "sub"][..], &["a", "sub", "s1"]] {
            let (after, t, _) = removal(&["a"], gone);

            let parent = gone[..gone.len() - 1].iter().fold(t.root(), |n, name| find(&t, n, name));
            assert_eq!(after, AfterRemoval { select: parent, view: None }, "removed {gone:?}");
        }
    }

    #[test]
    fn after_removal_of_the_view_root_shows_its_parent() {
        let (after, t, _) = removal(&["a", "sub"], &["a", "sub"]);

        let a = find(&t, t.root(), "a");
        assert_eq!(after, AfterRemoval { select: a, view: Some(a) });
    }

    #[test]
    fn after_removal_of_an_ancestor_of_the_view_root_shows_the_ancestor_parent() {
        let (after, t, _) = removal(&["a", "sub"], &["a"]);

        assert_eq!(after, AfterRemoval { select: t.root(), view: Some(t.root()) });
    }

    #[test]
    fn after_removal_outside_the_view_selects_the_parent_and_keeps_the_view() {
        let (after, t, _) = removal(&["a"], &["b", "b1"]);

        assert_eq!(after, AfterRemoval { select: find(&t, t.root(), "b"), view: None });
    }

    // ---- view_for_selection ----
    #[test]
    fn view_for_selection_descendant_keeps_the_view() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let s1 = find(&t, find(&t, a, "sub"), "s1");
        assert_eq!(view_for_selection(&t, a, s1), a);
    }

    #[test]
    fn view_for_selection_the_view_itself_keeps_the_view() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        assert_eq!(view_for_selection(&t, a, a), a);
    }

    #[test]
    fn view_for_selection_ancestor_shows_its_parent() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let sub = find(&t, a, "sub");
        assert_eq!(view_for_selection(&t, sub, a), t.root());
    }

    #[test]
    fn view_for_selection_cousin_folder_shows_its_parent() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let b = find(&t, t.root(), "b");
        assert_eq!(view_for_selection(&t, b, find(&t, a, "sub")), a);
    }

    #[test]
    fn view_for_selection_cousin_file_shows_its_parent() {
        let t = sample();
        let sub = find(&t, find(&t, t.root(), "a"), "sub");
        let b = find(&t, t.root(), "b");
        assert_eq!(view_for_selection(&t, b, find(&t, sub, "s1")), sub);
    }

    #[test]
    fn view_for_selection_scan_root_shows_the_root() {
        let t = sample();
        let sub = find(&t, find(&t, t.root(), "a"), "sub");
        assert_eq!(view_for_selection(&t, sub, t.root()), t.root());
    }

    // ---- outline_node ----
    #[test]
    fn outline_node_drawn_node_is_itself() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let a1 = find(&t, a, "a1");
        let rects = [rect(a.0, 0.0, 0.0, 50.0, 50.0), rect(a1.0, 2.0, 18.0, 20.0, 20.0)];
        assert_eq!(outline_node(&t, &rects, a1), Some(a1));
    }

    #[test]
    fn outline_node_pruned_node_is_its_nearest_drawn_folder() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let s1 = find(&t, find(&t, a, "sub"), "s1");
        let rects = [rect(a.0, 0.0, 0.0, 50.0, 50.0), rect(find(&t, a, "a1").0, 2.0, 18.0, 20.0, 20.0)];
        assert_eq!(outline_node(&t, &rects, s1), Some(a));
    }

    #[test]
    fn outline_node_nothing_drawn_on_the_chain_is_none() {
        let t = sample();
        let b = find(&t, t.root(), "b");
        let s1 = find(&t, find(&t, find(&t, t.root(), "a"), "sub"), "s1");
        let rects = [rect(b.0, 0.0, 0.0, 50.0, 50.0), rect(find(&t, b, "b1").0, 2.0, 18.0, 20.0, 20.0)];
        assert_eq!(outline_node(&t, &rects, s1), None);
    }

    // ---- zoom_way ----
    /// The picture of the scan root with `a`, `sub` and `b` drawn (not `s1`).
    fn root_picture(t: &Tree) -> Vec<Rect> {
        let a = find(t, t.root(), "a");
        let sub = find(t, a, "sub");
        let b = find(t, t.root(), "b");
        vec![rect(a.0, 2.0, 2.0, 100.0, 80.0), rect(sub.0, 4.0, 18.0, 40.0, 30.0), rect(b.0, 104.0, 2.0, 60.0, 80.0)]
    }

    #[test]
    fn zoom_way_child_folder_in_the_picture_zooms_into_its_rect() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        assert_eq!(zoom_way(&t, Some(t.root()), &root_picture(&t), a), ZoomWay::In((2.0, 2.0, 100.0, 80.0)));
    }

    #[test]
    fn zoom_way_deeper_folder_in_the_picture_zooms_into_its_rect() {
        let t = sample();
        let sub = find(&t, find(&t, t.root(), "a"), "sub");
        assert_eq!(zoom_way(&t, Some(t.root()), &root_picture(&t), sub), ZoomWay::In((4.0, 18.0, 40.0, 30.0)));
    }

    #[test]
    fn zoom_way_folder_under_the_picture_that_is_not_drawn_does_not_move() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let rects = [rect(find(&t, t.root(), "b").0, 0.0, 0.0, 50.0, 50.0)];
        assert_eq!(zoom_way(&t, Some(t.root()), &rects, a), ZoomWay::Still);
    }

    #[test]
    fn zoom_way_parent_zooms_out_of_the_shown_folder() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let sub = find(&t, a, "sub");
        assert_eq!(zoom_way(&t, Some(sub), &[], a), ZoomWay::Out(sub));
    }

    #[test]
    fn zoom_way_scan_root_from_a_deep_folder_zooms_out_of_the_shown_folder() {
        let t = sample();
        let sub = find(&t, find(&t, t.root(), "a"), "sub");
        assert_eq!(zoom_way(&t, Some(sub), &[], t.root()), ZoomWay::Out(sub));
    }

    #[test]
    fn zoom_way_same_folder_does_not_move() {
        let t = sample();
        assert_eq!(zoom_way(&t, Some(t.root()), &root_picture(&t), t.root()), ZoomWay::Still);
    }

    #[test]
    fn zoom_way_folder_in_another_branch_does_not_move() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let b = find(&t, t.root(), "b");
        let rects = [rect(find(&t, a, "sub").0, 0.0, 0.0, 50.0, 50.0), rect(b.0, 50.0, 0.0, 50.0, 50.0)];
        assert_eq!(zoom_way(&t, Some(a), &rects, b), ZoomWay::Still);
    }

    #[test]
    fn zoom_way_no_picture_does_not_move() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        assert_eq!(zoom_way(&t, None, &root_picture(&t), a), ZoomWay::Still);
    }

    // ---- frame_label ----
    /// root(0){ folder `name`(1){ f(2) of `size` bytes } }
    fn one_folder(name: &str, size: u64) -> (Tree, NodeId) {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
        };
        let t = rollup(0, vec![e(0, 0, "root", 0, true), e(1, 0, name, 0, true), e(2, 1, "f", size, false)]).unwrap().tree;
        let folder = find(&t, t.root(), name);
        (t, folder)
    }

    #[test]
    fn frame_label_folder_is_name_with_backslash_and_size() {
        let (t, local) = one_folder("Local", 306 * (1 << 30));
        assert_eq!(frame_label(&t, local), ("Local\\".to_string(), "306.0 GB".to_string()));
    }

    #[test]
    fn frame_label_unicode_name_is_kept_unchanged() {
        let (t, folder) = one_folder("Données 日本語 ✓", 2048);
        assert_eq!(frame_label(&t, folder), ("Données 日本語 ✓\\".to_string(), "2.0 KB".to_string()));
    }

    #[test]
    fn frame_label_name_with_a_right_to_left_override_shows_a_placeholder() {
        let (t, folder) = one_folder("invoice\u{202E}fdp", 1);
        assert_eq!(frame_label(&t, folder).0, "invoice\u{FFFD}fdp\\");
    }

    // ---- file_label ----
    /// root(0){ file `name`(1) of `size` bytes }
    fn one_file(name: &str, size: u64) -> (Tree, NodeId) {
        let e = |id: u64, name: &str, size: u64, is_dir: bool| RawEntry { id, parent: 0, name: name.into(), size, allocated: size, is_dir };
        let t = rollup(0, vec![e(0, "root", 0, true), e(1, name, size, false)]).unwrap().tree;
        let file = find(&t, t.root(), name);
        (t, file)
    }

    #[test]
    fn file_label_file_is_name_and_size() {
        let (t, file) = one_file("pagefile.sys", 64 * (1 << 30));
        assert_eq!(file_label(&t, file), ("pagefile.sys".to_string(), "64.0 GB".to_string()));
    }

    #[test]
    fn file_label_unicode_name_is_kept_unchanged() {
        let (t, file) = one_file("Données 日本語 ✓.txt", 2048);
        assert_eq!(file_label(&t, file), ("Données 日本語 ✓.txt".to_string(), "2.0 KB".to_string()));
    }

    #[test]
    fn file_label_200_character_name_is_kept_whole() {
        let name = format!("{}.bin", "x".repeat(196));
        let (t, file) = one_file(&name, 5);
        assert_eq!(file_label(&t, file), (name, "5 B".to_string()));
    }

    #[test]
    fn file_label_zero_byte_file_shows_zero_bytes() {
        let (t, file) = one_file("empty.log", 0);
        assert_eq!(file_label(&t, file), ("empty.log".to_string(), "0 B".to_string()));
    }

    #[test]
    fn file_label_name_with_a_right_to_left_override_shows_a_placeholder() {
        let (t, file) = one_file("invoice\u{202E}fdp.exe", 1);
        assert_eq!(file_label(&t, file).0, "invoice\u{FFFD}fdp.exe");
    }

    // ---- crumbs ----
    /// A chain of folders: the root named `root_name`, then one folder per name in `names`, each
    /// in the one before, and a file "f" in the last. The folders from the root down.
    fn chain(root_name: &str, names: &[&str]) -> (Tree, Vec<NodeId>) {
        let dir = |id: u64, parent: u64, name: &str| RawEntry { id, parent, name: name.into(), size: 0, allocated: 0, is_dir: true };
        let mut entries = vec![dir(0, 0, root_name)];
        for (i, name) in names.iter().enumerate() {
            entries.push(dir(i as u64 + 1, i as u64, name));
        }
        let last = names.len() as u64;
        entries.push(RawEntry { id: last + 1, parent: last, name: "f".into(), size: 1, allocated: 1, is_dir: false });
        let t = rollup(0, entries).unwrap().tree;
        let mut folders = vec![t.root()];
        for name in names {
            let next = find(&t, *folders.last().unwrap(), name);
            folders.push(next);
        }
        (t, folders)
    }

    /// The crumbs expected for the folders `shown` (index into `folders`, `None` = the gap) with `texts`.
    fn expected(folders: &[NodeId], shown: &[Option<usize>], texts: &[&str]) -> Vec<(Option<NodeId>, String)> {
        shown.iter().zip(texts).map(|(i, s)| (i.map(|i| folders[i]), (*s).to_string())).collect()
    }

    #[test]
    fn crumbs_drive_root_shows_the_drive_without_its_backslash() {
        let (t, f) = chain("C:\\", &[]);
        assert_eq!(crumbs(&t, t.root()), expected(&f, &[Some(0)], &["C:"]));
    }

    #[test]
    fn crumbs_folder_scan_root_keeps_its_full_path() {
        let (t, f) = chain("C:\\Users\\mynd\\DEV", &[]);
        assert_eq!(crumbs(&t, t.root()), expected(&f, &[Some(0)], &["C:\\Users\\mynd\\DEV"]));
    }

    #[test]
    fn crumbs_tree_with_only_the_root_is_one_crumb_for_the_root() {
        let t = rollup(0, vec![RawEntry { id: 0, parent: 0, name: "D:\\".into(), size: 0, allocated: 0, is_dir: true }])
            .unwrap()
            .tree;
        assert_eq!(crumbs(&t, t.root()), vec![(Some(t.root()), "D:".to_string())]);
    }

    #[test]
    fn crumbs_nested_folder_lists_every_folder_from_the_root() {
        let (t, f) = chain("C:\\", &["Users", "mynd"]);
        assert_eq!(crumbs(&t, f[2]), expected(&f, &[Some(0), Some(1), Some(2)], &["C:", "Users", "mynd"]));
    }

    #[test]
    fn crumbs_six_levels_shows_all_six() {
        let (t, f) = chain("C:\\", &["a", "b", "c", "d", "e"]);
        let all: Vec<Option<usize>> = (0..6).map(Some).collect();
        assert_eq!(crumbs(&t, f[5]), expected(&f, &all, &["C:", "a", "b", "c", "d", "e"]));
    }

    #[test]
    fn crumbs_seven_levels_shows_the_first_the_gap_and_the_last_four() {
        let (t, f) = chain("C:\\", &["a", "b", "c", "d", "e", "g"]);
        let shown = [Some(0), None, Some(3), Some(4), Some(5), Some(6)];
        assert_eq!(crumbs(&t, f[6]), expected(&f, &shown, &["C:", CRUMB_GAP, "c", "d", "e", "g"]));
    }

    #[test]
    fn crumbs_nine_levels_shows_the_first_the_gap_and_the_last_four() {
        let (t, f) = chain("C:\\", &["a", "b", "c", "d", "e", "g", "h", "i"]);
        let shown = [Some(0), None, Some(5), Some(6), Some(7), Some(8)];
        assert_eq!(crumbs(&t, f[8]), expected(&f, &shown, &["C:", CRUMB_GAP, "e", "g", "h", "i"]));
    }

    #[test]
    fn crumbs_fifty_levels_deep_shows_six_crumbs_ending_at_the_view() {
        let names: Vec<String> = (1..50).map(|i| format!("d{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (t, f) = chain("C:\\", &refs);
        let shown = [Some(0), None, Some(46), Some(47), Some(48), Some(49)];
        assert_eq!(crumbs(&t, f[49]), expected(&f, &shown, &["C:", CRUMB_GAP, "d46", "d47", "d48", "d49"]));
    }

    #[test]
    fn crumbs_names_with_a_backslash_or_unicode_are_kept_unchanged() {
        let (t, f) = chain("C:\\", &["a\\b", "Données 日本語 ✓"]);
        assert_eq!(crumbs(&t, f[2]), expected(&f, &[Some(0), Some(1), Some(2)], &["C:", "a\\b", "Données 日本語 ✓"]));
    }

    #[test]
    fn crumbs_very_long_name_is_kept_whole() {
        let long = "x".repeat(300);
        let (t, f) = chain("C:\\", &[&long]);
        assert_eq!(crumbs(&t, f[1]), expected(&f, &[Some(0), Some(1)], &["C:", &long]));
    }

    #[test]
    fn crumbs_of_a_file_end_with_the_file_after_its_folders() {
        let (t, f) = chain("C:\\", &["a"]);
        let file = find(&t, f[1], "f");
        assert_eq!(crumbs(&t, file), vec![(Some(f[0]), "C:".into()), (Some(f[1]), "a".into()), (Some(file), "f".into())]);
    }

    // ---- map_note ----
    #[test]
    fn map_note_folder_with_drawn_rects_is_empty() {
        let t = sample();
        let rects = treemap::layout_framed(&t, t.root(), 400, 300, treemap::Chrome::scaled(1.0));
        assert_eq!(map_note(&t, t.root(), &rects, true), "");
    }

    #[test]
    fn map_note_folder_with_no_sizes_says_so() {
        let t = with_empty_folder();
        let e = find(&t, t.root(), "e");
        assert_eq!(map_note(&t, e, &[], true), NO_SIZES);
    }

    /// P8.P2 verify MINOR-1: a folder full of files that are all under 1 px² said it had no sizes.
    #[test]
    fn map_note_folder_whose_files_are_all_pruned_says_they_are_too_small() {
        let t = sample();
        assert_eq!(map_note(&t, t.root(), &[], true), TOO_SMALL);
    }

    #[test]
    fn map_note_map_with_no_area_is_empty() {
        let t = with_empty_folder();
        assert_eq!(map_note(&t, find(&t, t.root(), "e"), &[], false), "");
        assert_eq!(map_note(&t, t.root(), &[], false), "");
    }

    #[test]
    fn crumbs_name_with_a_right_to_left_override_shows_a_placeholder() {
        let (t, f) = chain("C:\\", &["a\u{202E}b", "c\u{200B}d"]);
        assert_eq!(crumbs(&t, f[2]), expected(&f, &[Some(0), Some(1), Some(2)], &["C:", "a\u{FFFD}b", "c\u{FFFD}d"]));
    }

    #[test]
    fn crumbs_scan_root_with_a_control_character_shows_a_placeholder() {
        let (t, f) = chain("D:\\x\u{202E}y", &[]);
        assert_eq!(crumbs(&t, t.root()), expected(&f, &[Some(0)], &["D:\\x\u{FFFD}y"]));
    }

    // ---- tooltip_lines ----
    const MB12: u64 = 12 * 1024 * 1024;

    // C:\ (0)
    //   Windows (1) { System32 (2) { kernel32.dll (3) = 12 MB, README (4) = 5, empty.txt (5) = 0, photo.JpG (6) = 3 } }
    //   one (7) { only.txt (8) = 7 }
    //   none (9) {}
    //   .gitignore (10) = 1
    //   Données 日本語.zip (11) = 2048
    //   invoice\u{202E}fdp.exe (12) = 10
    fn tip_tree() -> Tree {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
        };
        let entries = vec![
            e(0, 0, "C:\\", 0, true),
            e(1, 0, "Windows", 0, true),
            e(2, 1, "System32", 0, true),
            e(3, 2, "kernel32.dll", MB12, false),
            e(4, 2, "README", 5, false),
            e(5, 2, "empty.txt", 0, false),
            e(6, 2, "photo.JpG", 3, false),
            e(7, 0, "one", 0, true),
            e(8, 7, "only.txt", 7, false),
            e(9, 0, "none", 0, true),
            e(10, 0, ".gitignore", 1, false),
            e(11, 0, "Données 日本語.zip", 2048, false),
            e(12, 0, "invoice\u{202E}fdp.exe", 10, false),
        ];
        rollup(0, entries).unwrap().tree
    }

    /// The tooltip lines of the node at `path` (names from the root down) in `tip_tree`.
    fn tip(path: &[&str]) -> (String, String) {
        let t = tip_tree();
        let node = path.iter().fold(t.root(), |n, name| find(&t, n, name));
        tooltip_lines(&t, &file_counts(&t), node)
    }

    fn lines(title: &str, detail: &str) -> (String, String) {
        (title.to_string(), detail.to_string())
    }

    #[test]
    fn tooltip_lines_file_gives_its_path_size_and_file_type() {
        assert_eq!(
            tip(&["Windows", "System32", "kernel32.dll"]),
            lines("C:\\Windows\\System32\\kernel32.dll", "12.0 MB. DLL file.")
        );
    }

    #[test]
    fn tooltip_lines_extension_in_mixed_case_is_upper_case() {
        assert_eq!(tip(&["Windows", "System32", "photo.JpG"]).1, "3 B. JPG file.");
    }

    #[test]
    fn tooltip_lines_file_with_no_extension_says_so() {
        assert_eq!(tip(&["Windows", "System32", "README"]), lines("C:\\Windows\\System32\\README", "5 B. File with no extension."));
    }

    #[test]
    fn tooltip_lines_name_with_only_a_leading_dot_has_no_extension() {
        assert_eq!(tip(&[".gitignore"]).1, "1 B. File with no extension.");
    }

    #[test]
    fn tooltip_lines_zero_size_file_shows_0_b() {
        assert_eq!(tip(&["Windows", "System32", "empty.txt"]).1, "0 B. TXT file.");
    }

    #[test]
    fn tooltip_lines_folder_gives_its_path_size_and_file_count() {
        assert_eq!(tip(&["Windows"]), lines("C:\\Windows", "12.0 MB. 4 files."));
    }

    #[test]
    fn tooltip_lines_scan_root_gives_its_path_and_every_file() {
        assert_eq!(tip(&[]), lines("C:\\", "12.0 MB. 8 files."));
    }

    #[test]
    fn tooltip_lines_folder_with_one_file_says_1_file() {
        assert_eq!(tip(&["one"]).1, "7 B. 1 file.");
    }

    #[test]
    fn tooltip_lines_empty_folder_says_0_files() {
        assert_eq!(tip(&["none"]).1, "0 B. 0 files.");
    }

    #[test]
    fn tooltip_lines_large_file_count_groups_the_digits() {
        let t = tip_tree();
        let windows = find(&t, t.root(), "Windows");
        let mut counts = file_counts(&t);
        counts[windows.0 as usize] = 1_234_567;
        assert_eq!(tooltip_lines(&t, &counts, windows).1, "12.0 MB. 1,234,567 files.");
    }

    #[test]
    fn tooltip_lines_unicode_name_is_kept_unchanged() {
        assert_eq!(tip(&["Données 日本語.zip"]), lines("C:\\Données 日本語.zip", "2.0 KB. ZIP file."));
    }

    #[test]
    fn tooltip_lines_long_name_is_kept_whole() {
        let long = format!("{}.bin", "Long file name ".repeat(20));
        let e =|id: u64, parent: u64, name: &str, is_dir: bool| RawEntry { id, parent, name: name.into(), size: 1, allocated: 1, is_dir };
        let t = rollup(0, vec![e(0, 0, "C:\\", true), e(1, 0, &long, false)]).unwrap().tree;
        let node = find(&t, t.root(), &long);
        assert_eq!(tooltip_lines(&t, &file_counts(&t), node).0, format!("C:\\{long}"));
    }

    #[test]
    fn tooltip_lines_right_to_left_override_in_a_file_name_shows_a_placeholder_and_the_real_type() {
        assert_eq!(tip(&["invoice\u{202E}fdp.exe"]), lines("C:\\invoice\u{FFFD}fdp.exe", "10 B. EXE file."));
    }

    #[test]
    fn tooltip_lines_control_character_in_a_folder_of_the_path_shows_a_placeholder() {
        let (t, folders) = chain("C:\\", &["d\u{202E}x"]);
        let file = find(&t, folders[1], "f");
        assert_eq!(tooltip_lines(&t, &file_counts(&t), file).0, "C:\\d\u{FFFD}x\\f");
    }

    #[test]
    fn tooltip_lines_control_character_in_the_scan_root_shows_a_placeholder() {
        let (t, _) = chain("D:\\x\u{202E}y", &[]);
        assert_eq!(tooltip_lines(&t, &file_counts(&t), t.root()).0, "D:\\x\u{FFFD}y");
    }

    // ---- tip_position ----
    /// Gap after the pointer used by the tests, as the window uses for x.
    const GAP: f32 = 16.0;

    #[test]
    fn tip_position_tip_that_fits_after_the_pointer_goes_after_it() {
        assert_eq!(tip_position(100.0, 50.0, 400.0, GAP), 116.0);
    }

    #[test]
    fn tip_position_tip_that_ends_on_the_map_edge_still_goes_after_the_pointer() {
        assert_eq!(tip_position(334.0, 50.0, 400.0, GAP), 350.0);
    }

    #[test]
    fn tip_position_tip_one_pixel_past_the_edge_goes_before_the_pointer() {
        assert_eq!(tip_position(335.0, 50.0, 400.0, GAP), 335.0 - TIP_FLIP_GAP - 50.0);
    }

    #[test]
    fn tip_position_tip_before_the_pointer_may_start_on_the_map_edge() {
        assert_eq!(tip_position(58.0, 50.0, 100.0, GAP), 0.0);
    }

    #[test]
    fn tip_position_tip_that_fits_on_neither_side_ends_on_the_far_edge() {
        assert_eq!(tip_position(40.0, 50.0, 100.0, GAP), 50.0);
    }

    #[test]
    fn tip_position_tip_as_long_as_the_map_starts_at_0() {
        assert_eq!(tip_position(30.0, 100.0, 100.0, GAP), 0.0);
    }

    #[test]
    fn tip_position_tip_longer_than_the_map_starts_at_0() {
        assert_eq!(tip_position(10.0, 150.0, 100.0, GAP), 0.0);
    }

    #[test]
    fn tip_position_every_tip_that_fits_stays_inside_the_map() {
        let mut checked = 0;
        for extent in [0u16, 1, 50, 100, 101] {
            for size in 0..=extent + 20 {
                for pointer in 0..=extent {
                    let (p, s, e) = (f32::from(pointer), f32::from(size), f32::from(extent));
                    let at = tip_position(p, s, e, GAP);
                    let inside = at >= 0.0 && (s > e || at + s <= e);
                    assert!(inside, "pointer {p} size {s} map {e} -> {at}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 10_000, "{checked}");
    }

    // ---- spoken text ----
    #[test]
    fn spoken_percent_sign_becomes_the_word_percent() {
        assert_eq!(spoken_percent("10.5%"), "10.5 percent");
    }

    #[test]
    fn spoken_percent_zero_and_whole_are_spoken_the_same_way() {
        assert_eq!([spoken_percent("0.0%"), spoken_percent("100.0%")], ["0.0 percent", "100.0 percent"]);
    }

    #[test]
    fn row_spoken_folder_gives_name_size_percent_and_files() {
        assert_eq!(row_spoken("target", "18.3 GB", "82.9%", 34_140), "target, 18.3 GB, 82.9 percent, 34,140 files");
    }

    #[test]
    fn row_spoken_one_file_says_1_file() {
        assert_eq!(row_spoken("Cargo.lock", "152.2 KB", "0.0%", 1), "Cargo.lock, 152.2 KB, 0.0 percent, 1 file");
    }

    #[test]
    fn row_spoken_empty_folder_says_0_files() {
        assert_eq!(row_spoken("none", "0 B", "0.0%", 0), "none, 0 B, 0.0 percent, 0 files");
    }

    /// The map value of the node at `path` (names from the root down) in `tip_tree`.
    fn value(path: &[&str]) -> String {
        let t = tip_tree();
        let node = path.iter().fold(t.root(), |n, name| find(&t, n, name));
        map_value(&t, node)
    }

    #[test]
    fn map_value_file_gives_its_name_and_size() {
        assert_eq!(value(&["Windows", "System32", "kernel32.dll"]), "kernel32.dll, 12.0 MB");
    }

    #[test]
    fn map_value_scan_root_gives_its_path_and_size() {
        assert_eq!(value(&[]), "C:\\, 12.0 MB");
    }

    #[test]
    fn map_value_name_with_a_right_to_left_override_shows_a_placeholder() {
        assert_eq!(value(&["invoice\u{202E}fdp.exe"]), "invoice\u{FFFD}fdp.exe, 10 B");
    }
}

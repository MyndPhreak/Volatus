//! Tree and treemap view logic: file counts, row flattening, sorting, expansion, selection
//! helpers and treemap hit-testing. Plain functions, no Slint types, so it is unit tested.

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
}

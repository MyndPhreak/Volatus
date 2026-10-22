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

/// The rect under the point, if any. Left and top edges are inside, right and bottom are not.
pub fn hit_test(rects: &[Rect], x: f64, y: f64) -> Option<NodeId> {
rects.iter().find(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|r| r.node)
}

/// The selected node's own rect `(x, y, w, h)` in the nested layout. `None` if it is not drawn.
pub fn selection_bounds(rects: &[Rect], selected: NodeId) -> Option<(f64, f64, f64, f64)> {
    let _ = (rects, selected);
    unimplemented!("selection_bounds for the nested layout")
}

/// The folder to zoom into when `leaf` is picked: the child of `view_root` on the path to
/// `leaf`, if that child is a folder. `None` if the leaf is a direct file of `view_root`.
pub fn drill_target(tree: &Tree, view_root: NodeId, leaf: NodeId) -> Option<NodeId> {
let mut cur = leaf;
loop {
    let parent = tree.node(cur).parent?;
    if parent == view_root {
        return tree.node(cur).is_dir.then_some(cur);
    }
    cur = parent;
}
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

    // ---- drill_target ----
    #[test]
    fn drill_target_leaf_in_subfolder_returns_child_of_view_root() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let sub = find(&t, a, "sub");
        let s1 = find(&t, sub, "s1");
        assert_eq!(drill_target(&t, t.root(), s1), Some(a));
        assert_eq!(drill_target(&t, a, s1), Some(sub));
    }

    #[test]
    fn drill_target_file_directly_under_view_root_is_none() {
        let t = sample();
        let c = find(&t, t.root(), "c");
        assert_eq!(drill_target(&t, t.root(), c), None);
    }

    #[test]
    fn drill_target_leaf_outside_view_root_is_none() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let b = find(&t, t.root(), "b");
        let b1 = find(&t, b, "b1");
        assert_eq!(drill_target(&t, a, b1), None);
    }
}

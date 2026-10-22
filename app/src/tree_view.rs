//! Tree and treemap view logic: file counts, row flattening, sorting, expansion, selection
//! helpers and treemap hit-testing. Plain functions, no Slint types, so it is unit tested.

use scan::{NodeId, Tree};
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
    pub fn new(_root: NodeId) -> Self {
        unimplemented!("ViewState::new")
    }

    /// Same key flips the direction. A new key starts ascending for Name, descending otherwise.
    pub fn sort_by(&mut self, _key: SortKey) {
        unimplemented!("ViewState::sort_by")
    }

    /// Expands a collapsed folder, collapses an expanded one. Files are ignored.
    pub fn toggle(&mut self, _tree: &Tree, _node: NodeId) {
        unimplemented!("ViewState::toggle")
    }

    /// Expands every ancestor of `node` so its row is visible.
    pub fn reveal(&mut self, _tree: &Tree, _node: NodeId) {
        unimplemented!("ViewState::reveal")
    }
}

/// Number of files under each node, indexed by `NodeId.0`. A file counts as 1 for itself.
pub fn file_counts(_tree: &Tree) -> Vec<u64> {
    unimplemented!("file_counts")
}

/// Visible rows, depth first, starting at the tree root. Only expanded folders are walked.
pub fn flatten(_tree: &Tree, _counts: &[u64], _view: &ViewState) -> Vec<Row> {
    unimplemented!("flatten")
}

/// Position of `node` in `rows`.
pub fn row_index(_rows: &[Row], _node: NodeId) -> Option<usize> {
    unimplemented!("row_index")
}

/// `part` as a share of `total`, one decimal: `33.3%`. A zero total gives `0.0%`.
pub fn format_percent(_part: u64, _total: u64) -> String {
    unimplemented!("format_percent")
}

/// The rect under the point, if any. Left and top edges are inside, right and bottom are not.
pub fn hit_test(_rects: &[Rect], _x: f64, _y: f64) -> Option<NodeId> {
    unimplemented!("hit_test")
}

/// Bounding box `(x, y, w, h)` of every rect at or under `selected`. `None` if none is drawn.
pub fn selection_bounds(_tree: &Tree, _rects: &[Rect], _selected: NodeId) -> Option<(f64, f64, f64, f64)> {
    unimplemented!("selection_bounds")
}

/// The folder to zoom into when `leaf` is picked: the child of `view_root` on the path to
/// `leaf`, if that child is a folder. `None` if the leaf is a direct file of `view_root`.
pub fn drill_target(_tree: &Tree, _view_root: NodeId, _leaf: NodeId) -> Option<NodeId> {
    unimplemented!("drill_target")
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
        assert_eq!(counts[t.root().0 as usize], 5);
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

    // ---- selection_bounds ----
    #[test]
    fn selection_bounds_file_is_its_own_rect() {
        let t = sample();
        let c = find(&t, t.root(), "c");
        let rects = [rect(c.0, 5.0, 6.0, 7.0, 8.0)];
        assert_eq!(selection_bounds(&t, &rects, c), Some((5.0, 6.0, 7.0, 8.0)));
    }

    #[test]
    fn selection_bounds_folder_is_union_of_descendant_rects() {
        let t = sample();
        let a = find(&t, t.root(), "a");
        let a1 = find(&t, a, "a1");
        let sub = find(&t, a, "sub");
        let s1 = find(&t, sub, "s1");
        let c = find(&t, t.root(), "c");
        let rects =
            [rect(a1.0, 0.0, 0.0, 10.0, 10.0), rect(s1.0, 10.0, 0.0, 10.0, 30.0), rect(c.0, 100.0, 100.0, 5.0, 5.0)];
        assert_eq!(selection_bounds(&t, &rects, a), Some((0.0, 0.0, 20.0, 30.0)));
        assert_eq!(selection_bounds(&t, &rects, sub), Some((10.0, 0.0, 10.0, 30.0)));
    }

    #[test]
    fn selection_bounds_node_not_drawn_is_none() {
        let t = sample();
        let b = find(&t, t.root(), "b");
        let c = find(&t, t.root(), "c");
        assert_eq!(selection_bounds(&t, &[rect(c.0, 0.0, 0.0, 1.0, 1.0)], b), None);
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

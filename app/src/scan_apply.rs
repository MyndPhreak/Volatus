//! Applying a finished delete to the open scan, on the UI thread.
//!
//! The `Data` of a scan is shared through `Arc` by `Loaded.data`, the `RowModel` and the treemap
//! layout and file-type workers. A delete changes it in place, so `remove_from` first takes the
//! `RowModel`'s weak link back (`RowModel::release`), then asks `Arc::get_mut` for the only reference.
//! It gets one only when no worker holds a clone, so no other thread can read the tree while it
//! changes. When a worker still holds one, nothing changes and the model gets its link back:
//! the caller tries again on a later turn of the event loop.

use super::{Data, Loaded};
use scan::{Detached, NodeId, Tree};
use std::sync::Arc;

/// A worker thread still holds the scan's `Data`: it cannot change now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Busy;

/// True when `n` is `root` or inside it. Works before and after `Tree::detach(root)`: the parent
/// links under `root` stay.
pub(crate) fn in_subtree(tree: &Tree, n: NodeId, root: NodeId) -> bool {
    let _ = (tree, n, root);
    false
}

/// Removes `node` from the shown scan in place: the tree, the file counts and every id the view
/// keeps. `Err(Busy)`, and nothing changed, while a worker holds the `Data`. `Ok(None)`, and
/// nothing changed, for the root, an id outside the tree, or a node already removed.
pub(crate) fn remove_from(l: &mut Loaded, node: NodeId) -> Result<Option<Detached>, Busy> {
    let _ = (l, node, Arc::<Data>::strong_count);
    Err(Busy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RowModel;
    use crate::tree_view::{ViewState, file_counts};
    use scan::rollup::{RawEntry, rollup};
    use slint::Model;
    use std::rc::Rc;
    use treemap::Rect;

    /// root(0) 160 { a(1) { a1.bin(3) 100, a2(4) { x.bin(6) 50 } }, b.bin(2) 10, c(5) {} }
    fn data() -> Arc<Data> {
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
            e(2, 0, "b.bin", 10, false),
            e(3, 1, "a1.bin", 100, false),
            e(4, 1, "a2", 0, true),
            e(5, 0, "c", 0, true),
            e(6, 4, "x.bin", 50, false),
        ];
        let tree = rollup(0, entries).unwrap().tree;
        Arc::new(Data { counts: file_counts(&tree), tree })
    }

    fn find(tree: &Tree, name: &str) -> NodeId {
        let mut stack = vec![tree.root()];
        while let Some(n) = stack.pop() {
            if tree.name(n) == name {
                return n;
            }
            stack.extend(tree.children(n));
        }
        panic!("no node {name}")
    }

    fn loaded(data: Arc<Data>) -> Loaded {
        let root = data.tree.root();
        let model = Rc::new(RowModel::new(data.clone()));
        Loaded { data, model, view: ViewState::new(root), selected: None, view_root: root, rects: Vec::new(), shown_root: None, totals: None, edits: 0 }
    }

    fn rect(node: NodeId) -> Rect {
        Rect { node, x: 0.0, y: 0.0, w: 1.0, h: 1.0 }
    }

    #[test]
    fn in_subtree_finds_the_node_and_what_is_under_it() {
        let d = data();
        let t = &d.tree;
        let (a, a2, x, b) = (find(t, "a"), find(t, "a2"), find(t, "x.bin"), find(t, "b.bin"));
        for (n, want) in [(a, true), (a2, true), (x, true), (b, false), (t.root(), false)] {
            assert_eq!(in_subtree(t, n, a), want, "{}", t.name(n));
        }
    }

    #[test]
    fn in_subtree_still_works_after_the_detach() {
        let mut d = Arc::try_unwrap(data()).ok().unwrap();
        let (a, x, b) = (find(&d.tree, "a"), find(&d.tree, "x.bin"), find(&d.tree, "b.bin"));
        d.tree.detach(a).unwrap();
        assert!(in_subtree(&d.tree, x, a));
        assert!(!in_subtree(&d.tree, b, a));
    }

    #[test]
    fn remove_from_a_unique_data_detaches_and_fixes_every_id() {
        let d = data();
        let t = &d.tree;
        let (root, a, a2, x, b) = (t.root(), find(t, "a"), find(t, "a2"), find(t, "x.bin"), find(t, "b.bin"));
        let want_selected = crate::tree_view::selection_after(t, a);
        assert!(want_selected.is_some_and(|n| n != a && n != root));
        let mut l = loaded(d.clone());
        drop(d);
        l.selected = Some(a);
        l.view_root = a2;
        l.view.expanded.extend([a, a2]);
        l.rects = vec![rect(a), rect(a2), rect(x), rect(b)];
        l.shown_root = Some(a2);
        l.totals = Some(Vec::new());

        let got = remove_from(&mut l, a);

        assert_eq!(got, Ok(Some(Detached { size: 150, allocated: 150, files: 2, dirs: 2 })));
        let tree = &l.data.tree;
        assert_eq!(tree.node(root).size, 10);
        assert_eq!(l.data.counts[root.0 as usize], 1);
        assert!(!tree.children(root).any(|c| c == a));
        // A sibling of `a` is selected; the view leaves the removed folder.
        assert_eq!(l.selected, want_selected);
        assert_eq!(l.view_root, root);
        assert_eq!(l.view.expanded.iter().copied().collect::<Vec<_>>(), vec![root]);
        assert_eq!(l.rects.iter().map(|r| r.node).collect::<Vec<_>>(), vec![b]);
        assert_eq!(l.shown_root, None);
        // The footer totals are of the old tree: they are counted again, and a count that started
        // before the removal is late (`Loaded::edits`).
        assert_eq!((l.totals.is_none(), l.edits), (true, 1));
        // The row model reads the changed data.
        let model_data = l.model.data.borrow().upgrade().expect("model has data again");
        assert!(Arc::ptr_eq(&model_data, &l.data));
    }

    #[test]
    fn remove_from_keeps_a_view_and_a_selection_outside_the_removed_node() {
        let d = data();
        let (root, a, c) = (d.tree.root(), find(&d.tree, "a"), find(&d.tree, "c"));
        let mut l = loaded(d);
        l.selected = Some(c);
        l.view_root = root;
        l.shown_root = Some(root);
        l.rects = vec![rect(root), rect(a), rect(c)];

        assert!(matches!(remove_from(&mut l, a), Ok(Some(_))));
        assert_eq!((l.selected, l.view_root, l.shown_root), (Some(c), root, Some(root)));
        assert_eq!(l.rects.iter().map(|r| r.node).collect::<Vec<_>>(), vec![root, c]);
    }

    #[test]
    fn remove_from_the_last_child_selects_the_previous_sibling() {
        let d = data();
        let (a, a1, a2) = (find(&d.tree, "a"), find(&d.tree, "a1.bin"), find(&d.tree, "a2"));
        let mut l = loaded(d);
        l.view_root = a;
        let last = l.data.tree.children(a).last().unwrap();
        let other = if last == a2 { a1 } else { a2 };
        l.selected = Some(last);

        assert!(matches!(remove_from(&mut l, last), Ok(Some(_))));
        assert_eq!((l.selected, l.view_root), (Some(other), a));
    }

    #[test]
    fn remove_from_shared_data_is_busy_and_changes_nothing() {
        let d = data();
        let (root, a) = (d.tree.root(), find(&d.tree, "a"));
        let mut l = loaded(d.clone());
        l.selected = Some(a);
        // `d` stands for a layout worker that still holds the data.
        let got = remove_from(&mut l, a);

        assert_eq!(got, Err(Busy));
        assert_eq!(l.data.tree.node(root).size, 160);
        assert!(l.data.tree.children(root).any(|c| c == a));
        assert_eq!(l.selected, Some(a));
        let model_data = l.model.data.borrow().upgrade().expect("model keeps its data");
        assert!(Arc::ptr_eq(&model_data, &d));

        // When the worker lets go, the same call removes it.
        drop(d);
        assert!(matches!(remove_from(&mut l, a), Ok(Some(_))));
        assert_eq!(l.data.tree.node(root).size, 10);
    }

    #[test]
    fn remove_from_the_root_or_a_removed_node_or_a_bad_id_is_none() {
        let d = data();
        let (root, a) = (d.tree.root(), find(&d.tree, "a"));
        let mut l = loaded(d);
        assert_eq!(remove_from(&mut l, root), Ok(None));
        assert_eq!(remove_from(&mut l, NodeId(999)), Ok(None));
        assert!(matches!(remove_from(&mut l, a), Ok(Some(_))));
        assert_eq!(remove_from(&mut l, a), Ok(None));
        assert_eq!(l.data.tree.node(root).size, 10);
        assert!(l.model.data.borrow().upgrade().is_some());
    }

    #[test]
    fn row_model_after_release_has_no_data_and_attach_gives_it_back() {
        let d = data();
        let model = RowModel::new(d.clone());
        model.set_rows(vec![crate::tree_view::Row { node: d.tree.root(), depth: 0, expanded: true, has_children: true }]);
        model.release();
        assert!(model.row_data(0).is_none());
        model.attach(&d);
        assert_eq!(model.row_data(0).map(|r| r.name.to_string()), Some("root".into()));
    }
}

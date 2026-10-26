//! Applying a finished delete to the open scan, on the UI thread.
//!
//! The `Data` of a scan is shared through `Arc` by `Loaded.data`, the `RowModel` and the treemap
//! layout and file-type workers. A delete changes it in place, so `remove_from` first takes the
//! `RowModel`'s weak link back (`RowModel::release`), then asks `Arc::get_mut` for the only reference.
//! It gets one only when no worker holds a clone, so no other thread can read the tree while it
//! changes. When a worker still holds one, nothing changes and the model gets its link back:
//! `apply_removed` tries again on a later turn of the event loop, a bounded number of times, then
//! leaves the tree as it is and offers a rescan. It never blocks the UI thread.
//!
//! The same module keeps the delete dialog's state and the job it runs (`DeleteSession`): the
//! dialog text and the state machine are in `delete_flow`, the Slint dialog comes in Task 7b.

use super::{
    Data, Loaded, MainWindow, SESSION, Session, count_types, crumb, drop_hover, refresh_drives, relayout, select,
    stop_move, sync_rows, sync_selection, with_loaded,
};
use crate::delete::{self, DeleteMode, RealFs, Remover};
use crate::delete_flow::{self, DeleteMsg, DialogEvent, DialogState, TreeAction};
use crate::purge::Purge;
use crate::recycle::Recycle;
use crate::tree_view;
use scan::{Detached, NodeId, Tree};
use slint::{ComponentHandle, ModelRc, Timer, VecModel};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Most later tries of `apply_removed` while a worker holds the scan's data, and the wait before
/// each: 3 s in all. Chosen, not measured: a layout of the shown folder or the file-type count of
/// the whole scan holds the data for that long only on a very large scan.
const APPLY_TRIES: u32 = 100;
const APPLY_RETRY: Duration = Duration::from_millis(30);

/// The delete dialog and the run it started. UI thread only.
pub(crate) struct DeleteSession {
    pub(crate) state: DialogState,
    job: Option<Job>,
}

impl Default for DeleteSession {
    fn default() -> Self {
        DeleteSession { state: DialogState::Closed, job: None }
    }
}

/// A running delete: the scan it was started on, the node it removes and its Stop flag.
struct Job {
    generation: u64,
    node: NodeId,
    stop: Arc<AtomicBool>,
}

/// The dialog for Delete (`shift` false) or Shift+Delete on the selection: `delete_flow::open_for`
/// with the real file system and the scan root AS TYPED (the policy resolves it itself).
pub(crate) fn open_delete(s: &Session, shift: bool) -> DialogState {
    let Some(l) = s.loaded.as_ref() else { return DialogState::Closed };
    delete_flow::open_for(
        &l.data.tree,
        &l.data.counts,
        l.selected,
        s.scan.state(),
        &s.scan_root,
        shift,
        |root, target, is_dir| delete::check_target(root, target, is_dir, &RealFs),
        delete_flow::is_link,
    )
}

/// Delete or Shift+Delete: opens the dialog for the selection when no dialog is open.
#[cfg_attr(not(test), expect(dead_code, reason = "called by the dialog and the keys in Task 7b"))]
#[cfg_attr(test, allow(dead_code, reason = "called by the dialog and the keys in Task 7b"))]
pub(crate) fn request_delete(shift: bool) {
    SESSION.with_borrow_mut(|s| {
        if matches!(s.delete.state, DialogState::Closed) {
            s.delete.state = open_delete(s, shift);
        }
    });
}

/// Feeds `event` to the dialog state machine.
fn step(s: &mut Session, event: DialogEvent) {
    let state = std::mem::replace(&mut s.delete.state, DialogState::Closed);
    s.delete.state = delete_flow::next(state, event);
}

/// The link that changes between the Recycle Bin and the permanent delete.
#[cfg_attr(not(test), expect(dead_code, reason = "called by the dialog in Task 7b"))]
#[cfg_attr(test, allow(dead_code, reason = "called by the dialog in Task 7b"))]
pub(crate) fn switch_delete_mode() {
    SESSION.with_borrow_mut(|s| step(s, DialogEvent::SwitchMode));
}

/// Cancel, Esc or Close. Does nothing while a delete runs.
#[cfg_attr(not(test), expect(dead_code, reason = "called by the dialog in Task 7b"))]
#[cfg_attr(test, allow(dead_code, reason = "called by the dialog in Task 7b"))]
pub(crate) fn close_delete() {
    SESSION.with_borrow_mut(|s| step(s, DialogEvent::Close));
}

/// Stop: the permanent delete ends after the item in progress. The Recycle Bin has no Stop.
#[cfg_attr(not(test), expect(dead_code, reason = "called by the dialog in Task 7b"))]
#[cfg_attr(test, allow(dead_code, reason = "called by the dialog in Task 7b"))]
pub(crate) fn stop_delete() {
    SESSION.with_borrow_mut(|s| {
        step(s, DialogEvent::Stop);
        if let (DialogState::Running { stopping: true, .. }, Some(job)) = (&s.delete.state, &s.delete.job) {
            job.stop.store(true, Ordering::Relaxed);
        }
    });
}

/// The engine for a mode. `owner` is the window handle the shell's own windows belong to.
fn remover_for(mode: DeleteMode, owner: Option<isize>) -> Arc<dyn Remover> {
    match mode {
        DeleteMode::RecycleBin => Arc::new(Recycle::new(owner)),
        DeleteMode::Permanent => Arc::new(Purge),
    }
}

/// The action button of the confirm dialog: starts the delete of the checked target on a worker
/// thread, tagged with the scan generation. Does nothing outside `Confirm`, while a scan runs, or
/// while another delete runs. `owner` is the window handle for the Recycle Bin's shell windows.
#[cfg_attr(not(test), expect(dead_code, reason = "called by the dialog in Task 7b"))]
#[cfg_attr(test, allow(dead_code, reason = "called by the dialog in Task 7b"))]
pub(crate) fn confirm_delete(ui: &MainWindow, owner: Option<isize>) {
    let start = SESSION.with_borrow_mut(|s| {
        let DialogState::Confirm { mode, target } = &s.delete.state else { return None };
        if !delete_flow::can_start(s.scan.state()) || s.delete.job.is_some() {
            return None;
        }
        let (mode, target) = (*mode, target.target.clone());
        let (generation, stop) = (s.scan.generation(), Arc::new(AtomicBool::new(false)));
        s.delete.job = Some(Job { generation, node: target.node, stop: stop.clone() });
        step(s, DialogEvent::Confirm);
        Some((mode, target, generation, stop))
    });
    let Some((mode, target, generation, stop)) = start else { return };
    let weak = ui.as_weak();
    let post = move |msg: DeleteMsg| {
        let weak = weak.clone();
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                on_delete_msg(&ui, msg);
            }
        });
    };
    if let Err(e) = delete_flow::spawn_delete(remover_for(mode, owner), target.clone(), generation, stop, post) {
        // Nothing ran: the result says so and the tree stays.
        eprintln!("Could not start the delete: {e}");
        let outcome = delete_flow::failed_outcome(&target);
        on_delete_msg(ui, DeleteMsg::Finished { generation, outcome, target_gone: false });
    }
}

/// A message of the running delete. The dialog always gets the progress and the result; the tree
/// changes only when the result is for the scan the window still shows (a rescan or a new scan
/// that started meanwhile already shows the disk as it is).
fn on_delete_msg(ui: &MainWindow, msg: DeleteMsg) {
    let detach = SESSION.with_borrow_mut(|s| {
        // No job, or a job of another generation: a late message.
        let generation = s.delete.job.as_ref().map(|j| j.generation).filter(|g| *g == msg.generation())?;
        match msg {
            DeleteMsg::Progress { items, bytes, .. } => {
                step(s, DialogEvent::Progress { items, bytes });
                None
            }
            DeleteMsg::Finished { outcome, target_gone, .. } => {
                let job = s.delete.job.take()?;
                step(s, DialogEvent::Finished { outcome, target_gone });
                if !(s.scan.is_current(generation) && s.loaded.is_some()) {
                    set_tree_action(s, TreeAction::Nothing);
                    return None;
                }
                matches!(s.delete.state, DialogState::Done { tree_action: TreeAction::Detach, .. }).then_some(job.node)
            }
        }
    });
    if let Some(node) = detach {
        apply_removed(ui, node);
    }
}

/// Changes the tree action of a finished dialog.
fn set_tree_action(s: &mut Session, action: TreeAction) {
    if let DialogState::Done { tree_action, .. } = &mut s.delete.state {
        *tree_action = action;
    }
}

/// The item is gone from the disk: removes `node` from the shown scan and redraws the tree, the
/// treemap and the footer (`remove_from`, then `refresh_after_remove`). While a worker holds the
/// data it tries again later (`APPLY_TRIES`); after the last try the tree stays and the dialog
/// offers a rescan. A newer scan that started meanwhile ends the tries.
pub(crate) fn apply_removed(ui: &MainWindow, node: NodeId) {
    let generation = SESSION.with_borrow(|s| s.scan.generation());
    try_remove(ui, node, generation, APPLY_TRIES);
}

fn try_remove(ui: &MainWindow, node: NodeId, generation: u64, tries_left: u32) {
    let result = SESSION.with_borrow_mut(|s| {
        if !s.scan.is_current(generation) {
            return Ok(None);
        }
        // A layout that a worker posts from here on is of the old tree: it is dropped. It also
        // lets go of its clone of the data sooner.
        s.layout_seq += 1;
        s.loaded.as_mut().map_or(Ok(None), |l| remove_from(l, node))
    });
    match result {
        Ok(Some(_)) => refresh_after_remove(ui),
        Ok(None) => {}
        Err(Busy) if tries_left > 0 => {
            let weak = ui.as_weak();
            Timer::single_shot(APPLY_RETRY, move || {
                if let Some(ui) = weak.upgrade() {
                    try_remove(&ui, node, generation, tries_left - 1);
                }
            });
        }
        Err(Busy) => {
            eprintln!("The scan data stayed in use. The tree is not changed; scan again to see the changes.");
            SESSION.with_borrow_mut(|s| set_tree_action(s, TreeAction::KeepAndOfferRescan));
            // The tries dropped the layouts that were on their way: draw the map again.
            relayout(ui);
        }
    }
}

/// Redraws everything after `remove_from`: no picture or hover of the old tree stays, a new
/// layout starts (it bumps `layout_seq`), the crumbs and rows are rebuilt, the selection shows,
/// the footer totals are counted again from the whole scan and the drive list (free space) is
/// read again.
fn refresh_after_remove(ui: &MainWindow) {
    SESSION.with_borrow_mut(|s| {
        s.held = None;
        s.out_from = None;
    });
    stop_move(ui);
    drop_hover(ui);
    relayout(ui);
    let (selected, crumbs) =
        with_loaded(|l| (l.selected, tree_view::crumbs(&l.data.tree, l.view_root))).unwrap_or_default();
    ui.set_crumbs(ModelRc::new(VecModel::from(crumbs.into_iter().map(crumb).collect::<Vec<_>>())));
    match selected {
        Some(n) => select(ui, n),
        None => {
            sync_rows(ui);
            sync_selection(ui);
        }
    }
    if let Some(data) = with_loaded(|l| l.data.clone()) {
        count_types(ui, data);
    }
    refresh_drives(ui);
}

/// A worker thread still holds the scan's `Data`: it cannot change now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Busy;

/// True when `n` is `root` or inside it. Works before and after `Tree::detach(root)`: the parent
/// links under `root` stay.
pub(crate) fn in_subtree(tree: &Tree, n: NodeId, root: NodeId) -> bool {
    std::iter::successors(Some(n), |&c| tree.node(c).parent).any(|c| c == root)
}

/// Removes `node` from the shown scan in place: the tree, the file counts and every id the view
/// keeps. `Err(Busy)`, and nothing changed, while a worker holds the `Data`. `Ok(None)`, and
/// nothing changed, for the root, an id outside the tree, or a node already removed.
pub(crate) fn remove_from(l: &mut Loaded, node: NodeId) -> Result<Option<Detached>, Busy> {
    let Loaded { data, model, view, selected, view_root, rects, shown_root, totals, edits } = l;
    // The model's weak link goes first, so `get_mut` can see the data is not shared.
    model.release();
    let result = match Arc::get_mut(data) {
        None => Err(Busy),
        Some(d) => Ok(remove_node(d, node, view, selected, view_root, rects, shown_root).inspect(|_| {
            // The file-type totals are of the old tree: they are counted again, and a count that
            // started before this removal is late (`store_totals`).
            *totals = None;
            *edits += 1;
        })),
    };
    model.attach(data);
    result
}

/// `remove_from` on data the caller owns alone. Every id is read BEFORE `Tree::detach`, which
/// clears the links `selection_after` and `view_for_deleted` walk.
fn remove_node(
    d: &mut Data,
    node: NodeId,
    view: &mut tree_view::ViewState,
    selected: &mut Option<NodeId>,
    view_root: &mut NodeId,
    rects: &mut Vec<treemap::Rect>,
    shown_root: &mut Option<NodeId>,
) -> Option<Detached> {
    // The root, an id outside the tree, and a node already removed (no parent) stay as they are.
    if node.0 as usize >= d.counts.len() {
        return None;
    }
    let parent = d.tree.node(node).parent?;
    let tree = &d.tree;
    let inside = |n: NodeId| in_subtree(tree, n, node);
    let next_selected = if selected.is_some_and(inside) { tree_view::selection_after(tree, node) } else { *selected };
    let next_view = tree_view::view_for_deleted(tree, *view_root, node);
    view.expanded.retain(|&n| !inside(n));
    rects.retain(|r| !inside(r.node));
    *shown_root = shown_root.filter(|&n| !inside(n));
    let detached = d.tree.detach(node)?;
    tree_view::counts_after_detach(&mut d.counts, &d.tree, Some(parent), &detached);
    *selected = next_selected;
    *view_root = next_view;
    Some(detached)
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
        assert!(l.model.data.borrow().upgrade().is_some_and(|m| Arc::ptr_eq(&m, &d)), "model keeps its data");

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

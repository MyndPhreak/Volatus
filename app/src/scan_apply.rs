//! Applying a finished delete to the open scan, on the UI thread.
//!
//! The `Data` of a scan is shared through `Arc` by `Loaded.data`, the `RowModel` and the treemap
//! layout and file-type workers. A delete changes it in place, so `remove_from` first takes the
//! `RowModel`'s weak link back (`RowModel::release`), then asks `Arc::get_mut` for the only reference.
//! It gets one only when no worker holds a clone, so no other thread can read the tree while it
//! changes. When a worker still holds one, nothing changes and the model gets its link back. The
//! removal then waits as a `Pending` in the session: each worker calls `data_released` when it
//! lets go of its clone, and a timer tries again too. After `APPLY_TRIES` timer tries the tree
//! stays, the banner and the dialog offer a rescan. Nothing blocks the UI thread.
//!
//! Every delete is tied to the scan it was opened on (`Origin`: the scan generation and the
//! identity of its data). A dialog, a job or a pending removal of another scan never touches the
//! shown tree; a new scan closes the dialog (`scan_changed`).
//!
//! The dialog text and the state machine are in `delete_flow`; the Slint dialog, the menu and the
//! keys are in `delete_ui`, which shows the dialog again (`delete_ui::sync`) after each call here.

use super::{
    Data, Loaded, MainWindow, SESSION, Session, count_types, crumb, drop_hover, refresh_drives, relayout, select,
    show_state, stop_move, sync_rows, sync_selection, with_loaded,
};
use crate::delete::{self, DeleteMode, DeleteTarget, RealFs, Refusal, Remover};
use crate::delete_flow::{self, DeleteMsg, DialogEvent, DialogState, TreeAction};
use crate::model::ScanState;
use crate::purge::Purge;
use crate::recycle::Recycle;
use crate::tree_view;
use scan::{Detached, NodeId, Tree};
use slint::{ComponentHandle, ModelRc, Timer, VecModel};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Most timer tries of a pending removal while a worker holds the scan's data, and the wait
/// before each: 10 s in all. Chosen, not measured. A worker that lets go of its clone applies the
/// removal at once (`data_released`), so the timer only covers a missed release.
const APPLY_TRIES: u32 = 100;
const APPLY_RETRY: Duration = Duration::from_millis(100);

/// The banner text when a removal could not be applied (ASD-STE100).
pub(crate) const STALE_LIST_NOTICE: &str = "Volatus could not update the list after the delete. Scan again to see the changes.";

/// The scan a dialog, a job or a pending removal belongs to: the scan generation and the serial
/// number of the shown data (`Loaded::serial`). `ScanControl::reset` keeps the generation, so the
/// generation alone does not tell two scans apart; a data pointer is not used, as a freed
/// address can come back for the next scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Origin {
    generation: u64,
    data: u64,
}

/// The scan the window shows now, or `None` when none is loaded.
fn origin_of(s: &Session) -> Option<Origin> {
    s.loaded.as_ref().map(|l| Origin { generation: s.scan.generation(), data: l.serial })
}

/// The delete dialog, the run it started, and a removal that waits for the data. UI thread only.
pub(crate) struct DeleteSession {
    pub(crate) state: DialogState,
    /// The scan the dialog was opened on.
    opened: Option<Origin>,
    job: Option<Job>,
    pending: Option<Pending>,
    /// Shown on the banner when a removal could not be applied. Cleared by a new scan.
    pub(crate) notice: Option<String>,
}

impl Default for DeleteSession {
    fn default() -> Self {
        DeleteSession { state: DialogState::Closed, opened: None, job: None, pending: None, notice: None }
    }
}

/// A running delete: the scan it was started on, the node it removes and its Stop flag.
struct Job {
    origin: Origin,
    node: NodeId,
    stop: Arc<AtomicBool>,
}

/// A removal from the tree that waits until no worker holds the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    origin: Origin,
    node: NodeId,
    tries_left: u32,
    /// It comes from a `Missing` refusal at Open, not from a finished delete.
    from_missing: bool,
}

/// A confirm is allowed only on the scan the dialog was opened on, while no scan runs.
pub(crate) fn may_confirm(opened: Option<Origin>, now: Option<Origin>, scan: &ScanState) -> bool {
    opened.is_some() && opened == now && delete_flow::can_start(scan)
}

/// The tree action a finished delete keeps: the run's own only when the message is of this job
/// (`msg_generation`) and the window still shows the job's scan; else `Nothing`, as a rescan or
/// a new scan already shows the disk as it is.
pub(crate) fn finished_action(job: Origin, msg_generation: u64, now: Option<Origin>, action: TreeAction) -> TreeAction {
    if msg_generation == job.generation && now == Some(job) { action } else { TreeAction::Nothing }
}

/// A new scan starts or the shown scan is dropped: the dialog closes unless a delete runs (its
/// result is then dropped by its origin), and a pending removal and the notice go.
pub(crate) fn scan_changed(s: &mut Session) {
    if !matches!(s.delete.state, DialogState::Running { .. }) {
        s.delete.state = DialogState::Closed;
        s.delete.opened = None;
    }
    s.delete.pending = None;
    s.delete.notice = None;
}

/// Opens the dialog on the selection when none is open. `check` is the policy, `is_link` the link
/// probe, `gone` the strict "not found" check. A `Missing` refusal whose item `gone` confirms
/// is removed from the tree too: `true` when such a removal now waits (`apply_pending` does it).
pub(crate) fn open_in(
    s: &mut Session,
    shift: bool,
    check: impl FnOnce(&Path, &Path, bool) -> Result<PathBuf, Refusal>,
    is_link: impl FnOnce(&Path) -> bool,
    gone: impl FnOnce(&Path) -> bool,
) -> bool {
    if !matches!(s.delete.state, DialogState::Closed) {
        return false;
    }
    // A removal still waits to leave the list: no second delete until it has (never two pending).
    if s.delete.pending.is_some() {
        s.delete.state = DialogState::Refused { text: delete_flow::BUSY_TEXT.to_string() };
        return false;
    }
    let Some(origin) = origin_of(s) else { return false };
    let Some(l) = s.loaded.as_ref() else { return false };
    let (tree, counts) = (&l.data.tree, &l.data.counts);
    let checked = delete_flow::open_check(tree, counts, l.selected, s.scan.state(), &s.scan_root, check, is_link);
    let Some((node, target)) = checked else { return false };
    let path = tree.path(node);
    s.delete.opened = Some(origin);
    match target {
        Err(r) => {
            let (text, detach) = delete_flow::refusal_for(r, || gone(&path));
            s.delete.state = DialogState::Refused { text };
            if detach {
                s.delete.pending = Some(Pending { origin, node, tries_left: APPLY_TRIES, from_missing: true });
            }
            detach
        }
        Ok(info) => {
            s.delete.state = delete_flow::next(DialogState::Closed, DialogEvent::Open { shift, target: Ok(info) });
            false
        }
    }
}

/// What `begin_confirm` starts.
pub(crate) struct Start {
    mode: DeleteMode,
    target: DeleteTarget,
    generation: u64,
    stop: Arc<AtomicBool>,
}

/// The action button: checks again, then makes the job. `None`, and no job, outside `Confirm`,
/// while another delete runs, while a scan runs, or when the window no longer shows the scan the
/// dialog was opened on. The second `check` (just before the delete) must give the same resolved
/// path as at Open; else the dialog shows why (`delete_flow::recheck`) and nothing starts.
pub(crate) fn begin_confirm(s: &mut Session, check: impl FnOnce(&Path, &Path, bool) -> Result<PathBuf, Refusal>) -> Option<Start> {
    let DialogState::Confirm { mode, target } = &s.delete.state else { return None };
    let now = origin_of(s);
    if s.delete.job.is_some() || !may_confirm(s.delete.opened, now, s.scan.state()) {
        return None;
    }
    if s.delete.pending.is_some() {
        step(s, DialogEvent::Refuse { text: delete_flow::BUSY_TEXT.to_string() });
        return None;
    }
    let (mode, target, origin) = (*mode, target.target.clone(), now?);
    let path = s.loaded.as_ref()?.data.tree.path(target.node);
    if let Err(text) = delete_flow::recheck(&target.path, check(&s.scan_root, &path, target.is_dir)) {
        step(s, DialogEvent::Refuse { text });
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    s.delete.job = Some(Job { origin, node: target.node, stop: stop.clone() });
    step(s, DialogEvent::Confirm);
    Some(Start { mode, target, generation: origin.generation, stop })
}

/// A message of the running delete. The dialog always gets the progress and the result. The tree
/// changes only for a result of the scan the window still shows (`finished_action`): then the
/// removal waits as a `Pending`, and `true` says `apply_pending` must run.
pub(crate) fn handle_msg(s: &mut Session, msg: DeleteMsg) -> bool {
    // No job, or a job of another generation: a late message.
    let Some(job_origin) = s.delete.job.as_ref().map(|j| j.origin).filter(|o| o.generation == msg.generation()) else {
        return false;
    };
    match msg {
        DeleteMsg::Progress { items, bytes, .. } => {
            step(s, DialogEvent::Progress { items, bytes });
            false
        }
        DeleteMsg::Finished { generation, outcome, target_gone } => {
            let Some(job) = s.delete.job.take() else { return false };
            step(s, DialogEvent::Finished { outcome, target_gone });
            let now = origin_of(s);
            let DialogState::Done { tree_action, .. } = &mut s.delete.state else { return false };
            *tree_action = finished_action(job_origin, generation, now, *tree_action);
            let detach = *tree_action == TreeAction::Detach;
            if !detach {
                return false;
            }
            // Open and Confirm refuse while a removal waits, so a second one cannot come here.
            debug_assert!(s.delete.pending.is_none(), "a second removal while one waits");
            if s.delete.pending.is_some() {
                // Keep the first; this one is not applied, and the user is told.
                s.delete.notice = Some(STALE_LIST_NOTICE.to_string());
                *tree_action_of(s) = TreeAction::KeepAndOfferRescan;
                return false;
            }
            s.delete.pending = Some(Pending { origin: job.origin, node: job.node, tries_left: APPLY_TRIES, from_missing: false });
            true
        }
    }
}

/// The tree action of a finished dialog. Only called in `Done`.
fn tree_action_of(s: &mut Session) -> &mut TreeAction {
    match &mut s.delete.state {
        DialogState::Done { tree_action, .. } => tree_action,
        _ => unreachable!("handle_msg moved the dialog to Done"),
    }
}

/// What one try of a pending removal did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingStep {
    /// No removal waits.
    Idle,
    /// The tree changed: redraw (`refresh_after_remove`).
    Applied,
    /// A worker still holds the data: try again later.
    Busy,
    /// It was for another scan, or the node cannot be removed: it is gone, nothing changed.
    Dropped,
    /// The last timer try failed: the tree stays, the banner and the dialog offer a rescan.
    GaveUp,
}

/// One try of the pending removal. `timer` tries count against `APPLY_TRIES`; a try after a
/// worker let go of its clone does not. `layout_seq` goes up only when the tree changes, so a
/// busy try never drops a layout.
pub(crate) fn step_pending(s: &mut Session, timer: bool) -> PendingStep {
    let Some(p) = s.delete.pending else { return PendingStep::Idle };
    if origin_of(s) != Some(p.origin) {
        s.delete.pending = None;
        return PendingStep::Dropped;
    }
    let Some(l) = s.loaded.as_mut() else { return PendingStep::Idle };
    match remove_from(l, p.node) {
        Ok(Some(_)) => {
            s.delete.pending = None;
            // A layout a worker posts from here on is of the old tree: it is dropped.
            s.layout_seq += 1;
            mark_list(s, p, true);
            PendingStep::Applied
        }
        Ok(None) => {
            s.delete.pending = None;
            mark_list(s, p, false);
            PendingStep::Dropped
        }
        Err(Busy) if !timer => PendingStep::Busy,
        Err(Busy) if p.tries_left > 1 => {
            s.delete.pending = Some(Pending { tries_left: p.tries_left - 1, ..p });
            PendingStep::Busy
        }
        Err(Busy) => {
            s.delete.pending = None;
            s.delete.notice = Some(STALE_LIST_NOTICE.to_string());
            mark_list(s, p, false);
            PendingStep::GaveUp
        }
    }
}

/// The dialog says what happened to the list: `Detached` once the item left it, the rescan
/// offer when it could not leave it.
fn mark_list(s: &mut Session, p: Pending, removed: bool) {
    match &mut s.delete.state {
        DialogState::Done { tree_action, .. } if !p.from_missing && *tree_action == TreeAction::Detach => {
            *tree_action = if removed { TreeAction::Detached } else { TreeAction::KeepAndOfferRescan };
        }
        DialogState::Refused { text } if p.from_missing && text.as_str() == delete_flow::GONE_PENDING => {
            *text = (if removed { delete_flow::GONE_REMOVED } else { delete_flow::GONE_KEPT }).to_string();
        }
        _ => {}
    }
}

/// The dialog for Delete (`shift` false) or Shift+Delete on the selection, with the real file
/// system and the scan root AS TYPED (the policy resolves it itself). An item gone from the disk
/// leaves the tree.
pub(crate) fn request_delete(ui: &MainWindow, shift: bool) {
    let check = |root: &Path, target: &Path, is_dir: bool| delete::check_target(root, target, is_dir, &RealFs);
    if SESSION.with_borrow_mut(|s| open_in(s, shift, check, delete_flow::is_link, delete_flow::target_gone)) {
        apply_pending(ui);
    }
}

/// Feeds `event` to the dialog state machine.
fn step(s: &mut Session, event: DialogEvent) {
    let state = std::mem::replace(&mut s.delete.state, DialogState::Closed);
    s.delete.state = delete_flow::next(state, event);
}

/// The link that changes between the Recycle Bin and the permanent delete.
pub(crate) fn switch_delete_mode() {
    SESSION.with_borrow_mut(|s| step(s, DialogEvent::SwitchMode));
}

/// Cancel, Esc or Close. Does nothing while a delete runs.
pub(crate) fn close_delete() {
    SESSION.with_borrow_mut(|s| {
        step(s, DialogEvent::Close);
        if matches!(s.delete.state, DialogState::Closed) {
            s.delete.opened = None;
        }
    });
}

/// Stop: the permanent delete ends after the item in progress. The Recycle Bin has no Stop.
pub(crate) fn stop_delete() {
    SESSION.with_borrow_mut(|s| {
        step(s, DialogEvent::Stop);
        if let (DialogState::Running { stopping: true, .. }, Some(job)) = (&s.delete.state, &s.delete.job) {
            job.stop.store(true, Ordering::Relaxed);
        }
    });
}

/// The engine for a mode. `owner` is the window handle the shell's own windows belong to.
pub(crate) fn remover_for(mode: DeleteMode, owner: Option<isize>) -> Arc<dyn Remover> {
    match mode {
        DeleteMode::RecycleBin => Arc::new(Recycle::new(owner)),
        DeleteMode::Permanent => Arc::new(Purge),
    }
}

/// Where a delete runs: `spawn_delete` (a worker thread) in the app. `Err` when nothing ran.
pub(crate) type Spawn = dyn Fn(Arc<dyn Remover>, DeleteTarget, u64, Arc<AtomicBool>, Box<dyn Fn(DeleteMsg) + Send>) -> std::io::Result<()>;

/// The engine for a mode and the window handle: `remover_for` in the app.
pub(crate) type RemoverFor = dyn Fn(DeleteMode, Option<isize>) -> Arc<dyn Remover>;

/// `spawn_delete` as a `Spawn`.
pub(crate) fn spawn_worker(
    remover: Arc<dyn Remover>,
    target: DeleteTarget,
    generation: u64,
    stop: Arc<AtomicBool>,
    post: Box<dyn Fn(DeleteMsg) + Send>,
) -> std::io::Result<()> {
    delete_flow::spawn_delete(remover, target, generation, stop, post).map(drop)
}

/// The action button of the confirm dialog: `begin_confirm` with the real policy, then the delete
/// through `spawn` (a worker thread in the app), tagged with the scan generation. `owner` is the
/// window handle for the Recycle Bin's shell windows; `remover_for` picks the engine (the tests
/// give a fake one).
pub(crate) fn confirm_delete(ui: &MainWindow, owner: Option<isize>, remover_for: &RemoverFor, spawn: &Spawn) {
    let check = |root: &Path, target: &Path, is_dir: bool| delete::check_target(root, target, is_dir, &RealFs);
    let Some(Start { mode, target, generation, stop }) = SESSION.with_borrow_mut(|s| begin_confirm(s, check)) else {
        return;
    };
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
    if let Err(e) = spawn(remover_for(mode, owner), target.clone(), generation, stop, Box::new(post)) {
        // Nothing ran: the result says so and the tree stays.
        eprintln!("Could not start the delete: {e}");
        let outcome = delete_flow::failed_outcome(&target);
        on_delete_msg(ui, DeleteMsg::Finished { generation, outcome, target_gone: false });
    }
}

/// `handle_msg` on the session, then the removal when it says so, then the dialog shows it.
pub(crate) fn on_delete_msg(ui: &MainWindow, msg: DeleteMsg) {
    if SESSION.with_borrow_mut(|s| handle_msg(s, msg)) {
        apply_pending(ui);
    }
    crate::delete_ui::sync(ui);
}

/// Tries the pending removal now; while a worker holds the data, a timer tries again.
fn apply_pending(ui: &MainWindow) {
    match SESSION.with_borrow_mut(|s| step_pending(s, false)) {
        PendingStep::Applied => refresh_after_remove(ui),
        PendingStep::Busy => retry_later(ui),
        PendingStep::Idle | PendingStep::Dropped | PendingStep::GaveUp => {}
    }
}

/// One timer try of the pending removal, after `APPLY_RETRY`, then the next while it is busy.
fn retry_later(ui: &MainWindow) {
    let weak = ui.as_weak();
    Timer::single_shot(APPLY_RETRY, move || {
        let Some(ui) = weak.upgrade() else { return };
        match SESSION.with_borrow_mut(|s| step_pending(s, true)) {
            PendingStep::Applied => refresh_after_remove(&ui),
            PendingStep::Busy => retry_later(&ui),
            PendingStep::GaveUp => {
                eprintln!("{STALE_LIST_NOTICE}");
                show_state(&ui);
            }
            PendingStep::Idle | PendingStep::Dropped => {}
        }
        // The dialog text says whether the item left the list.
        crate::delete_ui::sync(&ui);
    });
}

/// A worker let go of its clone of the scan's data: a pending removal may apply now.
pub(crate) fn data_released(ui: &MainWindow) {
    match SESSION.with_borrow_mut(|s| step_pending(s, false)) {
        PendingStep::Applied => {
            refresh_after_remove(ui);
            crate::delete_ui::sync(ui);
        }
        PendingStep::Dropped => crate::delete_ui::sync(ui),
        PendingStep::Idle | PendingStep::Busy | PendingStep::GaveUp => {}
    }
}

/// Redraws everything after `remove_from`: no picture or hover of the old tree stays, a new
/// layout starts, the crumbs and rows are rebuilt, the selection shows, the footer totals are
/// counted again from the whole scan and the drive list (free space) is read again.
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
    let Loaded { data, model, view, selected, view_root, rects, shown_root, totals, edits, .. } = l;
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
        Loaded { data, serial: crate::next_serial(), model, view: ViewState::new(root), selected: None, view_root: root, rects: Vec::new(), shown_root: None, totals: None, edits: 0 }
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

    // ---- the guards of a delete against a changed or newer scan ----

    use crate::delete::Outcome;
    use crate::model::{Event, Summary};

    fn summary() -> Summary {
        Summary {
            method: scan::Method::Walk,
            files: 3,
            dirs: 3,
            skipped: 0,
            elapsed: Duration::ZERO,
            phases: Default::default(),
            total: 160,
        }
    }

    /// A finished scan of `data` on screen, `node` selected, no dialog.
    fn show(data: Arc<Data>, node: &str) {
        SESSION.with_borrow_mut(|s| {
            if matches!(s.scan.state(), ScanState::Done(_)) {
                assert!(s.scan.reset());
            }
            let (g, _) = s.scan.start().expect("no scan runs");
            assert!(s.scan.apply(g, Event::Finished(summary())));
            let selected = find(&data.tree, node);
            let mut l = loaded(data);
            l.selected = Some(selected);
            s.loaded = Some(l);
            s.scan_root = PathBuf::from(r"C:\data");
            s.delete = DeleteSession::default();
        });
    }

    /// The policy, faked: every item is fine and resolves to `\\?\` plus its path.
    fn ok(_: &Path, t: &Path, _: bool) -> Result<PathBuf, Refusal> {
        Ok(PathBuf::from(format!(r"\\?\{}", t.display())))
    }

    fn open(shift: bool) -> bool {
        SESSION.with_borrow_mut(|s| open_in(s, shift, ok, |_| false, |_| false))
    }

    fn confirm() -> Option<u64> {
        SESSION.with_borrow_mut(|s| begin_confirm(s, ok).map(|st| st.generation))
    }

    fn state() -> DialogState {
        SESSION.with_borrow(|s| s.delete.state.clone())
    }

    fn now() -> Origin {
        SESSION.with_borrow(|s| origin_of(s).expect("a scan is shown"))
    }

    fn has_job() -> bool {
        SESSION.with_borrow(|s| s.delete.job.is_some())
    }

    #[test]
    fn open_in_stamps_the_scan_it_was_opened_on() {
        show(data(), "a");
        assert!(!open(false));
        assert!(matches!(state(), DialogState::Confirm { mode: DeleteMode::RecycleBin, .. }), "{:?}", state());
        assert_eq!(SESSION.with_borrow(|s| s.delete.opened), Some(now()));
    }

    #[test]
    fn begin_confirm_on_the_same_scan_starts_a_job() {
        show(data(), "a");
        open(true);
        let generation = now().generation;
        assert_eq!(confirm(), Some(generation));
        assert!(has_job());
        assert!(matches!(state(), DialogState::Running { mode: DeleteMode::Permanent, .. }), "{:?}", state());
    }

    #[test]
    fn begin_confirm_after_a_new_scan_finished_does_nothing() {
        show(data(), "a");
        open(false);
        // A rescan finished: a new generation and new data. The dialog was not closed.
        SESSION.with_borrow_mut(|s| {
            assert!(s.scan.reset());
            let (g, _) = s.scan.start().unwrap();
            assert!(s.scan.apply(g, Event::Finished(summary())));
            s.loaded = Some(loaded(data()));
        });
        assert_eq!(confirm(), None);
        assert!(!has_job());
    }

    #[test]
    fn begin_confirm_after_a_reset_that_keeps_the_generation_does_nothing() {
        show(data(), "a");
        open(false);
        let opened = now();
        SESSION.with_borrow_mut(|s| {
            assert!(s.scan.reset());
            s.loaded = None;
        });
        assert_eq!(confirm(), None);
        // The same generation with other data on screen.
        SESSION.with_borrow_mut(|s| s.loaded = Some(loaded(data())));
        assert_eq!(now().generation, opened.generation);
        assert_eq!(confirm(), None);
        assert!(!has_job());
    }

    #[test]
    fn begin_confirm_with_other_data_of_the_same_generation_does_nothing() {
        show(data(), "a");
        open(false);
        SESSION.with_borrow_mut(|s| s.loaded = Some(loaded(data())));
        assert_eq!(confirm(), None);
        assert!(!has_job());
    }

    #[test]
    fn may_confirm_table() {
        let a = Origin { generation: 1, data: 10 };
        let b = Origin { generation: 1, data: 11 };
        let c = Origin { generation: 2, data: 10 };
        let idle = ScanState::Idle;
        let scanning = ScanState::Scanning { files: 0, dirs: 0 };
        for (opened, now, scan, want) in [
            (Some(a), Some(a), &idle, true),
            (Some(a), Some(a), &scanning, false),
            (Some(a), Some(b), &idle, false),
            (Some(a), Some(c), &idle, false),
            (Some(a), None, &idle, false),
            (None, Some(a), &idle, false),
            (None, None, &idle, false),
        ] {
            assert_eq!(may_confirm(opened, now, scan), want, "{opened:?} {now:?} {scan:?}");
        }
    }

    #[test]
    fn begin_confirm_checks_again_and_refuses_another_path() {
        show(data(), "a");
        open(false);
        let other = SESSION.with_borrow_mut(|s| begin_confirm(s, |_, _, _| Ok(PathBuf::from(r"\\?\C:\elsewhere\a"))).is_some());
        assert!(!other);
        assert_eq!(state(), DialogState::Refused { text: delete_flow::CHANGED_TEXT.into() });
        assert!(!has_job());
    }

    #[test]
    fn begin_confirm_checks_again_and_shows_the_refusal() {
        show(data(), "a");
        open(false);
        let started = SESSION.with_borrow_mut(|s| begin_confirm(s, |_, _, _| Err(Refusal::KindChanged)).is_some());
        assert!(!started);
        assert_eq!(state(), DialogState::Refused { text: delete::refusal_text(Refusal::KindChanged).into() });
        assert!(!has_job());
    }

    #[test]
    fn begin_confirm_checks_again_with_the_root_as_typed_and_the_node_path() {
        show(data(), "a");
        open(false);
        let seen = std::cell::RefCell::new(None);
        SESSION.with_borrow_mut(|s| {
            begin_confirm(s, |root, t, is_dir| {
                *seen.borrow_mut() = Some((root.to_path_buf(), t.to_path_buf(), is_dir));
                ok(root, t, is_dir)
            })
        });
        assert_eq!(seen.into_inner(), Some((PathBuf::from(r"C:\data"), PathBuf::from(r"root\a"), true)));
        assert!(has_job());
    }

    #[test]
    fn finished_action_table() {
        let job = Origin { generation: 4, data: 10 };
        for action in [TreeAction::Detach, TreeAction::KeepAndOfferRescan, TreeAction::Nothing] {
            for (msg, now, want) in [
                (4, Some(job), action),
                (3, Some(job), TreeAction::Nothing),
                (4, Some(Origin { generation: 5, data: 10 }), TreeAction::Nothing),
                (4, Some(Origin { generation: 4, data: 11 }), TreeAction::Nothing),
                (4, None, TreeAction::Nothing),
            ] {
                assert_eq!(finished_action(job, msg, now, action), want, "{action:?} msg {msg} now {now:?}");
            }
        }
    }

    /// A running permanent delete of node `a` of a shown scan. Returns its generation.
    fn running() -> u64 {
        show(data(), "a");
        open(true);
        confirm().expect("job")
    }

    fn finished(generation: u64, gone: bool, outcome: Outcome) -> bool {
        SESSION.with_borrow_mut(|s| handle_msg(s, DeleteMsg::Finished { generation, outcome, target_gone: gone }))
    }

    fn done_action() -> Option<TreeAction> {
        match state() {
            DialogState::Done { tree_action, .. } => Some(tree_action),
            _ => None,
        }
    }

    fn removed() -> Outcome {
        Outcome { removed_items: 3, removed_bytes: 150, ..Outcome::default() }
    }

    #[test]
    fn handle_msg_current_result_with_the_item_gone_waits_to_remove_it() {
        let g = running();
        assert!(SESSION.with_borrow_mut(|s| !handle_msg(s, DeleteMsg::Progress { generation: g, items: 1, bytes: 2 })));
        assert!(matches!(state(), DialogState::Running { progress: delete_flow::RunProgress { items: 1, bytes: 2 }, .. }));
        assert!(finished(g, true, removed()));
        assert_eq!(done_action(), Some(TreeAction::Detach));
        assert!(SESSION.with_borrow(|s| s.delete.pending.is_some() && s.delete.job.is_none()));
    }

    #[test]
    fn handle_msg_current_result_with_the_item_still_there_keeps_the_tree() {
        let g = running();
        assert!(!finished(g, false, removed()));
        assert_eq!(done_action(), Some(TreeAction::KeepAndOfferRescan));
        assert!(SESSION.with_borrow(|s| s.delete.pending.is_none()));
    }

    #[test]
    fn handle_msg_result_for_a_scan_no_longer_shown_does_not_touch_the_tree() {
        // A new scan with new data; the running dialog stays (`scan_changed` keeps a running one).
        let g = running();
        SESSION.with_borrow_mut(|s| {
            assert!(s.scan.reset());
            let (n, _) = s.scan.start().unwrap();
            assert!(s.scan.apply(n, Event::Finished(summary())));
            s.loaded = Some(loaded(data()));
            scan_changed(s);
        });
        assert!(!finished(g, true, removed()));
        assert_eq!(done_action(), Some(TreeAction::Nothing));
        assert!(SESSION.with_borrow(|s| s.delete.pending.is_none()));
    }

    #[test]
    fn handle_msg_result_for_other_data_of_the_same_generation_does_not_touch_the_tree() {
        let g = running();
        SESSION.with_borrow_mut(|s| s.loaded = Some(loaded(data())));
        assert!(!finished(g, true, removed()));
        assert_eq!(done_action(), Some(TreeAction::Nothing));
    }

    #[test]
    fn handle_msg_result_with_no_scan_shown_does_not_touch_the_tree() {
        let g = running();
        SESSION.with_borrow_mut(|s| s.loaded = None);
        assert!(!finished(g, true, removed()));
        assert_eq!(done_action(), Some(TreeAction::Nothing));
    }

    #[test]
    fn handle_msg_of_another_generation_or_without_a_job_is_dropped() {
        let g = running();
        assert!(!finished(g + 1, true, removed()));
        assert!(matches!(state(), DialogState::Running { .. }), "{:?}", state());
        assert!(finished(g, true, removed()));
        // A second Finished: there is no job any more.
        assert!(!finished(g, true, removed()));
    }

    fn root_size() -> u64 {
        SESSION.with_borrow(|s| s.loaded.as_ref().map(|l| l.data.tree.node(l.data.tree.root()).size).unwrap())
    }

    #[test]
    fn step_pending_waits_for_the_last_clone_then_applies() {
        let g = running();
        let worker = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.clone());
        assert!(finished(g, true, removed()));
        let seq = SESSION.with_borrow(|s| s.layout_seq);

        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, false)), PendingStep::Busy);
        assert_eq!(root_size(), 160);
        // A busy try never drops a layout, and the dialog does not say the list changed.
        assert_eq!(SESSION.with_borrow(|s| s.layout_seq), seq);
        assert_eq!(done_action(), Some(TreeAction::Detach));

        drop(worker);
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, false)), PendingStep::Applied);
        assert_eq!(root_size(), 10);
        assert_eq!(SESSION.with_borrow(|s| s.layout_seq), seq + 1);
        assert_eq!(done_action(), Some(TreeAction::Detached));
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, false)), PendingStep::Idle);
    }

    #[test]
    fn step_pending_is_dropped_by_a_new_scan() {
        let g = running();
        let _worker = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.clone());
        assert!(finished(g, true, removed()));
        SESSION.with_borrow_mut(scan_changed);
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, false)), PendingStep::Idle);
    }

    #[test]
    fn step_pending_for_data_no_longer_shown_is_dropped() {
        let g = running();
        assert!(finished(g, true, removed()));
        // Other data on screen without `scan_changed`: the origin still tells.
        SESSION.with_borrow_mut(|s| s.loaded = Some(loaded(data())));
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, false)), PendingStep::Dropped);
        assert_eq!(root_size(), 160);
    }

    #[test]
    fn step_pending_after_the_last_timer_try_gives_up_and_says_so() {
        let g = running();
        let _worker = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.clone());
        assert!(finished(g, true, removed()));
        SESSION.with_borrow_mut(|s| s.delete.pending.as_mut().unwrap().tries_left = 2);
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, true)), PendingStep::Busy);
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, true)), PendingStep::GaveUp);
        assert_eq!(done_action(), Some(TreeAction::KeepAndOfferRescan));
        assert_eq!(SESSION.with_borrow(|s| s.delete.notice.clone()), Some(STALE_LIST_NOTICE.to_string()));
        assert_eq!(root_size(), 160);
        // A new scan clears the notice.
        SESSION.with_borrow_mut(scan_changed);
        assert_eq!(SESSION.with_borrow(|s| s.delete.notice.clone()), None);
    }

    #[test]
    fn scan_changed_closes_the_dialog_unless_a_delete_runs() {
        show(data(), "a");
        open(false);
        SESSION.with_borrow_mut(scan_changed);
        assert_eq!(state(), DialogState::Closed);
        assert_eq!(SESSION.with_borrow(|s| s.delete.opened), None);

        running();
        SESSION.with_borrow_mut(scan_changed);
        assert!(matches!(state(), DialogState::Running { .. }), "{:?}", state());
    }

    #[test]
    fn forget_scan_closes_the_dialog() {
        show(data(), "a");
        open(false);
        crate::forget_scan(|_| false);
        assert_eq!(state(), DialogState::Closed);
    }

    fn open_missing(gone: bool) -> bool {
        SESSION.with_borrow_mut(|s| open_in(s, false, |_, _, _| Err(Refusal::Missing), |_| false, |_| gone))
    }

    #[test]
    fn open_in_an_item_gone_from_the_disk_leaves_the_tree() {
        show(data(), "a");
        let a = SESSION.with_borrow(|s| find(&s.loaded.as_ref().unwrap().data.tree, "a"));
        assert!(open_missing(true));
        assert_eq!(state(), DialogState::Refused { text: delete_flow::GONE_PENDING.into() });
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, false)), PendingStep::Applied);
        assert_eq!(root_size(), 10);
        assert_eq!(state(), DialogState::Refused { text: delete_flow::GONE_REMOVED.into() });
        // The selection moved off the removed node.
        assert_ne!(SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().selected), Some(a));
    }

    #[test]
    fn open_in_a_missing_item_without_a_clean_not_found_stays() {
        show(data(), "a");
        assert!(!open_missing(false));
        assert_eq!(state(), DialogState::Refused { text: delete::refusal_text(Refusal::Missing).into() });
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, false)), PendingStep::Idle);
        assert_eq!(root_size(), 160);
    }

    #[test]
    fn open_in_a_gone_item_that_cannot_leave_the_list_says_so() {
        show(data(), "a");
        let _worker = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.clone());
        assert!(open_missing(true));
        SESSION.with_borrow_mut(|s| s.delete.pending.as_mut().unwrap().tries_left = 1);
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, true)), PendingStep::GaveUp);
        assert_eq!(state(), DialogState::Refused { text: delete_flow::GONE_KEPT.into() });
    }

    #[test]
    fn open_in_with_a_dialog_open_or_nothing_shown_does_nothing() {
        show(data(), "a");
        open(false);
        assert!(!open_missing(true));
        assert!(matches!(state(), DialogState::Confirm { .. }));
        SESSION.with_borrow_mut(|s| {
            s.delete = DeleteSession::default();
            s.loaded = None;
        });
        assert!(!open(false));
        assert_eq!(state(), DialogState::Closed);
    }

    #[test]
    fn stop_delete_sets_the_flag_of_a_permanent_delete_only() {
        let flag = || SESSION.with_borrow(|s| s.delete.job.as_ref().map(|j| j.stop.load(Ordering::Relaxed)));
        running();
        stop_delete();
        assert_eq!(flag(), Some(true));

        show(data(), "a");
        open(false);
        confirm().unwrap();
        stop_delete();
        assert_eq!(flag(), Some(false));
    }

    #[test]
    fn switch_and_close_drive_the_dialog() {
        show(data(), "a");
        open(false);
        switch_delete_mode();
        assert!(matches!(state(), DialogState::Confirm { mode: DeleteMode::Permanent, .. }));
        close_delete();
        assert_eq!(state(), DialogState::Closed);
        assert_eq!(SESSION.with_borrow(|s| s.delete.opened), None);
    }

    #[test]
    fn is_link_of_a_plain_folder_is_false() {
        assert!(!delete_flow::is_link(&std::env::temp_dir()));
    }

    // ---- one pending removal at a time ----

    /// Node `a` finished as gone while a worker holds the data: its removal waits. The dialog is
    /// closed and `b.bin` selected. Returns the worker's clone.
    fn a_pending() -> Arc<Data> {
        let g = running();
        let worker = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.clone());
        assert!(finished(g, true, removed()));
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, false)), PendingStep::Busy);
        close_delete();
        select_node("b.bin");
        worker
    }

    fn select_node(name: &str) {
        SESSION.with_borrow_mut(|s| {
            let l = s.loaded.as_mut().unwrap();
            l.selected = Some(find(&l.data.tree, name));
        });
    }

    fn pending_node() -> Option<NodeId> {
        SESSION.with_borrow(|s| s.delete.pending.map(|p| p.node))
    }

    fn node(name: &str) -> NodeId {
        SESSION.with_borrow(|s| find(&s.loaded.as_ref().unwrap().data.tree, name))
    }

    #[test]
    fn open_while_a_removal_waits_is_refused() {
        let _worker = a_pending();
        let a = node("a");
        assert!(!open(false));
        assert_eq!(state(), DialogState::Refused { text: delete_flow::BUSY_TEXT.into() });
        assert_eq!(pending_node(), Some(a));
        assert_eq!(root_size(), 160);
    }

    #[test]
    fn confirm_while_a_removal_waits_is_refused() {
        show(data(), "b.bin");
        open(false);
        let a = node("a");
        let origin = now();
        SESSION.with_borrow_mut(|s| {
            s.delete.pending = Some(Pending { origin, node: a, tries_left: APPLY_TRIES, from_missing: false });
        });
        assert_eq!(confirm(), None);
        assert!(!has_job());
        assert_eq!(state(), DialogState::Refused { text: delete_flow::BUSY_TEXT.into() });
        assert_eq!(pending_node(), Some(a));
    }

    #[test]
    fn a_gone_descendant_of_a_waiting_removal_cannot_be_opened() {
        let _worker = a_pending();
        let a = node("a");
        select_node("x.bin");
        assert!(!open_missing(true));
        assert_eq!(state(), DialogState::Refused { text: delete_flow::BUSY_TEXT.into() });
        assert_eq!(SESSION.with_borrow(|s| s.delete.pending.map(|p| (p.node, p.from_missing))), Some((a, false)));
    }

    #[test]
    fn after_the_waiting_removal_applies_a_delete_opens_again() {
        let worker = a_pending();
        assert!(!open(false));
        close_delete();
        drop(worker);
        assert_eq!(SESSION.with_borrow_mut(|s| step_pending(s, false)), PendingStep::Applied);
        assert_eq!(root_size(), 10);
        assert!(!open(false));
        assert!(matches!(state(), DialogState::Confirm { .. }), "{:?}", state());
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "a second removal while one waits")]
    fn a_second_finished_removal_never_replaces_the_waiting_one() {
        let _worker = a_pending();
        // Only a broken guard could start a job now; make one by hand.
        let (origin, b) = (now(), node("b.bin"));
        SESSION.with_borrow_mut(|s| {
            s.delete.job = Some(Job { origin, node: b, stop: Arc::new(AtomicBool::new(false)) });
            s.delete.state = DialogState::Running {
                mode: DeleteMode::Permanent,
                target: delete_flow::target_info(&s.loaded.as_ref().unwrap().data.tree, &s.loaded.as_ref().unwrap().data.counts, b, PathBuf::from(r"\\?\C:\data\b.bin"), false),
                progress: Default::default(),
                stopping: false,
            };
        });
        finished(origin.generation, true, removed());
    }
}

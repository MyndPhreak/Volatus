//! The delete dialog as a pure state machine, its text (ASD-STE100), the choice from the
//! selection to the dialog, and the worker that runs one delete. No Slint types, so it is unit
//! tested. `scan_apply.rs` drives it on the UI thread.

use crate::delete::{self, DeleteMode, DeleteTarget, Outcome, Refusal, Remover, SkipReason, Skipped};
use crate::model::{ScanState, Unit, display_name, format_size, group_digits};
use scan::{NodeId, Tree};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// `SkipReason::Other` code for a delete engine that panicked. Not a Win32, HRESULT or shell code,
/// and none of the `recycle` codes (`i32::MIN` to `i32::MIN + 4`).
pub const ENGINE_FAILED: i32 = i32::MIN + 16;

/// What the dialog shows about the item, and the checked target to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    /// The item's name, through `display_name`.
    pub name: String,
    /// The resolved path, without the `\\?\` prefix, through `display_name`.
    pub path: String,
    /// The scan's size of the item (`format_size`).
    pub size: String,
    /// Files under the item (1 for a file), from the scan.
    pub files: u64,
    /// Folders under the item, the item itself not included, from the scan.
    pub dirs: u64,
    /// The item is a junction or a symbolic link: only the link goes.
    pub is_link: bool,
    /// The target to delete. Its `path` is the RESOLVED path that `check_target` approved.
    pub target: DeleteTarget,
}

/// Items and bytes removed so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunProgress {
    pub items: u64,
    pub bytes: u64,
}

/// What a finished delete does to the open scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeAction {
    /// The item is gone from the disk: it leaves the tree. Until it has left it (the scan data
    /// can be in use by a worker), the list still shows it.
    Detach,
    /// The item is gone from the disk and has left the list.
    Detached,
    /// The item is still on the disk and the run changed it: the tree stays, a rescan is offered.
    KeepAndOfferRescan,
    /// Nothing changed on the disk.
    Nothing,
}

/// Where the delete dialog is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogState {
    Closed,
    /// The item cannot be deleted. `text` says why. There is no action button.
    Refused { text: String },
    Confirm { mode: DeleteMode, target: TargetInfo },
    /// `stopping`: the user selected Stop (permanent delete only); the run ends after the item in
    /// progress.
    Running { mode: DeleteMode, target: TargetInfo, progress: RunProgress, stopping: bool },
    Done { mode: DeleteMode, target: TargetInfo, outcome: Outcome, tree_action: TreeAction },
}

/// What happened to the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogEvent {
    /// Delete (`shift` false) or Shift+Delete (`shift` true) on a checked item, or a refusal.
    Open { shift: bool, target: Result<TargetInfo, Refusal> },
    /// The link that changes between the Recycle Bin and the permanent delete.
    SwitchMode,
    Confirm,
    /// The check just before the delete refused the item. `text` says why.
    Refuse { text: String },
    Progress { items: u64, bytes: u64 },
    /// `target_gone`: a check of the resolved path after the run found nothing there.
    Finished { outcome: Outcome, target_gone: bool },
    Stop,
    Close,
}

/// The next dialog state. An event that is not valid in the current state is ignored. `Open` works
/// only when the dialog is closed; `SwitchMode` only in `Confirm`; `Stop` only in `Running` with
/// the permanent delete (the Recycle Bin has no Stop); `Close` does nothing while a delete runs.
pub fn next(state: DialogState, event: DialogEvent) -> DialogState {
    use DialogEvent as E;
    use DialogState as S;
    match (state, event) {
        (S::Closed, E::Open { shift, target: Ok(target) }) => S::Confirm { mode: delete::mode_for(shift), target },
        (S::Closed, E::Open { target: Err(r), .. }) => S::Refused { text: delete::refusal_text(r).to_string() },
        (S::Confirm { mode, target }, E::SwitchMode) => S::Confirm { mode: other(mode), target },
        (S::Confirm { .. }, E::Refuse { text }) => S::Refused { text },
        (S::Confirm { mode, target }, E::Confirm) => {
            S::Running { mode, target, progress: RunProgress::default(), stopping: false }
        }
        (S::Confirm { .. } | S::Refused { .. } | S::Done { .. }, E::Close) => S::Closed,
        (S::Running { mode, target, stopping, .. }, E::Progress { items, bytes }) => {
            S::Running { mode, target, progress: RunProgress { items, bytes }, stopping }
        }
        (S::Running { mode: DeleteMode::Permanent, target, progress, .. }, E::Stop) => {
            S::Running { mode: DeleteMode::Permanent, target, progress, stopping: true }
        }
        (S::Running { mode, target, .. }, E::Finished { outcome, target_gone }) => {
            let tree_action = tree_action(&outcome, mode, target_gone);
            S::Done { mode, target, outcome, tree_action }
        }
        (s, _) => s,
    }
}

/// The mode the switch link changes to.
fn other(mode: DeleteMode) -> DeleteMode {
    match mode {
        DeleteMode::RecycleBin => DeleteMode::Permanent,
        DeleteMode::Permanent => DeleteMode::RecycleBin,
    }
}

/// What a finished run does to the open scan. `target_gone` comes from a check of the resolved
/// path after the run, never from the `Outcome` alone:
///
/// | The item after the run          | Action               |
/// |---------------------------------|----------------------|
/// | gone from the disk              | `Detach`             |
/// | still there, skips or removals  | `KeepAndOfferRescan` |
/// | still there, nothing removed    | `Nothing`            |
///
/// One exception: a Recycle Bin run that removed nothing, where every skip is one the bin
/// refused before anything changed (`recycle::left_in_place`), is `Nothing`. A rescan would show
/// the same disk.
pub fn tree_action(outcome: &Outcome, mode: DeleteMode, target_gone: bool) -> TreeAction {
    let refused_by_bin = mode == DeleteMode::RecycleBin
        && outcome.removed_items == 0
        && !outcome.skipped.is_empty()
        && outcome.skipped.iter().all(|s| crate::recycle::left_in_place(s.reason));
    if target_gone {
        TreeAction::Detach
    } else if refused_by_bin {
        TreeAction::Nothing
    } else if !outcome.skipped.is_empty() || outcome.removed_items > 0 {
        TreeAction::KeepAndOfferRescan
    } else {
        TreeAction::Nothing
    }
}

/// A delete can start only while no scan runs.
pub fn can_start(scan: &ScanState) -> bool {
    !matches!(scan, ScanState::Scanning { .. })
}

/// The confirm text, line by line in the order the dialog shows it.
#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmText {
    /// The name; a link is named as a link.
    pub name: String,
    pub path: String,
    pub size: String,
    /// "N files and M folders, from the scan."
    pub counts: String,
    /// What the action button does.
    pub effect: &'static str,
    /// For a link: only the link goes.
    pub link_note: Option<&'static str>,
}

/// The dialog's buttons for a mode.
#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buttons {
    pub action: &'static str,
    pub cancel: &'static str,
    /// Enter starts the action. False for the permanent delete: there, Cancel is the default.
    pub action_is_default: bool,
}

#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
pub fn confirm_text(mode: DeleteMode, t: &TargetInfo) -> ConfirmText {
    ConfirmText {
        name: if t.is_link { format!("Link: {}", t.name) } else { t.name.clone() },
        path: t.path.clone(),
        size: t.size.clone(),
        counts: counts_text(t.files, t.dirs),
        effect: match mode {
            DeleteMode::RecycleBin => {
                "Move to Recycle Bin puts the item in the Recycle Bin. You can restore it in Windows."
            }
            DeleteMode::Permanent => "Delete permanently removes the item from the disk. There is no undo.",
        },
        link_note: t
            .is_link
            .then_some("The item is a link. Volatus deletes only the link. The item that the link points to stays."),
    }
}

/// "1 file", "3 folders": the count with digit groups, the word in the singular for 1.
fn count(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", group_digits(n), if n == 1 { one } else { many })
}

/// "12 files and 3 folders, from the scan."
#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
pub fn counts_text(files: u64, dirs: u64) -> String {
    format!("{} and {}, from the scan.", count(files, "file", "files"), count(dirs, "folder", "folders"))
}

#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
pub fn buttons(mode: DeleteMode) -> Buttons {
    match mode {
        DeleteMode::RecycleBin => Buttons { action: "Move to Recycle Bin", cancel: "Cancel", action_is_default: true },
        DeleteMode::Permanent => Buttons { action: "Delete permanently", cancel: "Cancel", action_is_default: false },
    }
}

/// The text of the link that changes to the other mode.
#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
pub fn switch_text(mode: DeleteMode) -> &'static str {
    match mode {
        DeleteMode::RecycleBin => "Delete permanently instead",
        DeleteMode::Permanent => "Move to Recycle Bin instead",
    }
}

/// The line under the progress bar while a delete runs.
#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
pub fn progress_line(mode: DeleteMode, p: RunProgress) -> String {
    match mode {
        // One shell operation with no progress of its own.
        DeleteMode::RecycleBin => "Volatus moves the item to the Recycle Bin.".to_string(),
        DeleteMode::Permanent => format!("Removed {} so far ({}).", count(p.items, "item", "items"), format_size(p.bytes, Unit::Dynamic)),
    }
}

/// The result text, one short sentence per line (`delete::summary_lines`).
#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
pub fn result_lines(mode: DeleteMode, o: &Outcome) -> Vec<String> {
    delete::summary_lines(mode, o)
}

/// One line of the skip list: the path (no `\\?\`, through `display_name`), then the reason in
/// plain words. The `recycle` engine's own texts are used where it has one.
#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
pub fn skip_line(s: &Skipped) -> String {
    let reason: std::borrow::Cow<'static, str> = match crate::recycle::skip_text(s.reason) {
        Some(text) => text.into(),
        None => match s.reason {
            SkipReason::Locked => "Another program uses the item.".into(),
            SkipReason::AccessDenied => {
                "Windows does not give access to the item. Restart as administrator to try again.".into()
            }
            SkipReason::NeedsAdmin => "The item needs administrator rights.".into(),
            SkipReason::TooLong => "The path is too long.".into(),
            SkipReason::Other(ENGINE_FAILED) => {
                "Volatus stopped because of an internal error. Scan the folder again to see what is left.".into()
            }
            SkipReason::Other(code) => format!("Windows reported error {code}.").into(),
        },
    };
    format!("{}: {reason}", shown_path(&s.path))
}

/// A path as the dialog shows it: no `\\?\` prefix, hidden characters as placeholders.
fn shown_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    display_name(s.strip_prefix(r"\\?\").unwrap_or(&s)).into_owned()
}

/// The label of the "Restart as administrator" button, when a skip needs more rights
/// (`NeedsAdmin` or `AccessDenied`).
#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
pub fn admin_hint(o: &Outcome) -> Option<&'static str> {
    o.skipped
        .iter()
        .any(|s| matches!(s.reason, SkipReason::NeedsAdmin | SkipReason::AccessDenied))
        .then_some("Restart as administrator")
}

/// The refusal text when the check just before the delete finds another resolved path.
pub const CHANGED_TEXT: &str = "This item changed. Open the dialog again.";
/// The refusal text for an item gone from the disk, while it waits to leave the list.
pub const GONE_PENDING: &str = "This item is already gone. Volatus removes it from the list when the scan data is free.";
/// The refusal text for an item gone from the disk, after it left the list.
pub const GONE_REMOVED: &str = "This item is already gone. Volatus removed it from the list.";
/// The refusal text for an item gone from the disk that could not leave the list.
pub const GONE_KEPT: &str = "This item is already gone. Scan again to update the list.";
/// The refusal text while an earlier removal still waits to leave the list.
pub const BUSY_TEXT: &str = "Volatus is still updating the list. Try again in a moment.";

/// The check just before the delete: `Ok` only when it gives the same resolved path as the check
/// at Open (`opened`). Else the text for the dialog: the refusal's, or `CHANGED_TEXT`.
pub fn recheck(opened: &Path, again: Result<PathBuf, Refusal>) -> Result<(), String> {
    match again {
        Ok(p) if p == opened => Ok(()),
        Ok(_) => Err(CHANGED_TEXT.to_string()),
        Err(r) => Err(delete::refusal_text(r).to_string()),
    }
}

/// The text for a refusal at Open, and whether the item leaves the tree. Only a `Missing` item
/// that `gone` (a strict "not found" check) confirms leaves it; `gone` is asked for `Missing` only.
pub fn refusal_for(r: Refusal, gone: impl FnOnce() -> bool) -> (String, bool) {
    if r == Refusal::Missing && gone() {
        (GONE_PENDING.to_string(), true)
    } else {
        (delete::refusal_text(r).to_string(), false)
    }
}

/// What the result says about the list, for a tree action (ASD-STE100).
#[cfg_attr(not(test), expect(dead_code, reason = "shown by the dialog in Task 7b"))]
pub fn list_line(action: TreeAction) -> Option<&'static str> {
    match action {
        TreeAction::Detach => Some("The list still shows the item. Volatus removes it when the scan data is free."),
        TreeAction::Detached => Some("Volatus removed the item from the list."),
        TreeAction::KeepAndOfferRescan => Some("The list did not change. Scan again to see the changes."),
        TreeAction::Nothing => None,
    }
}

/// The checked target for Delete or Shift+Delete on `selected`, with its node; `None` (no dialog)
/// when nothing is selected, the id is not in the tree, or a scan runs. Else
/// `check(scan_root, path, is_dir)` decides: a refusal, or the target with the resolved path
/// stored for the delete. `scan_root` is the root AS TYPED: the policy resolves it itself.
/// `is_link(resolved)` is asked for a file item only (the scan records a link as a file).
pub fn open_check(
    tree: &Tree,
    counts: &[u64],
    selected: Option<NodeId>,
    scan: &ScanState,
    scan_root: &Path,
    check: impl FnOnce(&Path, &Path, bool) -> Result<PathBuf, Refusal>,
    is_link: impl FnOnce(&Path) -> bool,
) -> Option<(NodeId, Result<TargetInfo, Refusal>)> {
    // An id from another tree (a stale row) is not looked up.
    let node = selected.filter(|n| can_start(scan) && (n.0 as usize) < counts.len())?;
    let is_dir = tree.node(node).is_dir;
    let target = check(scan_root, &tree.path(node), is_dir).map(|resolved| {
        let link = !is_dir && is_link(&resolved);
        target_info(tree, counts, node, resolved, link)
    });
    Some((node, target))
}

/// The dialog's view of `node` with its resolved path.
pub fn target_info(tree: &Tree, counts: &[u64], node: NodeId, resolved: PathBuf, is_link: bool) -> TargetInfo {
    let n = tree.node(node);
    let (files, dirs) = (counts[node.0 as usize], folders_under(tree, node));
    TargetInfo {
        name: display_name(tree.name(node)).into_owned(),
        path: shown_path(&resolved),
        size: format_size(n.size, Unit::Dynamic),
        files,
        dirs,
        is_link,
        target: DeleteTarget { node, path: resolved, is_dir: n.is_dir, is_link, size: n.size, files, dirs },
    }
}

/// Folders under `node`, `node` itself not included. Walks the subtree once.
fn folders_under(tree: &Tree, node: NodeId) -> u64 {
    let mut stack: Vec<NodeId> = tree.children(node).collect();
    let mut dirs = 0;
    while let Some(n) = stack.pop() {
        if tree.node(n).is_dir {
            dirs += 1;
            stack.extend(tree.children(n));
        }
    }
    dirs
}

/// True when `p` is a reparse point (a junction or a symbolic link). Not followed. False when the
/// item cannot be read: the dialog then names it as a plain file, and the engines still remove
/// a link as a link.
pub fn is_link(p: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// What the delete worker posts to the UI thread, with the scan generation of its job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteMsg {
    Progress { generation: u64, items: u64, bytes: u64 },
    Finished { generation: u64, outcome: Outcome, target_gone: bool },
}

impl DeleteMsg {
    pub fn generation(&self) -> u64 {
        match self {
            DeleteMsg::Progress { generation, .. } | DeleteMsg::Finished { generation, .. } => *generation,
        }
    }
}

/// True only when the file system says `p` is not found. Any other answer (access denied, a
/// failing disk) counts as still there, so the tree never drops an item that may exist.
pub fn target_gone(p: &Path) -> bool {
    matches!(std::fs::symlink_metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// The `Outcome` for a run whose engine failed: nothing known removed, the target skipped with
/// `ENGINE_FAILED`.
pub fn failed_outcome(t: &DeleteTarget) -> Outcome {
    Outcome { skipped: vec![Skipped { path: t.path.clone(), reason: SkipReason::Other(ENGINE_FAILED) }], ..Outcome::default() }
}

/// Runs `remover` on `target` on THIS thread: each progress call is posted as `Progress`, then
/// `Finished` is posted exactly once, with `gone(target.path)` checked after the run. A panic in
/// the remover is caught and posted as `failed_outcome`.
pub fn run_delete(
    remover: &dyn Remover,
    target: &DeleteTarget,
    generation: u64,
    stop: &AtomicBool,
    gone: &dyn Fn(&Path) -> bool,
    post: &dyn Fn(DeleteMsg),
) {
    let progress = |items, bytes| post(DeleteMsg::Progress { generation, items, bytes });
    // A panic is turned into a skip, never lost. The engines keep no state across runs that a
    // panic could leave broken, so going on after one is safe.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| remover.remove(target, &progress, stop)))
        .unwrap_or_else(|_| failed_outcome(target));
    let target_gone = gone(&target.path);
    post(DeleteMsg::Finished { generation, outcome, target_gone });
}

/// Runs `remover` on a new worker thread (`run_delete` with `target_gone`). Never runs the
/// remover on the caller's thread. `Err` when the thread cannot start: then nothing ran.
pub fn spawn_delete(
    remover: Arc<dyn Remover>,
    target: DeleteTarget,
    generation: u64,
    stop: Arc<AtomicBool>,
    post: impl Fn(DeleteMsg) + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("volatus-delete".into())
        .spawn(move || run_delete(&*remover, &target, generation, &stop, &target_gone, &post))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delete::tests::{Fake, env};
    use crate::delete::{Env, check_target_in};
    use crate::model::Summary;
    use crate::tree_view::file_counts;
    use scan::rollup::{RawEntry, rollup};
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    use DeleteMode::{Permanent, RecycleBin};

    /// root `C:\data`(0) { `big`(1) { `a.bin`(3) 100, `sub`(4) { `b.bin`(5) 50 } }, `lnk`(2) 0 }
    fn tree() -> (Tree, Vec<u64>) {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
        };
        let entries = vec![
            e(0, 0, r"C:\data", 0, true),
            e(1, 0, "big", 0, true),
            e(2, 0, "lnk", 0, false),
            e(3, 1, "a.bin", 100, false),
            e(4, 1, "sub", 0, true),
            e(5, 4, "b.bin", 50, false),
        ];
        let tree = rollup(0, entries).unwrap().tree;
        let counts = file_counts(&tree);
        (tree, counts)
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

    fn info(mode_link: bool) -> TargetInfo {
        let (tree, counts) = tree();
        let node = find(&tree, if mode_link { "lnk" } else { "big" });
        let path = if mode_link { r"\\?\C:\data\lnk" } else { r"\\?\C:\data\big" };
        target_info(&tree, &counts, node, PathBuf::from(path), mode_link)
    }

    fn done() -> Outcome {
        Outcome { removed_items: 4, removed_bytes: 150, ..Outcome::default() }
    }

    fn skip(path: &str, reason: SkipReason) -> Skipped {
        Skipped { path: PathBuf::from(path), reason }
    }

    fn confirm(mode: DeleteMode) -> DialogState {
        DialogState::Confirm { mode, target: info(false) }
    }

    fn running(mode: DeleteMode, stopping: bool) -> DialogState {
        DialogState::Running { mode, target: info(false), progress: RunProgress::default(), stopping }
    }

    fn done_state(mode: DeleteMode) -> DialogState {
        DialogState::Done { mode, target: info(false), outcome: done(), tree_action: TreeAction::Detach }
    }

    // ---- the state machine ----

    #[test]
    fn next_valid_transitions() {
        use DialogEvent as E;
        use DialogState as S;
        let refused = S::Refused { text: delete::refusal_text(Refusal::ProtectedFolder).to_string() };
        let finished = E::Finished { outcome: done(), target_gone: true };
        let cases: Vec<(S, E, S)> = vec![
            (S::Closed, E::Open { shift: false, target: Ok(info(false)) }, confirm(RecycleBin)),
            (S::Closed, E::Open { shift: true, target: Ok(info(false)) }, confirm(Permanent)),
            (S::Closed, E::Open { shift: false, target: Err(Refusal::ProtectedFolder) }, refused.clone()),
            (S::Closed, E::Open { shift: true, target: Err(Refusal::ProtectedFolder) }, refused.clone()),
            (confirm(RecycleBin), E::SwitchMode, confirm(Permanent)),
            (confirm(Permanent), E::SwitchMode, confirm(RecycleBin)),
            (confirm(RecycleBin), E::Confirm, running(RecycleBin, false)),
            (confirm(Permanent), E::Confirm, running(Permanent, false)),
            (confirm(Permanent), E::Close, S::Closed),
            (refused.clone(), E::Close, S::Closed),
            (
                running(Permanent, false),
                E::Progress { items: 3, bytes: 70 },
                S::Running { mode: Permanent, target: info(false), progress: RunProgress { items: 3, bytes: 70 }, stopping: false },
            ),
            (running(Permanent, false), E::Stop, running(Permanent, true)),
            (
                running(Permanent, true),
                E::Progress { items: 1, bytes: 2 },
                S::Running { mode: Permanent, target: info(false), progress: RunProgress { items: 1, bytes: 2 }, stopping: true },
            ),
            (running(Permanent, true), finished.clone(), done_state(Permanent)),
            (running(RecycleBin, false), finished.clone(), done_state(RecycleBin)),
            (done_state(RecycleBin), E::Close, S::Closed),
        ];
        for (from, event, want) in cases {
            let got = next(from.clone(), event.clone());
            assert_eq!(got, want, "from {from:?} on {event:?}");
        }
    }

    #[test]
    fn next_invalid_events_are_ignored() {
        use DialogEvent as E;
        use DialogState as S;
        let refused = S::Refused { text: "x".into() };
        let open = E::Open { shift: false, target: Ok(info(false)) };
        let finished = E::Finished { outcome: done(), target_gone: true };
        let progress = E::Progress { items: 1, bytes: 1 };
        let cases: Vec<(S, Vec<E>)> = vec![
            (S::Closed, vec![E::SwitchMode, E::Confirm, progress.clone(), finished.clone(), E::Stop, E::Close]),
            (refused.clone(), vec![open.clone(), E::SwitchMode, E::Confirm, progress.clone(), finished.clone(), E::Stop]),
            (confirm(RecycleBin), vec![open.clone(), progress.clone(), finished.clone(), E::Stop]),
            // The Recycle Bin has no Stop; nothing closes a running delete.
            (running(RecycleBin, false), vec![open.clone(), E::SwitchMode, E::Confirm, E::Stop, E::Close]),
            (running(Permanent, false), vec![open.clone(), E::SwitchMode, E::Confirm, E::Close]),
            (done_state(Permanent), vec![open.clone(), E::SwitchMode, E::Confirm, progress, finished, E::Stop]),
        ];
        for (from, events) in cases {
            for event in events {
                assert_eq!(next(from.clone(), event.clone()), from, "{event:?} must not change {from:?}");
            }
        }
        // A changed state proves the table above is not a stub that ignores everything.
        assert_ne!(next(confirm(RecycleBin), E::SwitchMode), confirm(RecycleBin));
    }

    #[test]
    fn next_finished_takes_the_tree_action_from_the_outcome_and_the_disk() {
        let kept = Outcome { removed_items: 2, skipped: vec![skip(r"C:\data\big\a.bin", SkipReason::Locked)], ..Outcome::default() };
        let got = next(running(Permanent, false), DialogEvent::Finished { outcome: kept.clone(), target_gone: false });
        assert_eq!(
            got,
            DialogState::Done { mode: Permanent, target: info(false), outcome: kept, tree_action: TreeAction::KeepAndOfferRescan }
        );
    }

    // ---- tree_action ----

    #[test]
    fn tree_action_table() {
        use TreeAction::{Detach, KeepAndOfferRescan, Nothing};
        for mode in [RecycleBin, Permanent] {
            for skips in [false, true] {
                for removed in [0u64, 1, 7] {
                    for gone in [false, true] {
                        let o = Outcome {
                            removed_items: removed,
                            removed_bytes: removed * 10,
                            skipped: if skips { vec![skip(r"C:\data\big\a.bin", SkipReason::Locked)] } else { Vec::new() },
                            stopped: false,
                        };
                        let want = match (gone, skips, removed) {
                            (true, _, _) => Detach,
                            (false, true, _) | (false, false, 1..) => KeepAndOfferRescan,
                            (false, false, 0) => Nothing,
                        };
                        assert_eq!(tree_action(&o, mode, gone), want, "mode {mode:?} skips {skips} removed {removed} gone {gone}");
                    }
                }
            }
        }
    }

    #[test]
    fn tree_action_recycle_success_detaches_only_when_the_item_is_gone() {
        let o = Outcome { removed_items: 1, removed_bytes: 150, ..Outcome::default() };
        assert_eq!(tree_action(&o, RecycleBin, true), TreeAction::Detach);
        assert_eq!(tree_action(&o, RecycleBin, false), TreeAction::KeepAndOfferRescan);
    }

    #[test]
    fn tree_action_partial_delete_whose_folder_is_gone_detaches() {
        let o = Outcome { removed_items: 9, skipped: vec![skip(r"C:\data\big\a.bin", SkipReason::Locked)], stopped: true, ..Outcome::default() };
        assert_eq!(tree_action(&o, Permanent, true), TreeAction::Detach);
    }

    // ---- can_start ----

    #[test]
    fn can_start_is_false_only_while_scanning() {
        let summary = Summary {
            method: scan::Method::Walk,
            files: 1,
            dirs: 1,
            skipped: 0,
            elapsed: std::time::Duration::ZERO,
            phases: Default::default(),
            total: 1,
        };
        for (state, want) in [
            (ScanState::Idle, true),
            (ScanState::Scanning { files: 0, dirs: 0 }, false),
            (ScanState::Scanning { files: 9, dirs: 2 }, false),
            (ScanState::Done(summary), true),
            (ScanState::Error("x".into()), true),
            (ScanState::Cancelled, true),
        ] {
            assert_eq!(can_start(&state), want, "{state:?}");
        }
    }

    // ---- text ----

    #[test]
    fn target_info_takes_the_name_path_size_and_counts_from_the_scan() {
        let t = info(false);
        assert_eq!((t.name.as_str(), t.path.as_str(), t.size.as_str()), ("big", r"C:\data\big", "150 B"));
        assert_eq!((t.files, t.dirs, t.is_link), (2, 1, false));
        let (tree, _) = tree();
        let want = DeleteTarget {
            node: find(&tree, "big"),
            path: PathBuf::from(r"\\?\C:\data\big"),
            is_dir: true,
            is_link: false,
            size: 150,
            files: 2,
            dirs: 1,
        };
        assert_eq!(t.target, want);
    }

    #[test]
    fn target_info_of_a_file_counts_one_file_and_no_folder() {
        let (tree, counts) = tree();
        let t = target_info(&tree, &counts, find(&tree, "b.bin"), PathBuf::from(r"\\?\C:\data\big\sub\b.bin"), false);
        assert_eq!((t.files, t.dirs, t.target.is_dir), (1, 0, false));
    }

    #[test]
    fn target_info_shows_hidden_characters_as_placeholders() {
        let (tree, counts) = tree();
        let t = target_info(&tree, &counts, find(&tree, "big"), PathBuf::from("\\\\?\\C:\\data\\b\u{202E}ig"), false);
        assert_eq!(t.path, "C:\\data\\b\u{FFFD}ig");
    }

    #[test]
    fn confirm_text_recycle_bin() {
        let c = confirm_text(RecycleBin, &info(false));
        assert_eq!(
            c,
            ConfirmText {
                name: "big".into(),
                path: r"C:\data\big".into(),
                size: "150 B".into(),
                counts: "2 files and 1 folder, from the scan.".into(),
                effect: "Move to Recycle Bin puts the item in the Recycle Bin. You can restore it in Windows.",
                link_note: None,
            }
        );
        assert!(c.effect.contains("Recycle Bin"));
    }

    #[test]
    fn confirm_text_permanent_says_there_is_no_undo() {
        let c = confirm_text(Permanent, &info(false));
        assert_eq!(c.effect, "Delete permanently removes the item from the disk. There is no undo.");
        assert!(c.effect.contains("no undo"));
        assert!(!c.effect.contains("Recycle Bin"));
    }

    #[test]
    fn confirm_text_names_a_link_as_a_link() {
        let c = confirm_text(RecycleBin, &info(true));
        assert_eq!(c.name, "Link: lnk");
        assert_eq!(c.link_note, Some("The item is a link. Volatus deletes only the link. The item that the link points to stays."));
        assert_eq!(c.counts, "1 file and 0 folders, from the scan.");
    }

    #[test]
    fn counts_text_uses_the_singular_and_groups_digits() {
        for (files, dirs, want) in [
            (0, 0, "0 files and 0 folders, from the scan."),
            (1, 1, "1 file and 1 folder, from the scan."),
            (2, 3, "2 files and 3 folders, from the scan."),
            (1_234_567, 12_000, "1,234,567 files and 12,000 folders, from the scan."),
        ] {
            assert_eq!(counts_text(files, dirs), want);
        }
    }

    #[test]
    fn buttons_by_mode() {
        for (mode, want) in [
            (RecycleBin, Buttons { action: "Move to Recycle Bin", cancel: "Cancel", action_is_default: true }),
            (Permanent, Buttons { action: "Delete permanently", cancel: "Cancel", action_is_default: false }),
        ] {
            assert_eq!(buttons(mode), want, "{mode:?}");
        }
    }

    #[test]
    fn buttons_from_the_key_table() {
        // Plain Delete gives the Recycle Bin as the default; Shift gives the permanent delete, not default.
        assert_eq!(buttons(delete::mode_for(false)).action, "Move to Recycle Bin");
        assert!(buttons(delete::mode_for(false)).action_is_default);
        assert_eq!(buttons(delete::mode_for(true)).action, "Delete permanently");
        assert!(!buttons(delete::mode_for(true)).action_is_default);
    }

    #[test]
    fn switch_text_names_the_other_mode() {
        assert_eq!(switch_text(RecycleBin), "Delete permanently instead");
        assert_eq!(switch_text(Permanent), "Move to Recycle Bin instead");
    }

    #[test]
    fn progress_line_by_mode() {
        let p = RunProgress { items: 1234, bytes: 2048 };
        assert_eq!(progress_line(Permanent, p), "Removed 1,234 items so far (2.0 KB).");
        assert_eq!(progress_line(Permanent, RunProgress { items: 1, bytes: 5 }), "Removed 1 item so far (5 B).");
        assert_eq!(progress_line(RecycleBin, p), "Volatus moves the item to the Recycle Bin.");
    }

    #[test]
    fn result_lines_are_the_summary_lines() {
        let o = Outcome { removed_items: 1, removed_bytes: 5000, ..Outcome::default() };
        let lines = result_lines(RecycleBin, &o);
        assert_eq!(lines, delete::summary_lines(RecycleBin, &o));
        // The Recycle Bin result says the space is free only when the bin is emptied.
        assert!(lines.iter().any(|l| l.contains("empty the Recycle Bin")), "{lines:?}");
        let p = result_lines(Permanent, &done());
        assert_eq!(p, delete::summary_lines(Permanent, &done()));
        assert!(!p.is_empty());
    }

    #[test]
    fn skip_line_for_each_reason() {
        for (reason, want) in [
            (SkipReason::Locked, r"C:\data\a.bin: Another program uses the item."),
            (SkipReason::AccessDenied, r"C:\data\a.bin: Windows does not give access to the item. Restart as administrator to try again."),
            (SkipReason::NeedsAdmin, r"C:\data\a.bin: The item needs administrator rights."),
            (SkipReason::TooLong, r"C:\data\a.bin: The path is too long."),
            (SkipReason::Other(87), r"C:\data\a.bin: Windows reported error 87."),
            (
                SkipReason::Other(ENGINE_FAILED),
                r"C:\data\a.bin: Volatus stopped because of an internal error. Scan the folder again to see what is left.",
            ),
        ] {
            assert_eq!(skip_line(&skip(r"\\?\C:\data\a.bin", reason)), want, "{reason:?}");
        }
    }

    #[test]
    fn skip_line_keeps_the_recycle_engine_texts() {
        for code in [crate::recycle::PATH_REFUSED, crate::recycle::NOT_RECYCLABLE, crate::recycle::NOT_IN_BIN] {
            let r = SkipReason::Other(code);
            let want = format!(r"C:\data\a.bin: {}", crate::recycle::skip_text(r).unwrap());
            assert_eq!(skip_line(&skip(r"\\?\C:\data\a.bin", r)), want, "{code}");
        }
    }

    #[test]
    fn skip_line_shows_hidden_characters_as_placeholders() {
        assert_eq!(skip_line(&skip("C:\\x\u{202E}y", SkipReason::TooLong)), "C:\\x\u{FFFD}y: The path is too long.");
    }

    #[test]
    fn admin_hint_for_rights_only() {
        let with = |r| Outcome { skipped: vec![skip(r"C:\a", SkipReason::Locked), skip(r"C:\b", r)], ..Outcome::default() };
        assert_eq!(admin_hint(&with(SkipReason::NeedsAdmin)), Some("Restart as administrator"));
        assert_eq!(admin_hint(&with(SkipReason::AccessDenied)), Some("Restart as administrator"));
        assert_eq!(admin_hint(&with(SkipReason::TooLong)), None);
        assert_eq!(admin_hint(&done()), None);
    }

    // ---- open_for ----

    /// The dialog for Delete (`shift` false) or Shift+Delete on `selected`. `Closed` when nothing is
    /// selected, the id is not in the tree, or a scan runs. Else `check(scan_root, path, is_dir)`
    /// decides: `Refused` with its text, or `Confirm` with the resolved path stored for the delete.
    /// `scan_root` is the root AS TYPED: the policy resolves it itself. `is_link(resolved)` is asked
    /// for a file item only (the scan records a link as a file).
    #[expect(clippy::too_many_arguments, reason = "the scan's parts plus the two seams the tests replace")]
    fn open_for(
        tree: &Tree,
        counts: &[u64],
        selected: Option<NodeId>,
        scan: &ScanState,
        scan_root: &Path,
        shift: bool,
        check: impl FnOnce(&Path, &Path, bool) -> Result<PathBuf, Refusal>,
        is_link: impl FnOnce(&Path) -> bool,
    ) -> DialogState {
        match open_check(tree, counts, selected, scan, scan_root, check, is_link) {
            None => DialogState::Closed,
            Some((_, target)) => next(DialogState::Closed, DialogEvent::Open { shift, target }),
        }
    }

    fn fake() -> Fake {
        Fake::with(&[(r"C:\data\big", true), (r"C:\data\lnk", false), (r"C:\data\big\a.bin", false)])
    }

    /// `open_for` on node `name`, the checks done by `check_target_in` on `fake()` and `env()`.
    /// Returns the state and the root the check was given.
    fn open(name: Option<&str>, scan: &ScanState, root: &str, shift: bool, link: bool) -> (DialogState, Option<PathBuf>) {
        let (tree, counts) = tree();
        let selected = name.map(|n| find(&tree, n));
        let seen = std::cell::RefCell::new(None);
        let fs = fake();
        let e: Env = env();
        let state = open_for(
            &tree,
            &counts,
            selected,
            scan,
            Path::new(root),
            shift,
            |r, t, d| {
                *seen.borrow_mut() = Some(r.to_path_buf());
                check_target_in(r, t, d, &fs, &e)
            },
            |_| link,
        );
        (state, seen.into_inner())
    }

    fn no_scan_running() -> ScanState {
        ScanState::Idle
    }

    #[test]
    fn open_for_a_folder_confirms_with_the_resolved_path() {
        let (state, root) = open(Some("big"), &no_scan_running(), r"C:\data", false, false);
        let DialogState::Confirm { mode, target } = state else { panic!("{state:?}") };
        assert_eq!(mode, RecycleBin);
        assert_eq!(target.target.path, PathBuf::from(r"c:\data\big"));
        assert_eq!((target.name.as_str(), target.files, target.dirs, target.is_link), ("big", 2, 1, false));
        // The root goes to the policy as typed, not resolved.
        assert_eq!(root, Some(PathBuf::from(r"C:\data")));
    }

    #[test]
    fn open_for_with_shift_is_the_permanent_delete() {
        let (state, _) = open(Some("big"), &no_scan_running(), r"C:\data", true, false);
        assert!(matches!(state, DialogState::Confirm { mode: Permanent, .. }), "{state:?}");
    }

    #[test]
    fn open_for_a_link_names_it_as_a_link() {
        let (state, _) = open(Some("lnk"), &no_scan_running(), r"C:\data", false, true);
        let DialogState::Confirm { target, .. } = state else { panic!("{state:?}") };
        assert!(target.is_link && target.target.is_link);
    }

    #[test]
    fn open_for_a_folder_does_not_ask_if_it_is_a_link() {
        let (state, _) = open(Some("big"), &no_scan_running(), r"C:\data", false, true);
        let DialogState::Confirm { target, .. } = state else { panic!("{state:?}") };
        assert!(!target.is_link);
    }

    #[test]
    fn open_for_the_scan_root_is_refused() {
        let (state, _) = open(Some(r"C:\data"), &no_scan_running(), r"C:\data", false, false);
        assert_eq!(state, DialogState::Refused { text: delete::refusal_text(Refusal::NotUnderRoot).into() });
    }

    #[test]
    fn open_for_an_item_gone_from_the_disk_is_refused_as_missing() {
        let (state, _) = open(Some("b.bin"), &no_scan_running(), r"C:\data", false, false);
        assert_eq!(state, DialogState::Refused { text: delete::refusal_text(Refusal::Missing).into() });
    }

    #[test]
    fn open_for_nothing_selected_or_while_scanning_is_closed() {
        assert_eq!(open(None, &no_scan_running(), r"C:\data", false, false), (DialogState::Closed, None));
        let scanning = ScanState::Scanning { files: 1, dirs: 1 };
        assert_eq!(open(Some("big"), &scanning, r"C:\data", false, false), (DialogState::Closed, None));
        // A real open proves the closed results above are not a stub's.
        assert!(matches!(open(Some("big"), &no_scan_running(), r"C:\data", false, false).0, DialogState::Confirm { .. }));
    }

    #[test]
    fn open_for_an_id_outside_the_tree_is_closed() {
        let (tree, counts) = tree();
        let called = std::cell::Cell::new(false);
        let check = |_: &Path, _: &Path, _: bool| {
            called.set(true);
            Ok(PathBuf::new())
        };
        let s = open_for(&tree, &counts, Some(NodeId(999)), &ScanState::Idle, Path::new(r"C:\data"), false, check, |_| false);
        assert_eq!((s, called.get()), (DialogState::Closed, false));
        let ok = open_for(&tree, &counts, Some(find(&tree, "big")), &ScanState::Idle, Path::new(r"C:\data"), false, |_, _, _| Ok(PathBuf::from(r"C:\data\big")), |_| false);
        assert!(matches!(ok, DialogState::Confirm { .. }));
    }

    // ---- the worker ----

    /// A remover that records each call, sends the progress it was given, checks `stop`, and
    /// returns a canned outcome (or panics).
    struct FakeRemover {
        calls: Mutex<Vec<(PathBuf, bool)>>,
        progress: Vec<(u64, u64)>,
        outcome: Outcome,
        panic: bool,
    }

    impl FakeRemover {
        fn new(progress: Vec<(u64, u64)>, outcome: Outcome) -> FakeRemover {
            FakeRemover { calls: Mutex::new(Vec::new()), progress, outcome, panic: false }
        }
    }

    impl Remover for FakeRemover {
        fn remove(&self, t: &DeleteTarget, progress: &dyn Fn(u64, u64), stop: &AtomicBool) -> Outcome {
            self.calls.lock().unwrap().push((t.path.clone(), stop.load(Ordering::Relaxed)));
            for &(i, b) in &self.progress {
                progress(i, b);
            }
            if self.panic {
                panic!("fake remover failed");
            }
            self.outcome.clone()
        }
    }

    fn target(path: &str) -> DeleteTarget {
        DeleteTarget { node: NodeId(1), path: PathBuf::from(path), is_dir: true, is_link: false, size: 150, files: 2, dirs: 1 }
    }

    fn run(remover: &FakeRemover, gone: bool) -> Vec<DeleteMsg> {
        let got = Mutex::new(Vec::new());
        let stop = AtomicBool::new(false);
        run_delete(remover, &target(r"\\?\C:\data\big"), 7, &stop, &|_| gone, &|m| got.lock().unwrap().push(m));
        got.into_inner().unwrap()
    }

    #[test]
    fn run_delete_forwards_progress_in_order_then_finishes_once() {
        let r = FakeRemover::new(vec![(1, 10), (2, 20), (5, 150)], done());
        let msgs = run(&r, true);
        assert_eq!(
            msgs,
            vec![
                DeleteMsg::Progress { generation: 7, items: 1, bytes: 10 },
                DeleteMsg::Progress { generation: 7, items: 2, bytes: 20 },
                DeleteMsg::Progress { generation: 7, items: 5, bytes: 150 },
                DeleteMsg::Finished { generation: 7, outcome: done(), target_gone: true },
            ]
        );
        assert_eq!(r.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn run_delete_target_gone_comes_from_the_check_not_the_outcome() {
        let r = FakeRemover::new(Vec::new(), done());
        assert_eq!(run(&r, false), vec![DeleteMsg::Finished { generation: 7, outcome: done(), target_gone: false }]);
    }

    #[test]
    fn run_delete_checks_the_resolved_path_after_the_run() {
        let r = FakeRemover::new(Vec::new(), done());
        let asked = Mutex::new(Vec::new());
        let stop = AtomicBool::new(false);
        let gone = |p: &Path| {
            // The run is over when the check comes.
            assert_eq!(r.calls.lock().unwrap().len(), 1);
            asked.lock().unwrap().push(p.to_path_buf());
            true
        };
        run_delete(&r, &target(r"\\?\C:\data\big"), 1, &stop, &gone, &|_| {});
        assert_eq!(asked.into_inner().unwrap(), vec![PathBuf::from(r"\\?\C:\data\big")]);
    }

    #[test]
    fn run_delete_a_panic_in_the_remover_is_posted_as_one_skip() {
        let mut r = FakeRemover::new(vec![(1, 10)], done());
        r.panic = true;
        let msgs = run(&r, false);
        let t = target(r"\\?\C:\data\big");
        assert_eq!(
            msgs,
            vec![
                DeleteMsg::Progress { generation: 7, items: 1, bytes: 10 },
                DeleteMsg::Finished { generation: 7, outcome: failed_outcome(&t), target_gone: false },
            ]
        );
        assert_eq!(failed_outcome(&t).skipped.len(), 1);
        assert!(skip_line(&failed_outcome(&t).skipped[0]).contains("internal error"));
    }

    #[test]
    fn run_delete_passes_the_stop_flag_to_the_remover() {
        let r = FakeRemover::new(Vec::new(), Outcome { stopped: true, ..Outcome::default() });
        let stop = AtomicBool::new(true);
        run_delete(&r, &target(r"\\?\C:\data\big"), 1, &stop, &|_| false, &|_| {});
        assert_eq!(r.calls.lock().unwrap().as_slice(), [(PathBuf::from(r"\\?\C:\data\big"), true)]);
    }

    #[test]
    fn spawn_delete_runs_on_another_thread_and_posts_finished_once() {
        let r = Arc::new(FakeRemover::new(vec![(1, 1)], done()));
        let thread = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();
        let seen = thread.clone();
        // A path that is never made: the check after the run finds nothing there.
        let missing = std::env::temp_dir().join(format!("volatus-flow-{}-never-made", std::process::id()));
        let post = move |m| {
            *seen.lock().unwrap() = Some(std::thread::current().id());
            tx.send(m).unwrap();
        };
        let handle = spawn_delete(r.clone(), target(&missing.to_string_lossy()), 3, Arc::new(AtomicBool::new(false)), post).unwrap();
        handle.join().unwrap();
        let msgs: Vec<DeleteMsg> = rx.try_iter().collect();
        assert_eq!(
            msgs,
            vec![
                DeleteMsg::Progress { generation: 3, items: 1, bytes: 1 },
                DeleteMsg::Finished { generation: 3, outcome: done(), target_gone: true },
            ]
        );
        assert_ne!(*thread.lock().unwrap(), Some(std::thread::current().id()));
        assert_eq!(r.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn spawn_delete_a_panic_is_posted_not_lost() {
        let mut fake = FakeRemover::new(Vec::new(), done());
        fake.panic = true;
        let (tx, rx) = std::sync::mpsc::channel();
        let t = target(r"\\?\C:\volatus-never-made\x");
        let handle = spawn_delete(Arc::new(fake), t.clone(), 3, Arc::new(AtomicBool::new(false)), move |m| tx.send(m).unwrap()).unwrap();
        handle.join().unwrap();
        let msgs: Vec<DeleteMsg> = rx.try_iter().collect();
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(matches!(&msgs[0], DeleteMsg::Finished { generation: 3, outcome, .. } if *outcome == failed_outcome(&t)));
    }

    // ---- the second check, the gone item, the bin refusal ----

    #[test]
    fn next_refuse_from_confirm_only() {
        let refuse = DialogEvent::Refuse { text: CHANGED_TEXT.into() };
        for mode in [RecycleBin, Permanent] {
            assert_eq!(next(confirm(mode), refuse.clone()), DialogState::Refused { text: CHANGED_TEXT.into() });
        }
        for from in [DialogState::Closed, running(Permanent, false), done_state(RecycleBin)] {
            assert_eq!(next(from.clone(), refuse.clone()), from);
        }
    }

    #[test]
    fn recheck_same_path_is_ok_and_anything_else_says_why() {
        let opened = Path::new(r"c:\data\big");
        assert_eq!(recheck(opened, Ok(PathBuf::from(r"c:\data\big"))), Ok(()));
        assert_eq!(recheck(opened, Ok(PathBuf::from(r"c:\data\other"))), Err(CHANGED_TEXT.to_string()));
        assert_eq!(recheck(opened, Err(Refusal::Missing)), Err(delete::refusal_text(Refusal::Missing).to_string()));
    }

    #[test]
    fn recheck_with_the_probe_fake() {
        let e = env();
        let check = |fs: &Fake| check_target_in(Path::new(r"C:\data"), Path::new(r"C:\data\big"), true, fs, &e);
        let opened = check(&fake()).unwrap();
        // The same disk: the same path.
        assert_eq!(recheck(&opened, check(&fake())), Ok(()));
        // The folder went away after Open.
        let gone = Fake::with(&[(r"C:\data\lnk", false)]);
        assert_eq!(recheck(&opened, check(&gone)), Err(delete::refusal_text(Refusal::Missing).to_string()));
        // A file now has the folder's name.
        let file = Fake::with(&[(r"C:\data\big", false)]);
        assert_eq!(recheck(&opened, check(&file)), Err(delete::refusal_text(Refusal::KindChanged).to_string()));
        // The name now resolves to another place (a link in the parent).
        let moved = Fake::with(&[(r"C:\elsewhere\big", true)]).link(r"C:\data", r"C:\elsewhere");
        assert_eq!(recheck(&opened, check(&moved)), Err(CHANGED_TEXT.to_string()));
    }

    #[test]
    fn refusal_for_a_gone_item_removes_it_only_after_a_clean_not_found() {
        assert_eq!(refusal_for(Refusal::Missing, || true), (GONE_PENDING.to_string(), true));
        assert_eq!(refusal_for(Refusal::Missing, || false), (delete::refusal_text(Refusal::Missing).to_string(), false));
        let asked = std::cell::Cell::new(false);
        let got = refusal_for(Refusal::ProtectedFolder, || {
            asked.set(true);
            true
        });
        assert_eq!(got, (delete::refusal_text(Refusal::ProtectedFolder).to_string(), false));
        assert!(!asked.get(), "gone is asked for Missing only");
    }

    #[test]
    fn gone_texts_are_plain_and_differ() {
        for t in [GONE_PENDING, GONE_REMOVED, GONE_KEPT, CHANGED_TEXT] {
            assert!(t.ends_with('.'), "{t}");
        }
        assert!(GONE_REMOVED.contains("removed it from the list"));
        assert!(GONE_KEPT.contains("Scan again"));
    }

    #[test]
    fn list_line_by_tree_action() {
        assert_eq!(list_line(TreeAction::Detached), Some("Volatus removed the item from the list."));
        assert!(list_line(TreeAction::Detach).unwrap().contains("still shows"));
        assert!(list_line(TreeAction::KeepAndOfferRescan).unwrap().contains("Scan again"));
        assert_eq!(list_line(TreeAction::Nothing), None);
    }

    #[test]
    fn tree_action_a_bin_refusal_that_changed_nothing_is_nothing() {
        use crate::recycle::{NOT_IN_BIN, NOT_RECYCLABLE, PATH_REFUSED};
        let with = |skips: &[SkipReason], removed| Outcome {
            removed_items: removed,
            skipped: skips.iter().map(|r| skip(r"\\?\C:\data\big", *r)).collect(),
            ..Outcome::default()
        };
        let refused = with(&[SkipReason::Other(NOT_RECYCLABLE)], 0);
        assert_eq!(tree_action(&refused, RecycleBin, false), TreeAction::Nothing);
        assert_eq!(tree_action(&with(&[SkipReason::Other(PATH_REFUSED)], 0), RecycleBin, false), TreeAction::Nothing);
        // Gone after all: it leaves the tree.
        assert_eq!(tree_action(&refused, RecycleBin, true), TreeAction::Detach);
        // A skip that does not prove nothing changed keeps the rescan offer.
        assert_eq!(tree_action(&with(&[SkipReason::Other(NOT_IN_BIN)], 0), RecycleBin, false), TreeAction::KeepAndOfferRescan);
        assert_eq!(tree_action(&with(&[SkipReason::Locked], 0), RecycleBin, false), TreeAction::KeepAndOfferRescan);
        // The permanent delete has no bin refusals.
        assert_eq!(tree_action(&refused, Permanent, false), TreeAction::KeepAndOfferRescan);
    }

    #[test]
    fn open_check_gives_the_node_with_its_target() {
        let (tree, counts) = tree();
        let big = find(&tree, "big");
        let got = open_check(&tree, &counts, Some(big), &ScanState::Idle, Path::new(r"C:\data"), |_, _, _| Err(Refusal::Missing), |_| false);
        assert_eq!(got, Some((big, Err(Refusal::Missing))));
        let none = open_check(&tree, &counts, None, &ScanState::Idle, Path::new(r"C:\data"), |_, _, _| Err(Refusal::Missing), |_| false);
        assert_eq!(none, None);
    }

    #[test]
    fn delete_msg_generation() {
        assert_eq!(DeleteMsg::Progress { generation: 4, items: 0, bytes: 0 }.generation(), 4);
        assert_eq!(DeleteMsg::Finished { generation: 9, outcome: Outcome::default(), target_gone: false }.generation(), 9);
    }

    #[test]
    fn target_gone_is_true_only_for_not_found() {
        let missing = std::env::temp_dir().join(format!("volatus-flow-{}-gone", std::process::id()));
        assert!(target_gone(&missing));
        assert!(!target_gone(&std::env::temp_dir()));
    }
}

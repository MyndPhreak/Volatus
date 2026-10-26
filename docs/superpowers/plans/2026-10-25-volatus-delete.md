# Volatus delete Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the selected file or folder from the treemap or the tree: right-click **Delete** to the Recycle Bin, **Shift+Delete** permanently and fast, with a confirm and progress dialog, and keep the open scan correct afterwards.

**Architecture:** Pure policy (what may be deleted, what the dialog says, how results fold up) lives in `app/src/delete.rs` and is exhaustively unit tested. Two engines sit behind one trait: `recycle` (the shell, one operation) and `purge` (a parallel file walk with direct Win32 deletes). The scan crate learns to unlink a node and subtract its size from its ancestors. A Slint dialog drives it and the existing tree and treemap redraw from the changed `Tree`.

**Tech Stack:** Rust 2024, Slint 1.18, `windows-sys` 0.61 (more features), `rayon` (already in `scan`). No new crates unless the purge benchmark in Task 5 shows a need.

**Spec:** `docs/superpowers/specs/2026-10-25-volatus-delete-design.md`. Read it with this plan: the safety rules and the "(my call)" decisions are there.

## Summary

Volatus shows what uses a drive but cannot remove anything. This plan adds delete as a core feature in five phases: tree removal in `scan`, delete policy, the two engines, the UI, and docs. Each phase is its own PR, red commit then green commit per task.

## Global Constraints

- Plain Delete is **always** the Recycle Bin; permanent delete needs **Shift** (spec, "Safety rules"). No flag; delete is on from the first release (owner decided, recorded as a deviation from the flag rule).
- Both modes open the confirm dialog. Dialog buttons: plain Delete gives **Move to Recycle Bin** (default) and **Cancel**; Shift gives **Delete permanently** (not default) and **Cancel**; a link in the dialog switches mode.
- The item must be a descendant of the scan root. Refused as the item itself: a drive root, `Windows` (and everything inside it), `Program Files`, `Program Files (x86)`, `ProgramData`, the user profile folder. Everything else inside the last four is allowed. Case-insensitive, on resolved paths, every volume.
- Links (junctions, symlinks) are removed as links, never followed. The scan already records them as zero-size file entries.
- Locked or protected files are skipped, listed with a plain-words reason, and the run continues. Read-only files lose the read-only flag and are removed.
- Before a delete: check the path exists and is the same kind. No re-measure. Unknown target means the action is disabled; never fall through to a delete.
- Recycle Bin mode has no Stop button. The result says disk space is freed only when the bin is emptied.
- After any delete the scan is corrected in memory (node unlinked, ancestor sizes and counts reduced). With skips, skipped files stay in the tree.
- No test touches a path outside a temp directory the test made. No test waits on wall-clock time.
- UI text follows ASD-STE100.
- Commit style: `Add failing tests for <area>` then `Implement <area>`. Commits end with the Co-Authored-By line from the session.

## Review Focus

1. A folder with a path over 260 characters: it is deleted, not skipped.
2. The treemap is zoomed into the deleted folder (`view_root`): the view moves to its parent, not to a node that no longer exists.
3. The item was deleted outside Volatus after the scan: the dialog says it is gone and removes it from the tree; no error crash.
4. A delete while a scan runs (or a rescan starts mid-delete): delete is disabled; a stale delete result is dropped by scan generation.
5. A folder holding a junction to another folder: the target is untouched and uncounted.

## Files

| File | Change |
|---|---|
| `scan/src/lib.rs` | add `Tree::detach(NodeId) -> Detached` (unlink, ancestor subtraction) |
| `scan/tests/detach.rs` | new: tree removal tests |
| `app/src/delete.rs` | new: policy, request check, outcome folding, dialog text (pure) |
| `app/src/purge.rs` | new: fast permanent delete engine |
| `app/src/recycle.rs` | new: Recycle Bin engine (shell) |
| `app/src/tree_view.rs` | add `counts_after_detach`; view-root fix helper |
| `app/src/main.rs` | wire menu, keys, dialog, post-delete refresh |
| `app/ui/delete.slint` | new: the confirm, progress and result dialog |
| `app/ui/main.slint`, `app/ui/treemap.slint` | context menu on the tree rows and the treemap; new callbacks |
| `app/Cargo.toml` | extra `windows-sys` features (Win32_System_IO, Win32_Security if the build needs them) |
| `app/tests/delete_fs.rs` | new: temp-directory tests of both engines (Windows only) |
| `docs/BENCHMARK.md`, `docs/USAGE.md`, `README.md` | purge numbers, usage, README line |

## API Contracts (land as compiling stubs in each phase's red commit)

```rust
// scan/src/lib.rs
pub struct Detached { pub size: u64, pub allocated: u64, pub files: u64, pub dirs: u64 }
impl Tree { pub fn detach(&mut self, n: NodeId) -> Option<Detached>; } // None for the root

// app/src/delete.rs
pub enum DeleteMode { RecycleBin, Permanent }
pub enum Refusal { NotUnderRoot, ProtectedFolder, DriveRoot, Missing, KindChanged, ScanRunning }
pub struct DeleteTarget { pub node: NodeId, pub path: PathBuf, pub is_dir: bool, pub is_link: bool,
                          pub size: u64, pub files: u64, pub dirs: u64 }
pub fn check_target(root: &Path, target: &Path, is_dir: bool, fs: &dyn Probe) -> Result<(), Refusal>;
pub fn mode_for(shift: bool) -> DeleteMode;
pub struct Skipped { pub path: PathBuf, pub reason: SkipReason }
pub enum SkipReason { Locked, AccessDenied, NeedsAdmin, TooLong, Other(i32) }
pub struct Outcome { pub removed_items: u64, pub removed_bytes: u64, pub skipped: Vec<Skipped>, pub stopped: bool }
pub fn summary_lines(mode: DeleteMode, o: &Outcome) -> Vec<String>;
pub trait Probe { fn exists(&self, p: &Path) -> bool; fn is_dir(&self, p: &Path) -> bool; }

// app/src/purge.rs, recycle.rs
pub trait Remover: Send + Sync {
    fn remove(&self, t: &DeleteTarget, progress: &dyn Fn(u64, u64), stop: &AtomicBool) -> Outcome;
}
pub struct Purge;    // fast permanent
pub struct Recycle;  // shell, one operation
```

No HTTP or persisted shapes change. The `Tree` gains a method only; no existing signature changes.

## Test Plan

State of test-first: every task below is red/green. **Exempt, with cover:** the Slint dialog layout and the context-menu wiring (markup) are covered by numbered manual steps in Task 7; the shell call inside `Recycle` is a thin wrapper, covered by one temp-directory integration test that runs only where a Recycle Bin exists (marked `#[ignore]` if CI has none) plus the decision logic around it.

`scan/tests/detach.rs`
- detach a file: parent and every ancestor lose its `size` and `allocated`; sibling order is kept
- detach a folder: ancestors lose the folder's rolled-up size; `Detached.files`/`dirs` count the subtree
- detach the first, middle and last child: the sibling chain stays intact (three cases)
- detach the root: returns `None`, tree unchanged
- a detached node is no longer reachable from `children()` of its parent
- detach twice: second call returns `None`

`app/src/tree_view.rs` (inline)
- `counts_after_detach` lowers each ancestor's file count by the subtree's files
- `view_for_deleted(view_root, deleted)` returns the parent when the view root is the deleted node or inside it; unchanged otherwise
- selection after delete moves to next sibling, else previous, else parent

`app/src/delete.rs` (inline)
- `check_target`: item equal to the scan root → `NotUnderRoot`; sibling of the root → `NotUnderRoot`; child of the root → ok
- each protected folder as the item → `ProtectedFolder`; a child of each of `Program Files`, `Program Files (x86)`, `ProgramData`, the user profile → ok; a child of `Windows` → `ProtectedFolder`
- `C:\` and `\\?\D:\` → `DriveRoot`; case differences (`c:\WINDOWS`) refused; trailing backslash refused
- a path that no longer exists → `Missing`; a file where a folder was → `KindChanged`
- `mode_for(false)` is `RecycleBin`, `mode_for(true)` is `Permanent` (the 2-case table)
- `summary_lines` for each of: all removed, some skipped, stopped, recycle (contains the "space is freed when the bin is emptied" line), permanent (does not), `NeedsAdmin` skip (offers restart as administrator)
- the skip reasons map the Win32 codes 5, 32, 33, 206 to `AccessDenied`, `Locked`, `Locked`, `TooLong`; any other code to `Other(code)`

`app/tests/delete_fs.rs` (temp directories)
- `Purge`: removes a tree of 1000 small files and nested folders; `removed_items` and `removed_bytes` equal what was made
- `Purge`: a read-only file is removed
- `Purge`: a file held open without share-delete is skipped and listed `Locked`; the rest is removed; the folder stays
- `Purge`: a junction inside the folder is removed and the junction's target folder and its files remain
- `Purge`: a path longer than 260 characters is removed
- `Purge`: `stop` set mid-run returns `stopped: true`; what was removed stays removed
- `Purge`: the target no longer exists → `Outcome` with 0 removed and one `Missing`-style skip, no panic
- `Recycle`: a temp folder leaves its place (path no longer exists); `removed_items` is 1

## Out of Scope

The Sankey view, age colors, the "free X GB" running total, multi-select, undo for permanent delete, auto-picking what to delete, a Windows 11 shell integration, restoring the dialog after "Restart as administrator".

## Definition of Done

- [ ] `cargo test --workspace` passes, including each red test above going red then green (red commits verified by checkout)
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] Plain Delete from the context menu on a temp folder moves it to the Recycle Bin (manual step 3)
- [ ] Shift+Delete permanently removes it (manual step 4); plain click can never do it (`mode_for` test + manual step 5)
- [ ] The protected-folder list is enforced for every row in the table (tests above)
- [ ] After a delete the tree, treemap, and footer sizes drop by the item's size with no rescan (manual step 6, `detach` tests)
- [ ] `Purge` is measured against Explorer and `rd /s /q` on a 100,000-file folder and the numbers are in `docs/BENCHMARK.md` (Task 5)
- [ ] `README.md` and `docs/USAGE.md` describe delete and the Recycle Bin caveat

## Acceptance Criteria

1. Right-click **Delete** on a folder shows the dialog with its path, size, file count and "Move to Recycle Bin" as the default; confirming sends it to the Recycle Bin. (DoD 3)
2. Shift+Delete shows "Delete permanently" with a no-undo line; confirming removes it without using the Recycle Bin. (DoD 4, 5)
3. A drive root or a protected folder cannot be deleted; the dialog says why. (DoD 3 tests)
4. A file Volatus cannot delete is listed with a reason and does not stop the rest. (`delete_fs` tests)
5. After the delete the scan shows the new sizes, and the selection and zoomed view are valid. (DoD 6)
6. The result of a Recycle Bin delete says the space is not free until the bin is emptied. (`summary_lines` tests)

## Phases (one PR each, stacked)

1. **Tree removal** (Task 1)
2. **Policy** (Tasks 2 and 3)
3. **Engines** (Tasks 4 and 5)
4. **UI** (Tasks 6 and 7)
5. **Docs** (Task 8)

---

### Task 1: `Tree::detach`

**Files:** Modify `scan/src/lib.rs`; Test `scan/tests/detach.rs`.

**Interfaces:** Produces `Tree::detach(&mut self, NodeId) -> Option<Detached>` and `Detached` (see contracts).

- [ ] **Step 1: Red commit.** Add the `Detached` struct and `detach` stub returning `None`; write the six `detach.rs` tests above using a hand-built tree from `scan::rollup` entries. Run `cargo test -p scan --test detach`; expect assertion failures (not compile errors). Commit `Add failing tests for removing a node from the tree`.
- [ ] **Step 2: Implement `detach`.** Unlink the node from its parent's `first_child`/`next_sibling` chain (walk the chain; no back-pointer exists), then walk `parent` links subtracting `size` and `allocated`. Count files and dirs in the subtree with a stack. Leave the arena slots in place (no reuse). Run the tests; expect PASS. Commit `Implement removing a node from the tree`.

### Task 2: Counts, selection and view after a detach

**Files:** Modify `app/src/tree_view.rs`.

**Interfaces:** Consumes `Detached`. Produces:
`pub fn counts_after_detach(counts: &mut [u64], tree: &Tree, parent: Option<NodeId>, d: &Detached)`,
`pub fn view_for_deleted(tree: &Tree, view_root: NodeId, deleted: NodeId) -> NodeId`,
`pub fn selection_after(tree: &Tree, deleted: NodeId) -> Option<NodeId>` (must be called **before** `detach`, since it reads the sibling chain).

- [ ] **Step 1: Red commit** with stubs and the three inline test groups from the Test Plan. Expect assertion failures. Commit `Add failing tests for tree view state after a delete`.
- [ ] **Step 2: Implement.** `view_for_deleted` walks `parent` links from `view_root`; if it meets `deleted`, return `deleted`'s parent. Commit `Implement tree view state after a delete`.

### Task 3: Delete policy

**Files:** Create `app/src/delete.rs`; register `mod delete;` in `app/src/main.rs`.

**Interfaces:** Produces everything under `app/src/delete.rs` in the contracts. `Probe` is the seam: the real one calls `std::fs::symlink_metadata`; tests use a map-backed fake. The protected list is one `const`/function `protected_folders(env: &Env) -> Vec<PathBuf>` where `Env` carries `windir`, `program_files`, `program_files_x86`, `program_data`, `user_profile`, so tests pass fixed strings and never read the real environment.

- [ ] **Step 1: Red commit.** Stubs returning `Err(Refusal::ProtectedFolder)` / `Vec::new()`; write every `delete.rs` test above (the protected table and the `mode_for` 2-case table enumerated, not sampled). Commit `Add failing tests for delete policy`.
- [ ] **Step 2: Implement.** Compare paths lowercased with `\\?\` stripped and trailing separators trimmed; `Windows` refuses by prefix, the other four by equality. Use `scan::is_volume_root` for drive roots. Commit `Implement delete policy`.

### Task 4: `Purge` engine

**Files:** Create `app/src/purge.rs`; Test `app/tests/delete_fs.rs`; Modify `app/Cargo.toml` (windows-sys features).

**Interfaces:** Consumes `DeleteTarget`, `Outcome`, `Remover`. Produces `Purge: Remover`. The error-code mapping function `reason_from_win32(code: i32) -> SkipReason` lives in `delete.rs` (Task 3 test table) and is used here.

- [ ] **Step 1: Red commit.** `Purge::remove` stub returns an empty `Outcome`; write the seven `Purge` tests in `delete_fs.rs`. Expect assertion failures. Commit `Add failing tests for the fast delete`.
- [ ] **Step 2: Implement.** Start simple and correct: enumerate with `FindFirstFileExW` using `\\?\`-prefixed paths; never follow a reparse point (remove the link itself with `RemoveDirectoryW` or `DeleteFileW`); clear the read-only attribute and retry once on access denied; delete files in parallel with rayon, then folders deepest first; report progress through `progress(items, bytes)`; check `stop` between items. Record each failure as a `Skipped`. Run the tests; expect PASS. Commit `Implement the fast delete`.

### Task 5: Measure `Purge`, then tune

**Files:** Modify `app/src/purge.rs`, `docs/BENCHMARK.md`; add `app/examples/purge_bench.rs`.

- [ ] **Step 1:** Write the example: make a folder of 100,000 small files in nested folders under a temp path, then time `Purge`. Run it, then time Explorer delete and `rd /s /q` on identical folders by hand. Record the three numbers in `docs/BENCHMARK.md` in the existing table style.
- [ ] **Step 2:** Try `FILE_DISPOSITION_FLAG_POSIX_SEMANTICS` through `SetFileInformationByHandle` and the thread count; keep a change only if it is measurably faster and all Task 4 tests still pass. Record what was tried. Commit `Measure and tune the fast delete`.
- [ ] **Step 3: Gate.** If `Purge` is not clearly faster than `rd /s /q`, stop and report to the owner before continuing; the feature's premise would need rethinking.

### Task 6: `Recycle` engine

**Files:** Create `app/src/recycle.rs`; Test `app/tests/delete_fs.rs`.

**Interfaces:** Produces `Recycle: Remover`.

- [ ] **Step 1: Red commit** with a stub and the `Recycle` test (folder leaves its place; `removed_items == 1`). Commit `Add failing test for the Recycle Bin delete`.
- [ ] **Step 2: Implement** with `SHFileOperationW`, `FO_DELETE | FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI`, path double-NUL terminated. A non-zero result or an aborted flag becomes one `Skipped`; "too big for the bin" is `SkipReason::Other` with a message that offers the permanent delete (no automatic fallback). Commit `Implement the Recycle Bin delete`.

### Task 7: Dialog, menu and wiring (UI)

**Files:** Create `app/ui/delete.slint`; Modify `app/ui/main.slint`, `app/ui/treemap.slint`, `app/src/main.rs`.

**Interfaces:** Consumes everything above. New callbacks on `MainWindow`: `request-delete(bool /*shift*/)`, `confirm-delete(bool /*permanent*/)`, `stop-delete()`, `close-delete()`. New properties for the dialog (state: closed / confirm / running / done, path, size text, counts, progress, result lines).

- [ ] **Step 1: Context menu.** Add a right-click menu with **Delete** on tree rows and treemap tiles (Slint `ContextMenuArea` if 1.18 has it, else a `PopupWindow`; check with the Slint docs tool). Right-click selects the item under the pointer first. `Delete` key on a selected row calls `request-delete(false)`; `Shift+Delete` calls `request-delete(true)`. The menu's Delete reads the Shift state at the click.
- [ ] **Step 2: `request_delete` in `main.rs`.** Build a `DeleteTarget` from the selected node; call `check_target` with the real `Probe`; on `Err` show the refusal in the dialog's text and no action button; on `Ok` open the confirm state. Disabled while `ScanControl` is scanning.
- [ ] **Step 3: Run on a worker thread.** On confirm, spawn a thread that calls the chosen `Remover`, posting progress and the `Outcome` back with `invoke_from_event_loop`, tagged with the scan generation so a result for an old scan is dropped. A `stop` `AtomicBool` serves Stop (hidden in Recycle mode).
- [ ] **Step 4: Apply the result on the UI thread.** If the item is gone from disk (all or partly removed with no skips): `selection_after`, then `Tree::detach`, `counts_after_detach`, `view_for_deleted`, rebuild rows and rects, refresh the footer and the drive free space (`drives`). With skips, detach only if the folder itself is gone; otherwise leave the tree and offer **Rescan** in the dialog. Resolve the `Arc<Data>` sharing (the `RowModel` and layout threads hold clones): bump `layout_seq`, rebuild the `RowModel` from a fresh `Arc`, and use `Arc::get_mut`/`Arc::try_unwrap` once no layout is in flight, retrying on the next event-loop turn. Record the chosen approach in the PR.
- [ ] **Step 5: Manual verification (UI is exempt from test-first; these steps cover it).**
  1. Scan a temp folder with a few subfolders; right-click a subfolder; the menu shows Delete.
  2. Choose Delete: the dialog shows path, size, file count, "Move to Recycle Bin" as default.
  3. Confirm: the folder is in the Recycle Bin; the tree, treemap and footer drop by its size.
  4. Repeat with Shift held: the dialog says "Delete permanently" and "no undo"; confirm; the folder is gone and not in the bin.
  5. Click Delete without Shift ten times on different folders: no permanent delete ever happens.
  6. Zoom the treemap into a folder, delete that folder: the view moves to its parent.
  7. Try to delete the scan root and `C:\Windows`: refused with a reason.
  8. Lock a file in a folder (open it in a program that blocks delete), permanent delete the folder: the result lists the file as locked; the rest is removed.
  9. Keyboard only: Delete key, Tab to buttons, Enter, Esc.
  Commit `Add the delete dialog and menu`.

### Task 8: Docs

**Files:** Modify `README.md`, `docs/USAGE.md`.

- [ ] **Step 1:** Add a short section: how to delete, the Shift rule, the Recycle Bin caveat ("disk space is freed only when the Recycle Bin is emptied"), the protected folders, and that links are removed without following. ASD-STE100 style. Commit `Document delete`.

---

## Self-review

- **Spec coverage:** context menu and Shift (Task 7), fast permanent (4, 5), Recycle Bin (6), dialog content and buttons (7), protected list and recheck and links (3, 4), skips and read-only (4), scan correction (1, 2, 7), no flag (Global Constraints), Recycle caveat (3 `summary_lines`, 8), elevation hint (3 `NeedsAdmin`). Gaps: none found.
- **Risk to watch:** Task 7 Step 4, because `Data` is shared through `Arc` by the row model and layout threads. Task 5 has a stop gate if the fast delete is not faster.
- **Names checked** across tasks: `Detached`, `DeleteTarget`, `Outcome`, `Remover`, `check_target`, `mode_for`, `reason_from_win32`.

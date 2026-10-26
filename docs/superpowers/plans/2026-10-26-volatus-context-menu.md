plan-version: 1

# Volatus context menu: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Right-click (or the Menu key / Shift+F10) on a treemap tile or frame, a tree row or a crumb opens Volatus items plus the real Windows shell menu for that path. A shell Delete removes the item from the map at once; other changes mark the map outdated with a Rescan button.
**Architecture:** Pure decisions in a new `app/src/menu.rs` (gate, disk check, items, ids, flags, verb, post-action judge); COM and Win32 in a new `app/src/shell.rs` (exempt, manual checks); removal as an in-place unlink in `scan::Tree` behind `Arc::make_mut` (no overlay); Slint passes right-button and menu-key events with window-logical points.
**Tech Stack:** Rust 2024, Slint 1.18.1 (winit 0.30.13, `Cargo.lock:5592`), `windows` 0.62.2 (`Cargo.lock:5316`, already compiled through Slint/wgpu), `arboard` 3.7.0 (`Cargo.lock:232`), existing `windows-sys` 0.61.
**Spec:** `docs/superpowers/specs/2026-10-26-volatus-context-menu-design.md` (approved; deviations D1-D2 owner-approved, D3-D5 below are the planner's).

## Global Constraints
- Work on a new branch from `main` (after PR #1). Agents never push, open PRs, tag or release: **OWNER GATE** for each.
- No runtime flag (approved deviation D2). Parity: every existing test passes unchanged except the three `RowModel { data: .. }` fixture sites (`main.rs:822,1685,2054`), which change only the field type.
- `windows` stays at 0.62.2: `cargo tree -p app -i windows -e normal` must succeed (it errors when two versions exist) and `git diff Cargo.lock` adds no `[[package]]`. Same for `arboard` (`default-features = false`).
- Paths for the shell come only from `Tree::path` (`scan/src/lib.rs:61`; the root's name is the scan path as typed, `walk.rs:222`, `mft.rs:596`). `display_name` is never used for a path.
- No `SESSION` borrow may be held across `TrackPopupMenuEx` or `InvokeCommand` (both pump messages).
- Baseline: run `cargo test --workspace` before P2 and record the counts. Every phase ends with 0 failed and `cargo clippy --workspace --all-targets -- -D warnings` clean.
- UI text ASD-STE100. Exact strings: "Explore Folder", "Command Prompt Here", "Copy Path", "Scan this folder" (OD1), "This item changed on disk. Rescan." (disabled), "Rescan", "Windows has no menu for this item." (disabled), notice "Files changed after this scan.", root gone "The scanned folder was deleted.", notes "Volatus cannot copy the path." / "Volatus cannot open the folder." / "Volatus cannot open a command prompt."

## Review Focus (inputs the tests could miss)
1. **Odd paths**: spaces, unicode, `;` `&` `%` `,` in names, a drive root, > 260 characters, a name made lossy at scan time (`walk.rs:177`, `mft.rs:159`). Pinned by P3.T2 temp-dir tests; manual M11, M16.
2. **Disk changed after the scan**: item deleted elsewhere, file replaced by a folder. Pinned by P3.T2 `prepare_*`; manual M8.
3. **Deleting the view root, an ancestor of it, or the scan root**. Pinned by P2.T3 `after_removal_*` and P2.T4; manual M12.
4. **Right-click during a zoom or before a relayout lands** (stale `rects`). Pinned by P3.T1 `menu_target_during_a_zoom_out_is_the_left_click_target`, P2.T3 `remove_node_drops_the_rects_of_the_subtree`; manual M13.
5. **A worker holding `Arc<Data>` at removal time** (layout or type count): the copy path, and late chips. Pinned by P2.T2/P2.T3; manual M5 timing.
6. **Delete cancelled, Shift held, slow Recycle Bin**: P3.T4 judge table; manual M6, M7; OD3.
7. **Shell extension hangs or crashes; window closed while the menu or a shell dialog is open**: exempt (in-process shell, as Explorer); manual M9, M10, M14.

---

## Summary
Volatus shows no context menu today. This work adds: (P1) a throwaway spike that settles the Win32/Slint unknowns; (P2) removal of a node from a finished scan without a rescan; (P3) the pure menu decisions; (P4) the shell menu, its wiring to every entry point, the post-action checks, the outdated notice and Rescan, and the Volatus actions; (P5) docs, accessible text and recorded deviations.

## Architectural Decisions
| Decision | Reason |
|---|---|
| **D3 (deviation from the design's "overlay" call, left to the builder):** removal is `Tree::remove` (unlink from the parent's child list, subtract `size`/`allocated` up the ancestors, set the removed node's `parent = None`) on `Arc::make_mut(&mut loaded.data)`. | Every reader (`flatten`, `layout_framed` `treemap/src/lib.rs:106-127`, labels, hover, tooltip, `map_value`, `row_data` `main.rs:94-111`) reads `tree.node(n).size` and `tree.children(n)` directly (32 sites). An overlay must be threaded through all of them and the treemap crate; one missed reader shows a stale size. In place, all readers are right by construction. `make_mut` never mutates shared data: it clones when a worker holds an `Arc` (rare: a layout or the type count running at that moment), so the design's constraint holds. Nested removals need no special case. |
| `RowModel.data` becomes `RefCell<Weak<Data>>` (re-pointed after a removal). | A strong `Arc` in the model would make every `make_mut` a full copy (about 0.3 s and 2x memory on 5 M nodes). With only `Weak`s, `make_mut` moves the `Data` header, not the node buffer. |
| `Loaded` keeps `totals: Option<Vec<TypeTotal>>` and an `edits: u64` counter; chips are derived from totals on the UI thread (`footer::chips`). Removal subtracts `type_totals(tree, n)` (computed before the unlink) from `totals`; a count still running when a removal happens is dropped by `edits` and started again. | Spec: subtract, no recount. `store_chips`' `Arc::ptr_eq` check (`main.rs:870`) is wrong after an in-place change; `edits` replaces it. |
| After a removal `l.rects` drops every rect whose node is not attached (`Tree::is_attached`, O(depth) each), then `relayout`. | Hit test, outline and arrow keys stay right in the ~0.1-0.3 s before the new layout lands. ponytail: O(rects x depth); fine for <= 10^5 rects. |
| One menu entry: `open_menu(ui, node, at, keyboard)`; the gate `menu::allowed` (finished scan, data loaded) and the disk check run there only. | Six invocation paths (below) cannot drift. |
| Right-click on a tile or a row selects it first (`select`); on a crumb it does not. | `select` of an ancestor moves the map (`view_for_selection`, `main.rs:801`); Explorer does not navigate on a breadcrumb right-click. |
| Pre-menu check `fs::symlink_metadata` + `FILE_ATTRIBUTE_DIRECTORY` vs `node.is_dir`; any error or mismatch = `Changed` (fail closed). | Rust std adds `\\?\` for long paths; a junction keeps the directory bit; a lossy name gives NotFound. |
| Delete = `GetCommandString(GCS_VERBW)` equal to `delete` (ASCII case-insensitive), read before `InvokeCommand`. Verbs from a failed call are `None` = not delete. | Spec call; third-party "delete" items can never remove a node. |
| Post-action checks at 0 s, 0.5 s, 2 s (one `Follow` in `Session`, one `Timer`); compare disk state before/after. | Spec call. A new menu, a rescan, "New scan" and exit drop it. |
| `CMF_NORMAL`, plus `CMF_EXTENDEDVERBS` when Shift is down at open; never `CMF_CANRENAME`. Invoke mask `CMIC_MASK_UNICODE` + `CMIC_MASK_PTINVOKE`, plus `SHIFT_DOWN`/`CONTROL_DOWN` from `GetKeyState` at invoke. | Spec call; Shift+Delete is permanent as in Explorer. |
| Volatus ids 1-5; shell ids `0x100..=0x7FFF`. | No overlap; `picked(id)` is total. |
| **D4:** Command Prompt Here = `ShellExecuteExW("open", "cmd.exe", lpDirectory = folder)` (no arguments). Not `wt.exe -d`. (OD2) | No path in a command line: nothing to quote or inject (`&`, `%`, `;` in names). Windows 11's "default terminal" setting picks Windows Terminal or the console host. Same API as `elevate.rs:68`. |
| **D5:** Explore Folder = `SHOpenFolderAndSelectItems(pidl, 0, ..)` for a file (opens the parent, selects it); `ShellExecuteExW("explore", folder)` for a folder. Not `explorer.exe /select,"..."`. | Explorer's own parser splits on `,` and mishandles `"C:\"`; the API takes a PIDL. |
| Copy Path = `arboard::Clipboard::set_text(path)` (raw path). | Installed dependency (rung 5); Slint 1.18 has no public clipboard setter. |
| Menu owner/subclass, Slint key names, deferral of the modal call: chosen by the spike (P1) and written into `shell.rs`' module doc with the log reference. | Cannot be settled from code. |
| D1 (approved): `windows = { version = "0.62.2", features = [Win32_Foundation, Win32_System_Com, Win32_UI_Shell, Win32_UI_Shell_Common, Win32_UI_WindowsAndMessaging, Win32_UI_Input_KeyboardAndMouse, Win32_Graphics_Gdi] }` under `[target.'cfg(windows)'.dependencies]`. | COM interfaces (`IContextMenu*`, `IShellFolder`) exist only in `windows`, not `windows-sys`. Builder checks each item against docs.rs `windows` 0.62.2 (`SetWindowSubclass` is in `Win32_UI_Shell`). |

## Invocation paths (gate + disk check in `open_menu`)
1 map right-button up (`treemap.slint:491` pointer-event) · 2 tree row right-button (`main.slint:390`) · 3 crumb right-button (`treemap.slint:139`) · 4 Menu key / Shift+F10 in `map-focus` (`treemap.slint:478`) · 5 the same in `tree-focus` (`main.slint:365`) · 6 the same in a `CrumbLink` (`treemap.slint:128`). If the spike shows keys arrive only as `WM_CONTEXTMENU` (lParam -1), paths 4-6 become one subclass path that targets the selected node. Tested by P3.T1 (each path + equivalence) and M2, M3.

## Failure behaviour
| Dependency | Not available / failed | Direction |
|---|---|---|
| Disk check before the menu | NotFound, kind mismatch, any error | fail closed: only "This item changed on disk. Rescan." + "Rescan" |
| COM (`CoInitializeEx` STA) | `RPC_E_CHANGED_MODE` | degrade: Volatus items + "Windows has no menu for this item." |
| `SHParseDisplayName` / `SHBindToParent` / `GetUIObjectOf` / `QueryContextMenu` | error HRESULT (e.g. long path) | degrade: same line; never a menu for another path |
| Window subclass / menu messages | install fails | degrade: `IContextMenu` only (owner-drawn submenus may be empty); `eprintln!` once |
| `GetCommandString` | error / empty | verb `None`: cannot remove, only "outdated" |
| `InvokeCommand` | error HRESULT | checks still run; the disk decides |
| Post-action disk read | unreadable | `Outdated`, never `Remove` |
| Clipboard / ShellExecuteEx / SHOpenFolderAndSelectItems | error | one note on the `elevate-note` line (`main.slint:311`, cleared by `start_scan` `main.rs:969`) |
| Shell extension hangs / crashes | UI thread blocks / process ends | accepted (Goal 6, as Explorer); M14 measures |

## Framework identifiers (capture in P1 before trusting P3/P4 tests)
S1 Slint right button: `PointerEventButton.right` with `PointerEventKind.up`, and whether `TouchArea.clicked` also fires for it. S2 `event.text` for the Menu key and F10 (+`event.modifiers.shift`) on winit 0.30.13 (log the code points); does `WM_CONTEXTMENU` reach a subclass. S3 menu messages (`WM_INITMENUPOPUP`, `WM_DRAWITEM`, `WM_MEASUREITEM`, `WM_MENUCHAR`) seen by `SetWindowSubclass` on Slint's HWND during `TrackPopupMenuEx`, without a comctl32 v6 manifest (`app/app.manifest` has none); fallback: a hidden owned helper window as menu owner. S4 `CoInitializeEx` result on the UI thread (winit may have called `OleInitialize`). S5 Slint window-logical origin vs `ClientToScreen` at scale 1.0 and 1.5. S6 `GCS_VERBW` strings for every item of a file and a folder menu, with and without Shift (fixture for P3 tests, copied verbatim). S7 `InvokeCommand("delete")`: returns before or after the item vanishes (small file, 1 GB folder, Recycle Bin and Shift), and its HRESULT on a cancelled confirmation. S8 whether Slint timers / `invoke_from_event_loop` closures run inside the modal loop (re-entrancy), and whether a direct call from the Slint callback or a zero `Timer` is needed. S9 time to `QueryContextMenu` + first show, cold and warm, file and folder, with the owner's extensions. S10 `SHParseDisplayName` on a 300-character path.
Docs (fetch in the spike): learn.microsoft.com `/windows/win32/api/shobjidl_core/nn-shobjidl_core-icontextmenu` (and `-querycontextmenu`, `-invokecommand`, `-getcommandstring`, `nn-shobjidl_core-icontextmenu3`), `/api/shlobj_core/nf-shlobj_core-shparsedisplayname`, `nf-shlobj_core-shbindtoparent`, `nf-shlobj_core-shopenfolderandselectitems`, `/api/shobjidl_core/nf-shobjidl_core-ishellfolder-getuiobjectof`, `/api/winuser/nf-winuser-trackpopupmenuex`, `/api/commctrl/nf-commctrl-setwindowsubclass`.

## Files
| File | Change |
|---|---|
| `scan/src/lib.rs` | `#[derive(Clone)]` on `Tree`; `Tree::remove`, `Tree::is_attached`, `Removed`; tests. |
| `app/Cargo.toml` | `windows` 0.62.2 (D1 features), `arboard = { version = "3.7", default-features = false }`. |
| `app/src/menu.rs` (new) | Pure decisions (contracts below); tests. |
| `app/src/shell.rs` (new) | COM, `IContextMenu`/2/3, popup, subclass or helper window, invoke, explore, terminal. Exempt. |
| `app/src/footer.rs` | `subtract`; tests. |
| `app/src/tree_view.rs` | `after_removal`; tests. |
| `app/src/main.rs` | `Data: Clone`; `RowModel` weak data; `Loaded.totals/edits`; `Session.outdated/follow`; `remove_node`, `removed`, `rescan`, `open_menu`, `menu_target`, `run_action`; callbacks; tests. |
| `app/ui/treemap.slint`, `app/ui/main.slint` | Right-button and menu-key callbacks with window points; `outdated` + `rescan` in the top bar; accessible descriptions. |
| `docs/USAGE.md`, `PLAN.md` | Keys, menu section; deviations D1-D5 under a new "Context menu" decisions table. |
| `.claude/work/storagetool-wiztree-clone/probes/cm-spike/`, `logs/CM-spike.md` | P1 only (git-excluded, `.git/info/exclude:7`). |

## API Stubs & Contracts (land as compiling stubs in the phase's first red commit)
```rust
// scan
pub struct Removed { pub parent: NodeId, pub size: u64, pub allocated: u64 }
impl Tree { pub fn remove(&mut self, n: NodeId) -> Option<Removed>; /* None: root or detached */
            pub fn is_attached(&self, n: NodeId) -> bool; }
// footer
pub fn subtract(totals: &[TypeTotal], gone: &[TypeTotal]) -> Vec<TypeTotal>; // order: bytes desc, ext asc
// tree_view
pub struct AfterRemoval { pub select: NodeId, pub view: Option<NodeId> }
pub fn after_removal(tree: &Tree, view_root: NodeId, r: &Removed) -> AfterRemoval; // call after remove
// menu
pub enum Disk { Gone, Dir, File(u64), Unreadable }
pub fn disk_of(m: std::io::Result<std::fs::Metadata>) -> Disk;  pub fn read_disk(p: &Path) -> Disk;
pub enum Prepared { Ready { path: PathBuf, before: Disk }, Changed }
pub fn prepare(tree: &Tree, n: NodeId, disk: impl Fn(&Path) -> Disk) -> Prepared;
pub fn allowed(state: &ScanState, loaded: bool) -> bool;
#[repr(u32)] pub enum Cmd { ExploreFolder = 1, CommandPrompt, CopyPath, ShowFolder, Rescan }
pub enum Item { Action(Cmd, &'static str), Disabled(&'static str), Separator }
pub fn items(p: &Prepared, is_dir: bool, shell_ok: bool) -> Vec<Item>;
pub const SHELL_FIRST: u32 = 0x100; pub const SHELL_LAST: u32 = 0x7FFF;
pub enum Picked { Nothing, Volatus(Cmd), Shell(u32 /* offset */) }
pub fn picked(id: u32) -> Picked;
pub fn query_flags(shift: bool) -> u32;  pub fn invoke_mask(shift: bool, ctrl: bool) -> u32;
pub fn is_delete(verb: Option<&str>) -> bool;
pub enum Outcome { Wait, Remove, Outdated, Nothing }
pub fn judge(delete: bool, before: &Disk, now: &Disk, last: bool) -> Outcome;
pub struct Follow { pub node: NodeId, pub edits: u64, pub path: PathBuf, pub delete: bool, pub before: Disk, stage: u8 }
impl Follow { pub fn new(..) -> Self; pub fn step(&mut self, now: &Disk) -> (Outcome, Option<Duration>); } // next delay
pub fn terminal_folder(path: &Path, is_dir: bool) -> PathBuf;
pub fn screen_point(window: (f32, f32), scale: f32, origin: (i32, i32)) -> (i32, i32);
// main.rs
enum Source { Map { x: f32, y: f32 }, Row(i32), Crumb(i32), Key }
fn target_of(l: &Loaded, src: Source, hit: Option<NodeId>) -> Option<(NodeId, bool /* select first */)>;
fn remove_node(l: &mut Loaded, n: NodeId) -> Option<AfterRemoval>; // None for the root
fn run_action(cmd: Cmd, path: &Path, is_dir: bool, act: impl FnOnce(Cmd, &Path) -> Result<(), String>) -> Option<String>; // note
```
Slint: `callback treemap-menu(float, float, float, float)` (map x, y; window x, y), `tree-menu(int, float, float)`, `crumb-menu(int, float, float)`, `map-menu-key(float, float)`, `tree-menu-key(float, float)` (window point of the selected box/row, computed in Slint from `absolute-position`), `in property <bool> outdated`, `callback rescan()`.

## Test Plan
```
scan/src/lib.rs (tests)
  - remove of the first / a middle / the last child unlinks it, keeps the others in order (3 cases)
  - remove subtracts size and allocated from every ancestor, root included
  - remove of a folder detaches every node under it (is_attached false), siblings stay attached
  - remove of the root or of a detached node -> None, tree unchanged
  - remove child then its parent -> ancestors lose parent's remaining size only (no double count)
  - path of a surviving node is unchanged after a removal
footer.rs
  - subtract lowers bytes and files of each extension; an extension at 0 files is dropped
  - subtract result keeps bytes desc then ext asc (a change that reorders two extensions)
tree_view.rs
  - after_removal: removed inside the view -> select parent, view None
  - after_removal: removed == view root -> view Some(parent); removed ancestor of view root -> Some(parent)
  - after_removal: removed outside the view (other branch) -> select parent, view None
main.rs
  - remove_node reduces the file counts of every ancestor
  - remove_node rows skip the node and show the reduced size and percent of each ancestor
  - remove_node drops the rects of the subtree; hit_test on its old spot is the parent frame
  - remove_node with no other holder keeps the node buffer (same &Node address before/after)
  - remove_node while a worker holds the Arc copies; the holder's tree is unchanged
  - remove_node of the root -> None; removed() shows the start screen and "The scanned folder was deleted."
  - removed() selects the parent; the map shows the parent when the view root was removed
  - chips after a removal = chips of (totals - subtree totals); counts nothing again
  - chips counted before a removal and landing after it are dropped; a new count starts
  - menu_target: map point on a file tile -> that file, select; on a frame header -> that folder, select
  - menu_target: row i -> its node, select; crumb id -> that folder, no select; gap crumb -1 / bad row -> None
  - menu_target: key -> the selected node; nothing selected -> None
  - menu_target: the same item by map, row, crumb and key -> the same node (equivalence)
  - menu_target during a zoom out is the left-click target (picture_point, main.rs:458)
  - outdated: shown with Rescan only when Done and outdated; rescan / new scan clear outdated and follow
  - rescan starts the same root (relaunch_path, main.rs:1475) with the same flags
  - run_action: a failing action gives its note text; a working one gives None (3 commands)
menu.rs
  - allowed: true only for Done with data; false for Idle, Scanning, Error, Cancelled, Done without data (every ScanState variant)
  - prepare: path = scan root + raw names (spoofed() fixture, main.rs:1671), never display_name
  - prepare: disk Gone / Unreadable / Dir for a file / File for a folder -> Changed (4); match -> Ready
  - prepare (temp dir): spaces + unicode + `; & % ,` name -> Ready; 300-char path -> Ready
  - prepare (temp dir): name with an unpaired surrogate, scanned by walk -> Changed
  - prepare (temp dir): file replaced by a folder after the scan -> Changed
  - items: file -> Explore Folder, Command Prompt Here, Copy Path, Separator; folder adds Scan this folder
  - items: Changed -> only the disabled changed line and Rescan; shell failed -> Volatus items + disabled line
  - picked: 0, 1..=5, 6, 0xFF, 0x100, 0x7FFF, 0x8000 -> Nothing, Volatus, Nothing, Nothing, Shell(0), Shell(0x7EFF), Nothing
  - query_flags: no shift -> CMF_NORMAL; shift -> CMF_EXTENDEDVERBS; CMF_CANRENAME unset in both
  - invoke_mask: the 4 shift x ctrl combinations
  - is_delete: "delete", "Delete" true; "delete ", "deletex", "Löschen", "", None false; S6 fixture verbs
  - judge: delete x now {Gone, same, different, Unreadable} x last {no, yes} (16 cases), e.g.
      delete+Gone -> Remove; delete+same+last -> Nothing (cancelled; OD3); other+Gone -> Outdated;
      any+Unreadable -> Outdated; same+not last -> Wait
  - Follow.step: delays 0.5 s then 1.5 s; after the third check no delay; a decision stops it
  - terminal_folder: file -> parent; folder -> itself; file in C:\ -> C:\; C:\ -> C:\
  - screen_point: scale 1, 1.5, 2 with an origin offset; rounds half away from zero
shell.rs (integration, real shell, STA test thread)
  - the menu of a temp file has the verbs delete and properties and no rename
  - the menu of a temp folder has delete; with shift the flags add extended verbs (count >= without)
```
**Exempt from test-first, covered instead:** `TrackPopupMenuEx`, owner-drawn submenus, `InvokeCommand` effects, Explorer/terminal launch, clipboard, Slint markup and key wiring: manual M1-M16 below; the decisions they use are the pure tests above. The spike (P1) has no tests (throwaway).

**Manual verification (owner's machine, release build, screenshots to `.claude/work/storagetool-wiztree-clone/proofs/CM/`):**
M1 right-click a file tile: menu at the pointer; Volatus items, separator, shell items; Open with and Send to submenus filled with icons. M2 the same on a folder frame header, a tree row, a crumb (the scan root, a drive root); the crumb does not navigate. M3 Menu key and Shift+F10 in the tree, the map and on a crumb: menu at the item. M4 Shift+right-click adds extended verbs; no Rename in either menu. M5 Delete a file and a folder on a C: scan with `--timing`: gone from tree and map in <= 0.3 s (timing line), every parent smaller, footer changed, parent selected. M6 Shift+Delete asks for a permanent delete; the item is removed. M7 Delete, then cancel the confirmation: nothing changes, no notice. M8 delete an item in Explorer, then right-click it in Volatus: only the changed line + Rescan; Rescan works. M9 Properties opens; close Volatus while it is open: no crash dialog. M10 Alt+F4 with the menu open closes the menu only; the window cannot close during the Delete confirmation. M11 Explore Folder (file selected; folder opened), Command Prompt Here (prompt in the folder; names with spaces, unicode, `&`, `%`, `;`), Copy Path (paste in Notepad: the raw path). M12 Delete the view root through a crumb: map shows the parent; delete the scan root of a temp-folder scan: start screen with "The scanned folder was deleted.". M13 right-click during a zoom: the menu is for the tile under the pointer; no half zoom after it closes. M14 time to first menu, cold and warm, file and folder (compare S9); a slow extension only delays the menu. M15 Narrator reads the new descriptions; Tab reaches Rescan. M16 unicode folder and a 300-character path: the menu works or shows "Windows has no menu for this item.", never another item's menu.

## Out of Scope
Several selected items; Export to CSV; Copy file and size info; Volatus-made file operations; map updates for rename, move, paste or content changes inside a folder that still exists (only "outdated" when the clicked path vanishes or changes); the banner's scan summary (stays "as scanned"); undo / Recycle Bin restore; protection of system folders; menus on the start screen or drive buttons; out-of-process shell hosting (crash isolation); hard links: deleting one name removes the whole counted size (stated limit, no notice).

## Definition of Done
- [ ] Right-click / Menu key / Shift+F10 open one menu on all six paths, only for a finished scan (menu_target + allowed tests; M1-M3)
- [ ] A changed or missing item never reaches the shell (prepare tests; M8, M16)
- [ ] Shell menu has no Rename; Shift adds extended verbs (query_flags, shell.rs tests; M4)
- [ ] Only the canonical `delete` verb can remove a node (is_delete, judge tests; M5)
- [ ] After a Delete: rows, sizes, percents, counts, footer and rects updated, parent selected (remove_node, subtract, after_removal tests; M5, M6)
- [ ] Deleting the view root / scan root moves the view / shows the start screen (after_removal, removed() tests; M12)
- [ ] Cancelled Delete changes nothing (judge; M7); other vanishing changes show "Files changed after this scan." + Rescan (judge, outdated tests)
- [ ] Rescan and New scan clear outdated and the pending check (outdated tests)
- [ ] Volatus items work; failures give one note (run_action, terminal_folder tests; M11)
- [ ] `windows` 0.62.2 and `arboard` add no package to `Cargo.lock` (`cargo tree -p app -i windows -e normal`; lock diff)
- [ ] USAGE.md, accessible descriptions and PLAN.md deviations D1-D5 updated (M15; P5 diff)
- [ ] `cargo test --workspace` 0 failed; clippy `-D warnings` clean

## Acceptance Criteria
1. The system shows Volatus items and the item's shell menu for a right-clicked or key-selected item of a finished scan. (DoD 1, 3)
2. The system never gives the shell a path that fails the disk check. (DoD 2)
3. The system removes a deleted item and corrects every total within about 0.3 s, with no rescan. (DoD 4-6)
4. The system marks the map outdated, with Rescan, when another action makes the path vanish or change. (DoD 7-8)
5. The system's own items open Explorer, a command prompt or copy the path, and say so when they fail. (DoD 9)

## Phases
Each phase: one stacked PR from the feature branch (OWNER GATE for push/PR). Verify per phase: `cargo test --workspace` -> 0 failed, count = baseline + new tests; `cargo clippy --workspace --all-targets -- -D warnings` -> no output errors.

### P1 Spike (throwaway; opus/high)
In `probes/cm-spike/` (own `[workspace]`, repo `Cargo.lock` copied, as `logs/P8-spike.md:4`): a Slint 1.18.1 window with a TouchArea and FocusScope that opens the shell menu of a real file and folder. Answer S1-S10 in `logs/CM-spike.md` with the exact identifiers, log lines, screenshots and the chosen fallback for each. Gate: P4 starts only when S1-S8 have an answer; any "does not work" goes back to the planner (plan-version bump), not to the builder.

### P2 Removal without rescan (opus/high)
| Task | Behaviour | Files | Test Plan lines | Risk |
|---|---|---|---|---|
| P2.T1 | `Tree::remove`, `is_attached`, `Clone` | `scan/src/lib.rs` | scan (6) | sibling link corruption |
| P2.T2 | totals in `Loaded`, `subtract`, `edits` drops late chips, recount after removal while counting | `footer.rs`, `main.rs` | footer (2), main chips (2) | none |
| P2.T3 | `remove_node`: `make_mut`, weak `RowModel`, counts, rect filter, `after_removal` | `main.rs`, `tree_view.rs` | tree_view (3), main remove_node (5) | a missed strong holder makes every removal a copy |
| P2.T4 | `removed(ui, n)`: select parent, move view, `relayout`, `sync_types`, drop hover/`out_from`, root gone -> start screen + message; `--timing` prints the removal time | `main.rs`, `model.rs` if needed | main removed() (2) | none |

### P3 Menu decisions (sonnet/medium)
| Task | Behaviour | Files | Test Plan lines | Risk |
|---|---|---|---|---|
| P3.T1 | gate and targets on all paths, select-first rule | `menu.rs`, `main.rs` | allowed, menu_target (5) | none |
| P3.T2 | path + disk check, fail closed | `menu.rs` | prepare (5) | std long-path behaviour |
| P3.T3 | items, ids, flags, mask, verb, terminal folder, screen point | `menu.rs` | items .. screen_point (8) | flag constants from `windows` |
| P3.T4 | `judge` + `Follow` | `menu.rs` | judge, Follow.step | OD3 |

### P4 Shell and wiring (opus/high)
| Task | Behaviour | Files | Test Plan lines | Risk |
|---|---|---|---|---|
| P4.T1 | deps; COM; build + show the menu from a map right-click per S1/S3/S5/S8; hide hover and tooltip first | `Cargo.toml`, `shell.rs`, `main.rs`, `treemap.slint`, `main.slint` | shell.rs (2); M1, M4, M14 | re-entrancy (S8) |
| P4.T2 | rows, crumbs and key paths per S2 | `main.slint`, `treemap.slint`, `main.rs` | menu_target equivalence; M2, M3, M13 | double open (Slint key + WM_CONTEXTMENU) |
| P4.T3 | verb read, invoke, `Follow` timer, removal or outdated, notice + Rescan in the top bar | `main.rs`, `shell.rs`, `main.slint` | outdated, rescan (2); M5-M10, M12 | OD3 |
| P4.T4 | Volatus actions and notes | `shell.rs`, `main.rs` | run_action; M11, M16 | none |

### P5 Docs and accessibility (sonnet/medium)
USAGE.md: keys table rows (Menu key, Shift+F10 in tree, treemap, crumbs) and a "Right-click menu" section; accessible descriptions of the tree, treemap and crumbs name the menu key; PLAN.md "Context menu" decisions table with D1-D5. Covered by M15 and the diff.

## Open Decisions (owner)
- **OD1 "Scan this folder"**: the spec makes it "the same as a double-click", which opens the folder in the map (`open_node`, `main.rs:1102`); it does not scan. Options: (a) keep the label, open in map; (b) label "Show this folder", open in map; (c) start a new scan of that folder. Recommend (b): the text says what happens. Cost if wrong: one string.
- **OD2 Terminal**: (a) `cmd.exe` via ShellExecuteEx, Windows' default terminal decides (D4); (b) `wt.exe -d` first as in the spec. Recommend (a): no command-line quoting, respects the user's setting. Cost if wrong: about 10 lines in `shell.rs`.
- **OD3 Slow Delete**: if S7 shows `InvokeCommand("delete")` returns before a long delete ends, the item can vanish after the 2 s check and stay shown with no notice. Options: (a) delete + path still present at the last check -> outdated notice, only when the HRESULT was not "cancelled" (if S7 can tell them apart); (b) spec rule: nothing. Recommend (a) when S7 tells cancel apart, else (b) plus M7. Cost if wrong: a shown deleted item (b) or a needless notice (a).

## Self-review
- D3 replaces the design's overlay "my call" with an in-place unlink behind `Arc::make_mut`; the owner has not seen this choice. It removes the "every reader must consult the overlay" risk; its own risk (a strong `Arc` holder) is pinned by the node-address test.
- D4/D5 replace argument-built launches with platform APIs; D4 is also OD2.
- Partial deletes (a locked file keeps the folder) leave sizes unchanged with no notice: the disk check cannot tell them from a cancel. Stated limit, in OD3's scope.
- Folder content changes by non-delete verbs (extract here, new zip) are not detected: Out of Scope.
- Exact Slint key names, menu owner and modal-call placement are deliberately left to S1-S8; P4 cannot start without them.
- Unreadable items share the "changed on disk" line (spec text) rather than a new message.

## Spike results (P1, 2026-10-26; full log `logs/CM-spike.md` and evidence in the work folder)
S1-S10 answered; the subclass works (`SetWindowSubclass` on Slint's HWND; keep a hidden owned helper window as the fallback if it returns false). Changes to the phases:
1. Keys arrive through Slint: Menu key = `Key.Menu` (U+F735); Shift+F10 = `Key.F10` with `modifiers.shift`. The right button arrives in `pointer-event` (`PointerEventButton.right`); `TouchArea.clicked` does NOT fire for it. `WM_CONTEXTMENU` (lParam -1) also arrives for the real keys: the subclass must IGNORE it (else a double open).
2. Menu owner = the subclassed Slint HWND; forward `WM_INITMENUPOPUP` (needed for Open with / Send to) and keep forwarding `WM_DRAWITEM`/`WM_MEASUREITEM`/`WM_MENUCHAR` (never sent here).
3. Bug found: after `TrackPopupMenuEx` returns, the click that closed the menu lands at the stale right-click point. Send one `WM_MOUSEMOVE` with the current cursor position to the Slint HWND after the menu. New manual check M17: right-click a tile, then left-click a different tile to close the menu: the new tile gets selected.
4. Shell separators use ids 0x7FFC-0x7FFE; `picked` stays total; fixtures must not assume every id has a verb.
5. `CMIC_MASK_UNICODE` is missing in windows 0.62.2: define `0x4000` locally; test `invoke_mask` asserts 0x4000.
6. OD3 resolved by S7: `InvokeCommand("delete")` is synchronous (returns after the item is gone); cancel returns `0x80270000`, also after "Item Not Found"; success `S_OK`. `judge` input: HRESULT `0x80270000` -> Nothing. The slow-delete risk does not exist here.
7. A path deleted between the pre-menu check and the pick: the shell shows "Item Not Found" and `InvokeCommand` blocks until the user closes it. Re-run `read_disk` right before `InvokeCommand` and skip the invoke when the path is Gone.
8. The first menu takes up to ~0.8 s with the UI frozen (`QueryContextMenu` blocks; extensions): set a wait cursor (`SetCursor(IDC_WAIT)`) before `QueryContextMenu`. While a menu, the Delete confirmation or Properties is open, the Slint window does not repaint (winit buffers WM_PAINT).
9. Pass the shell the plain path; a `\?\` prefix fails with E_INVALIDARG (long paths up to 328 chars work without it). `C:\` gives a menu without `delete`.
10. `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` after the window exists returns S_FALSE (winit already called OleInitialize): treat S_FALSE as success. The classic light menu shows on the dark window (a dark menu needs undocumented `SetPreferredAppMode`: out of scope). Not tested by the agent (owner steps): real right-click, Menu key and Shift+F10 by hand, Shift+right-click (extended verbs), a real 200% monitor, closing the window while Properties is open (M9, M10).

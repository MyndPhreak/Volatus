# Volatus delete: design

Date: 2026-10-25. Status: draft for approval. Written after a conversation with the owner. Lines marked **(my call)** are the writer's defaults or guesses, not the owner's words.

## Intent

Volatus is used to free disk space: find the big folder, open into it to check, then get rid of it. Today Volatus can only look. The owner (and a friend who uses SpaceMonger the same way) says deleting "is the main use really, getting rid of garbage and old files". Two real cases were named:

- "I need 30 GB right now": delete enough to reach a size.
- "A video project from 3 years ago": copy a folder to a server, then wipe it.

The owner said the folders are usually removed whole ("I usually nuke entire folders"). The owner also suggested a custom delete dialog, because the standard Windows delete is slow.

## What the owner decided

- Volatus deletes. It is a main feature, not an extra.
- Two ways to delete, both wanted: a **fast permanent delete** and **send to the Recycle Bin**.
- A custom dialog is wanted (the owner said "maybe"; treat it as wanted).
- Whole folders are the usual target. File-level work is secondary.

## Goals

1. Delete the selected file or folder from the treemap and from the folder tree, from a right-click context menu (owner decided). **Delete** moves it to the Recycle Bin. **Shift held while choosing Delete** (or Shift+Delete on the keyboard, as in Explorer) deletes it permanently. **(my call)** The Delete key alone does the Recycle Bin delete. The builder may add a toolbar button.
2. **Fast delete** that is clearly faster than Explorer on a folder with many files, because it does not pre-count the files (the scan already has the count and size), does not move files to the Recycle Bin, and deletes in parallel.
3. **Recycle Bin option** for when the user is not sure.
4. **One dialog** that names the item, shows its size and file count (from the scan), and has both actions. It shows progress, can be cancelled, and ends with a result: how many items and bytes were removed, and a list of anything that could not be removed.
5. After a delete, the open scan stays correct: the removed item leaves the tree, its size leaves every parent folder, and the treemap and tree redraw. No rescan is needed.

## Non-goals and anti-goals

- Not in this project: the Sankey view, the age colors, the "I need X GB" running total, multi-select. They are the next project. **(my call)** One item at a time here, with the design not blocking multi-select later.
- No undo for the permanent delete. The dialog says so in plain words.
- No auto-pick of what to delete. No "clean up for me" button.
- Anti-goals: deleting something other than what the dialog named (a stale scan, a changed folder, a junction followed into another place); a permanent delete that is one stray click away; a dialog that says "done" while files remain; the tree and sizes shown after a delete that disagree with the disk.

## Constraints

- Rust workspace (`app`, `scan`, `treemap`), Slint UI, Windows 11. The scan gives a `Tree` with `size`, `allocated`, `is_dir` and a path per node (`scan/src/lib.rs`). The tree is read-only today. Selection lives in `app/src/main.rs` (`selected`).
- Windows APIs are already used through `windows-sys` (`app/Cargo.toml`); new features of that crate are fine, other new dependencies need a reason.
- Engineering rules: plan first, tests first for the pure logic, delete is on by default (owner decided; a deviation from the usual flag rule, recorded here), no silent loss of anything, deviations recorded.
- UI text in plain ASD-STE100 style.
- A real delete tests only on a temporary folder the test makes. No test touches a path outside it.

## Safety rules (the part where a wrong guess costs data)

These are the writer's proposals, to be confirmed in the review of this document.

- **Plain Delete is always the Recycle Bin. (Owner decided, 2026-10-25.)** Permanent delete needs Shift held, so it cannot happen by a plain click. **(my call)** Both open the confirm dialog; Shift only makes it a permanent-delete dialog (its main button reads "Delete permanently" and the dialog says there is no undo). No "type DELETE to confirm" step.
- **Never delete a drive root, a Windows folder, or the folder the scan started from if it is a drive root. (my call)** The item must be a descendant of the scan root. The dialog refuses the drive root, `C:\Windows`, `C:\Program Files`, `C:\Program Files (x86)`, `C:\ProgramData` and the user profile folder itself (their children are allowed). The list is a constant in one place.
- **Junctions and symbolic links are removed as links. (my call)** A delete never follows a link into its target. A link is removed; what it points to stays.
- **The path is checked just before the delete, not only at scan time. (my call)** If the item no longer exists, or is now a different kind (file where a folder was), the dialog says so and does nothing.
- **Locked and protected files are skipped, not fatal. (owner agreed to "skip locked files and keep going")** The run continues, and the result lists every skipped path with the Windows error in plain words. Read-only files are cleared of the read-only flag and removed. **(my call)** Files that need administrator rights are skipped with a message that offers "Restart as administrator" (the app already has that action in `app/src/elevate.rs`).
- **A failure to read the tree or the path never falls through to a delete. (my call)** If anything about the target is unknown, the action is disabled.

## The deletion methods

- **Recycle Bin:** the Windows shell operation (`SHFileOperationW` or `IFileOperation`) on the one chosen path, with the standard progress UI turned off (Volatus shows its own). A folder goes into the bin as one item. If the shell reports "too big for the bin" or the bin is off for that volume, the dialog says so and offers the permanent delete; it never falls back by itself.
- **Fast permanent:** the builder chooses the exact method after measuring on a folder with many small files. The idea, as a guess to test and not a requirement: walk the folder once, delete files in parallel (many worker threads on one directory tree), then remove the empty folders from the deepest up, using the long-path form (`\\?\`) so paths over 260 characters work. A POSIX-style delete (`FILE_DISPOSITION_FLAG_POSIX_SEMANTICS`) is worth trying because it lets a file that is open elsewhere vanish from its folder at once. The builder checks which works on Windows 10 and 11 and records the result.
- **Both methods report progress** as items removed and bytes removed against the totals the scan gave. If the folder changed since the scan, the numbers can differ; the dialog does not claim an exact percentage it cannot know **(my call)**: it shows the count and bytes removed so far, and a bar based on the scan's totals that is capped just below full until the run ends.

## The dialog

Content, in this order **(my call on the wording and layout, owner decided the content)**:

1. The name and the full path of the item.
2. Its size and the number of files and folders (from the scan).
3. A line that says what each button does, in plain words: the Recycle Bin button can be undone in Windows, the permanent button cannot.
4. Buttons, by how it was opened: from plain Delete, **Move to Recycle Bin** (default) and **Cancel**; from Shift+Delete, **Delete permanently** (not the default; Cancel is) and **Cancel**. **(my call)** A small link in the dialog switches between the two, so a user who forgot Shift is not stuck.
5. While running: progress, items and bytes removed, a **Stop** button. Stop ends the run after the item in progress; what was already removed stays removed, and the result says so.
6. When done: a summary (removed, skipped, bytes freed) and the list of skipped paths with reasons. A **Rescan** button is offered if skips occurred.

The dialog is part of the Slint window (or a modal child window; the builder decides). It must work with the keyboard (Enter on the default button, Esc to cancel) and with a screen reader, as the rest of the app does.

## Keeping the scan correct

- After a delete, remove the node from the in-memory `Tree` and subtract its size (and `allocated`) from every ancestor, then redraw. The `Tree` is an arena with child and sibling links, so removing a node means unlinking it; the builder decides how (unlink, or rebuild the affected branch).
- If any items were skipped, the folder is still there with the skipped files in it. In that case the node's size is reduced by the bytes that were removed, and the skipped files stay in the tree **(my call)**. A full rescan is always available.
- The footer and the drive free-space numbers are refreshed after a delete.
- The selection moves to the next sibling or the parent after the item is removed.

## No flag

The owner decided delete is on from the first release. The safety rules above are the protection: the Shift requirement for permanent delete, the confirm dialog, the protected-folder list, and the check before each delete. There is no flag and no parity test for one.

## More calls, after a cold read (all mine)

- **Protected list.** Each of the six entries (the drive root, `Windows`, `Program Files`, `Program Files (x86)`, `ProgramData`, the user profile folder) is refused as the item itself. Everything inside is allowed, except `Windows` and everything inside it, which is refused too. Matching ignores case and is done on the full, resolved path (long names, no `\\?\` prefix difference). It applies to every volume, not only `C:`. Scanning a protected folder does not change the rule.
- **Stale scan.** Before a delete the app checks only that the path exists and is the same kind. It does not re-measure. The dialog's size and count are labelled "from the scan". A folder that grew since the scan is still deleted whole; the result shows the real counts.
- **Links.** The scan already records a junction or symlink as a zero-size file entry and never follows it (`scan/src/walk.rs`, `scan/src/mft.rs`), and every file is a node. So the dialog's numbers never include a link's target, and deleting a link removes only the link. The dialog names the item as a link. Cloud placeholder files are removed like any file.
- **Recycle Bin.** It is one shell operation, so Stop is not offered in that mode. The shell gives no per-file progress, so the dialog shows a busy bar and the result states the item moved. The result says plainly that **disk space is freed only when the Recycle Bin is emptied**, and the free-space figure does not change until then. After a Recycle Bin move the node leaves the tree, as the item left the folder. The permanent delete is the "free space now" action.
- **Elevation.** "Restart as administrator" starts a new, empty window as today. There is no restore of the dialog or the selection. The result list tells the user to restart as administrator and delete again.

## Left to the builder

- The exact fast-delete algorithm and thread count, measured and recorded in `docs/BENCHMARK.md` style with numbers against Explorer and against `rd /s /q`.
- Where the Delete button and key live in the treemap and the tree, and whether a right-click menu is added.
- How the tree removal is done and how it is tested (including parent size sums).
- The dialog's visual design, using the existing theme.
- How to test delete safely: the temporary-folder tests, the refusal list, link handling, locked-file handling (a test that holds a file open), long paths, read-only files.
- Updates to `README.md` and `docs/USAGE.md`.

## Seam with the shell menu (owner, 2026-10-26)

The Delete of this design is the only Delete in the shell context menu of PR #7: a **Delete** item in the Volatus group at the top runs this flow for the item the menu was opened on, with Shift read when the item is chosen; the shell's own Delete is hidden. See the addendum in `2026-10-26-volatus-context-menu-design.md`.

## Open items for the owner

None. The decisions marked **(my call)** are for checking while reading, the most important being the protected-folder list, that both Delete and Shift+Delete still show a confirm dialog, and that the Recycle Bin does not free space until emptied.

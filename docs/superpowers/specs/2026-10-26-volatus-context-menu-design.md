# Volatus context menu: design

Date: 2026-10-26. Status: draft for approval. Written after a conversation with the owner. Lines marked **(my call)** are the writer's defaults or guesses, not the owner's words.

## Intent

When the owner right-clicks a file or folder in Volatus (on a treemap tile, a tree row, or a crumb), they get a menu like the one in WizTree: a few Volatus items at the top, then the real Windows menu for that file or folder (the one Explorer shows, with every registered shell extension: Open with, Send to, 7-Zip, Defender and so on). The owner's two reference screenshots (WizTree's menu for a file and for a folder) are the model: "Explore Folder, Command Prompt Here, Copy Path ..." on top, then the shell items, ending in "Cut, Copy, Create shortcut, Delete, Properties".

## What the owner decided

- The menu is the real shell menu (`IContextMenu`) for the clicked item, with Volatus' own items above it, as WizTree does. The owner knows it will show "a lot of random crap": it shows whatever the machine has registered.
- It appears on files and on folders.
- **Delete** (through the shell) removes the item from the map at once: the tree and the treemap update without a rescan, and the parent sizes shrink.
- **Rename, Cut/Paste and other changes** do not need to update the map: the map is marked "outdated" with a "Rescan" button.

## Goals

1. Right-click on a treemap tile, a tree row or a crumb opens the menu at the pointer. The Menu key and Shift+F10 open it for the selected item, at the item's position.
2. The Volatus items work: Explore Folder, Copy Path, Command Prompt Here, Scan this folder (folders only).
3. The shell items are the real ones for that path and they run as in Explorer (a Delete asks Windows' own confirmation and uses the Recycle Bin unless Shift is held; Properties opens the Windows dialog).
4. After a Delete that really removed the item, the item disappears from the tree and the map within about 0.3 s on a 5 M-file drive, sizes of every parent folder are reduced, the file-type footer updates, and the selection moves to the parent.
5. After any other shell action that makes the clicked path vanish or change, the map shows an "outdated" notice with a "Rescan" button (rescans the same root).
6. Nothing the menu does can freeze the window for longer than the shell itself takes. (A slow shell extension makes the menu slow to appear; that is accepted, as in Explorer.)

## Non-goals and anti-goals

- No Volatus-made file operations (no own Delete, Move, Rename). Every change goes through the shell.
- No "Export to .CSV", "Copy file and size info" yet (seen in WizTree's menu; a later item).
- Not a failure: a shell extension item that does nothing useful in Volatus. It is a failure if a click on a menu item can start a second scan, run a destructive action the user did not choose, or leave the map showing a deleted item as present with no notice.
- No special protection against deleting system folders: the shell's own rules and confirmation apply.

## Constraints

- Rust + Slint 1.18, Windows 11, the existing Mica window. The shell menu needs the window's `HWND` (from the existing `window_handle`), a single-threaded apartment (the UI thread), and the window's messages for owner-drawn items (Open with, Send to). Slint does not expose the window procedure, so a window subclass (`SetWindowSubclass`) is likely needed. The `windows` crate is already a dependency of the app.
- The scan tree is shared read-only with worker threads (`Arc<Data>`), so a deletion must not mutate it in place.
- Existing behaviour stays: keyboard navigation, selection sync, hover/tooltip (hidden while a menu is open), zoom, labels, crumbs, footer, start screen, cancel, async Browse, the Explorer menu entries from the installer.
- Names and paths shown in the Volatus items go through `display_name` where they are displayed; the menu's shell part is Windows' own.
- UI text in plain ASD-STE100. Engineering rules: plan first, tests first for pure logic, manual checks for the shell parts, deviations recorded. Nothing is pushed without the owner's go-ahead.

## My calls (the owner has not seen these as decisions)

- **Volatus items, in this order:** Explore Folder (for a file: opens its folder with the file selected), Command Prompt Here (a terminal in the folder, or the file's folder; the default terminal, Windows Terminal if present), Copy Path, Scan this folder (folders only; the same as a double-click). A separator, then the shell items.
- **Shell menu flags:** the normal menu (`CMF_NORMAL`), the extended verbs only when Shift is held (as Explorer). No Rename item (the "can rename" flag is not set), because a rename cannot be shown in the map without a rescan. Cut and Copy stay (they only touch the clipboard).
- **Which path:** the clicked tile, row or crumb is the item. A right-click on an unselected item selects it first (like Explorer). For a frame header or tile the item is that file or folder. Several selected items are not supported (one item).
- **Delete detection:** after the shell command returns, Volatus checks whether the item still exists on disk (`GetFileAttributes` style check; a short re-check after 0.5 s and 2 s because some shells return before the work ends). If the verb that ran was `delete` and the path is gone, the item is removed through an overlay. Any other change (the path is gone after a rename or a move, or sizes could have changed) shows the outdated notice.
- **Removal mechanism:** an overlay record (removed node ids and per-ancestor size deltas) applied at layout and in the tree rows, not a copy of the 5 M-node arena (a copy would cost about 0.2 to 0.4 s and a lot of memory). The overlay is cleared by a rescan.
- **When the menu is available:** only on a finished scan (not while scanning, not on the start screen). During a zoom animation the menu opens on the final layout.
- **Deleting the scan root or an ancestor of the view:** the view moves to the nearest surviving ancestor; if the root itself is gone, the app returns to the start screen with a short message.
- **Rescan button:** shown in the results top bar next to "New scan" only while the map is outdated; it rescans the same root with the same options.
- **Menu look:** the standard Windows menu (dark or light as Windows draws it). No custom-drawn menu.

## Left to the builder

- The window subclass or message hook, how `IContextMenu2/3` messages are forwarded, `TrackPopupMenuEx` flags, COM initialisation, releasing the shell objects.
- The terminal launch (which executable, quoting a path with spaces and unicode).
- The overlay data structure, how the tree rows, labels, hover and the footer totals read it, and how the type totals are corrected (subtract the deleted subtree or recount).
- Keyboard handling for the Menu key and Shift+F10, the menu position for a keyboard-opened menu, and focus after the menu closes.
- How to test the shell-facing code without the real shell (a fake path provider for the pure logic; the real menu is a manual check).

## More calls, after a cold read (all mine; mark them as such)

- **A wrong path must never reach the shell (fail closed).** The path of an item is the scan root path plus the raw node names up the parents (`Tree::path`); the root path comes from the finished scan (`Session.scan_root`; if `Data` lacks it, add it). `display_name` is for display only and never used for a path. Names are Rust strings; a name that was changed by a lossy conversion at scan time, or an item that no longer exists, would give a wrong or missing path. Before the menu opens, the path is checked on disk (it exists, and a folder is a folder, a file is a file). If the check fails there are no shell items: the menu shows one line, "This item changed on disk. Rescan.", plus the Rescan action. Test: names with unusual characters, a deleted item, a file replaced by a folder.
- **How "delete" is recognised.** Only the shell's own canonical verb counts. After the user picks an item, Volatus reads the verb with `GetCommandString` (`GCS_VERBW`) for that command id before invoking it. Only the verb `delete` can remove an item from the map. Any other verb (including third-party items whose text says "delete") that makes the path vanish, or any change that cannot be told, shows the outdated notice. After invoking `delete`, Volatus checks the disk once when the call returns and again after 0.5 s and 2 s (the Recycle Bin move can finish late). There is at most one pending check; a new right-click, a rescan, "New scan" or closing the window cancels it. If the user cancels the shell's confirmation the path still exists and nothing changes.
- **What the overlay corrects.** Both `size` and `allocated` of every ancestor, the per-node file counts, the "percent of parent" values, and the file-type totals (subtract the removed subtree: cost proportional to its size, no recount). Removed nodes are skipped by the tree rows, layout, labels, hover and hit-test; sibling order is not changed. The scanner counts a hard-linked file once, so deleting one name removes the whole counted size (a stated limit; the outdated notice is not shown for it). The overlay is cleared by a rescan.
- **Crumbs and frames.** A right-click on a crumb is that folder (the scan root and a drive root included; Delete on them is whatever the shell allows). A right-click on a folder frame header or body is that folder; on a file tile it is that file. New callbacks carry the button and the position in window coordinates from `.slint` to Rust.
- **Keyboard.** The first phase is a short spike (like the renderer spike): can the window subclass (`SetWindowSubclass`, calling `DefSubclassProc`, removed on exit) see `WM_CONTEXTMENU`, `VK_APPS`, Shift+F10 and the menu messages (`WM_INITMENUPOPUP`, `WM_DRAWITEM`, `WM_MEASUREITEM`, `WM_MENUCHAR`) on this Slint/winit setup, and where does a keyboard-opened menu appear (the selected row's or tile's rectangle in window coordinates). The plan's later phases depend on its result.
- **State.** The outdated notice, the overlay and the pending check live in `Session`, so they survive a resize and a zoom; a rescan of the same root clears all three, and "New scan" clears them too.
- **Terminal.** Windows Terminal (`wt.exe -d <folder>`) when it exists, else `cmd.exe`. If neither starts, a one-line banner message.

## Open items for the owner

None. The owner confirmed on 2026-10-26: (1) the `windows` crate is added as a direct dependency of the app, in the version family already in `Cargo.lock` (recorded deviation); (2) the menu ships with no runtime flag (recorded deviation).

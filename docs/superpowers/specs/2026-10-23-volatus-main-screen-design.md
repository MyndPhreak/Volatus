# Volatus main screen redesign: design

Date: 2026-10-23. Status: draft for approval. Written after a conversation with the owner, who chose the look and layout. Lines marked **(my call)** are the writer's guesses or defaults, not the owner's words.

## Intent

Volatus is a fast disk-usage analyzer for Windows (Rust, Slint, Mica). The scan is already fast: a warm elevated scan of a 1.6 TB drive with 5 M files takes about 5.6 s, against about 23 s for WizTree. The screen is not yet good. Today the owner sees "basic lists and squares".

The owner wants one thing from the main screen: right after a scan, see **what takes up the most space**, as labeled groups, and **dig deeper by double-clicking a group**. The treemap is the main focus of the app. WizTree's treemap is the reference for the feel, but the owner wants it less flat and less clunky.

## What the owner decided

- **Layout after a scan.** Tree on the left (a sidebar), treemap on the right (the main area), a file-type strip as a footer along the bottom.
- **Treemap style.** Grouped: every folder is a frame with its name and size in the top-left corner, like `Local\ (306 GB)`. Frames nest all the way down and their labels stack and indent, as in WizTree. Files at the bottom are colored tiles. A label shows only when its frame is big enough to hold it.
- **Look.** "Fluent glass": frames are frosted glass over the Mica window with a thin light border. Tiles are rounded with a soft highlight along the top edge. Flat colors were "boring".
- **Drilling in.** Double-click a frame to open that folder as the new top level. A breadcrumb above the treemap and Backspace go back up.
- **Hover.** A tile lifts and glows. This must be drawn **on top of everything**, so the folder frame never cuts it off.
- **Start screen.** Before a scan: one glass **button for each drive** for a quick scan (name, used/free bar, size), plus a "Choose folder" button. A click starts the scan.
- **Scope.** No file deletion or moving, no search box, no duplicate finder, no settings page. "It'll come" later.

## Goals

1. After a scan of `C:` (5 M files) the owner sees the nested, labeled treemap and the tree sidebar within about 1 s of the scan ending, and the window stays responsive.
2. Opening a folder, going up, selecting and hovering feel instant (under about 100 ms to the first changed frame).
3. The look matches the owner's description: nested labeled glass frames, rounded tiles with highlight, glow on hover, Mica backdrop, light and dark themes following the system.
4. The tree and the treemap always show the same selection.
5. The file-type footer shows the biggest file types as colored chips with percentages, and the colors match the tiles.

## Non-goals and anti-goals

- No file operations (delete, move, open in Explorer) in this work.
- No search, duplicate finder, settings, or color-by-age mode (the owner liked the idea less than glass; it needs modified dates that are not loaded). A color-by-age toggle may come later.
- Not a failure if some small frames show no label. It is a failure if the window hangs during a scan or a resize, if the treemap and tree disagree on the selection, or if the UI loses today's keyboard access.
- No per-tile glow on all tiles at once (too costly); only the hovered tile glows.

## Constraints

- Slint 1.18 with the Fluent style and the existing Mica setup. Windows 11. Keep light and dark themes following the system.
- Existing behavior stays: keyboard navigation in the tree, headers and treemap with focus rings; immediate Cancel; async Browse dialog; the `--timing` banner line; "Restart as administrator" only offered after a standard scan of a whole drive; the fast path only for whole drives.
- Performance: the UI thread must never block on layout or scan work. Layout and rendering already run on a worker thread with a debounce. A 6 M-node tree must stay workable.
- UI text follows ASD-STE100 (short, plain sentences).
- Engineering rules apply: plan first, tests first for logic (red commit then green), exemptions stated for visual work with a manual check in their place.

## How it gets made (decided by the owner, as decided)

- Slint stays the UI toolkit, built on the existing `app` crate.
- The owner pointed out that Slint has a live preview and an MCP server for a running app; the builder should use them to check the UI.

## My calls (the owner has not seen these as decisions)

- Tile color follows **file type** (WizTree style), and the footer chips are the color key. The owner never said so; it follows from the WizTree screenshots.
- Folder labels are drawn as real Slint text over the treemap picture, only for frames big enough, so text stays crisp. The tiles themselves stay a bitmap drawn on the worker thread.
- Overlays drawn above the whole treemap: hover glow and lift, the tooltip (full path, size, file count), and the selection outline that slides from one frame to the next.
- Opening a folder zooms in with a short animation; going up zooms out. Tiles fade in when a scan ends.
- Cushion shading (bright top-left, dark bottom-right) on small tiles so thousands of tiny ones stay readable, as an extra, not the main look.
- The tree sidebar follows what the treemap shows: it expands to the selected folder.
- The start screen is the window's state before the first scan. After a scan starts, the window switches to the tree/treemap layout with a progress bar and Cancel.

## Left to the builder

- Label fit rule, minimum frame size for a label, and the maximum number of overlay labels on screen at once.
- Animation lengths and easing.
- The color per file type (palette, a fixed set of about 10, the rest grey), and how colors stay readable in light and dark themes and for color-blind users.
- How to get the file-type totals for the footer and per-tile type colors. The tree stores names but no extension or type; an extension can be read from the name after the scan. Where and when that pass runs is the builder's decision, as long as the scan and the UI do not slow noticeably.
- Drive list and used/free values (Windows APIs), and what to show for drives that cannot be scanned.
- Window minimum size, DPI scaling, and accessibility names for the new elements.
- Whether the treemap bitmap is redrawn on hover (should not be) and how overlays are layered.

## Open items for the owner

None now. Questions raised by the builder check are listed in the hand-over message.

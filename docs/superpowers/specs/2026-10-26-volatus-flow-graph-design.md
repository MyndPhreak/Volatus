# Volatus flow graph: design

Date: 2026-10-26. Status: draft for approval. Written after a conversation with the owner. Lines marked **(my call)** are the writer's defaults or guesses, not the owner's words. A clickable mockup was shown in the conversation (a Sankey-style graph with sample data); it is a sketch of the idea, not a layout to copy.

## Intent

The treemap answers "which files and folders are big". The owner also wants a high-level picture of **where the data goes**: the scan root on the left, its folders as bands that flow to the right, and the ability to open a folder and watch the data split further. A friend of the owner asked for it ("everyone always does boxes") and showed a money-flow (Sankey) chart as the model. The owner's reading of it: "just a different way of mapping the tree": folders first, expand a folder to see where its data goes, a high-level overview.

## What the owner decided

- It is a **second view**. The treemap stays the main view and the default; it is "too nice to replace".
- It maps **folders**. You can **expand a folder** to see where the data goes inside it.
- It is a **way to look**, not a cleanup tool: no "I need X GB" total, no picking of several folders, no delete-all-picks. (The owner dropped those after seeing the mockup.)
- The **context menu stays**: right-click on a band opens the same menu as on a treemap tile (the shell menu with the Volatus items and the one Volatus Delete).
- Band colors: **age**, with a **toggle** to **file type**.

## Goals

1. A switch in the results top bar: **Treemap | Graph**. Treemap is the default. Both views read the same scan, the same selection and the same view folder.
2. The graph shows the view folder on the left and its biggest subfolders as bands to the right; the width of a band is its size. Folders under 3 % of the view folder merge into one "other (N)" band. Files of a folder show as one "files" band, so single files are never drawn.
3. **Expand in place**: a click on a folder's expander shows its subfolders as a new column to the right; another click collapses it. Double-click (or Enter) makes the folder the new view folder, as in the treemap. The crumbs above the map are shared with the treemap.
4. **Selection is shared.** A click on a band selects that folder in the scan, and the tree and the treemap follow (and the other way). The Delete key and Shift+Delete work on the selected band, with the dialog and every safety rule of the delete design.
5. **Right-click** on a band, and the Menu key and Shift+F10, open the existing context menu for that folder (the on-disk check, the Volatus items, the shell items, the Volatus Delete). Right-click on empty space opens nothing.
6. **Colors.** Age: the newest modification inside the folder, in three steps (changed in the last 6 months, 6 to 24 months, over 24 months) with a legend **(my call: the steps)**. File type: the file type that holds the most bytes in the folder, with the same colors and legend as the treemap. A toggle in the graph's bar switches between them; the default is age **(my call)**.
7. A hover tooltip with the folder name, size, share of its parent, file count and the age of its newest change **(my call: same tooltip rules as the treemap)**.
8. The graph works on a 5-million-file scan without a visible stall: it draws only the bands that are on screen (a few hundred at most), and heavy work (file-type totals) runs off the UI thread.

## Non-goals and anti-goals

- No picking, running totals, size targets or multi-item delete. No "clean up for me".
- No animation of the flows **(my call)**; opening and closing a folder may be instant.
- No new way to change the scan tree: a delete from the graph goes through the existing delete flow and `Tree::detach`.
- Anti-goals: a graph whose band widths mislead (width is exactly proportional to size; a band too small to draw is merged, never stretched to a minimum width); a click or right-click that acts on a different folder than the one under the pointer; the window freezing while the graph builds; showing a deleted folder after a delete; two selections that disagree between the views.

## Constraints

- Rust workspace (`scan`, `treemap`, `app`), Slint 1.18, Windows 11. Read `docs/superpowers/specs/2026-10-26-volatus-context-menu-design.md` and `docs/superpowers/specs/2026-10-25-volatus-delete-design.md`: the menu, the selection model, the delete flow and the `Arc<Data>` rules (workers read the tree; only the UI thread changes it through `Tree::detach` and the pending-removal mechanism) apply unchanged.
- The scan records **no timestamps today** (`scan::Node` has size, allocated size and links only). Age needs a modified time per entry from both scanners (the MFT path reads it from the file record; the walker from the directory listing), kept in the tree and rolled up per folder as "newest modification in the subtree" **(my call: newest inside, not the folder's own time, because a folder's own time changes only when entries are added or removed)**.
- The scan must not get measurably slower: the MFT scan of the benchmark drive in `docs/BENCHMARK.md` must stay within 5 % of its current time, and memory must grow by no more than 8 bytes per node **(my call: both are measurable limits for the plan)**.
- Existing behaviour stays: treemap, tree, footer, crumbs, tooltips, settings (units, theme), the start screen, the installer entries.
- UI text in plain ASD-STE100. Engineering rules: plan first, tests first for pure logic, manual checks for the drawing, deviations recorded. Nothing is pushed without the owner's go-ahead.

## My calls (the owner has not seen these as decisions)

- **Structure.** The layout is a pure function in a new crate `flow` (like `treemap`): input the tree, the view folder, the set of expanded folders and the size in pixels; output the bands, the flows between them and what is drawn in each. It is unit tested without a window.
- **Drawing.** The graph is drawn to a picture on a worker thread (as the treemap is), the labels and expanders are Slint items over it, and hit-testing uses the band rectangles. This reuses the staleness rules of the treemap layout (a result for an old layout is dropped).
- **The view folder is shared** with the treemap, so switching views keeps your place. The set of expanded folders belongs to the graph and is cleared by a rescan and when a folder in it is deleted.
- **Columns.** Up to four columns fit the window; deeper expansion scrolls sideways. A folder with no subfolders has no expander.
- **Keyboard.** Arrow keys move the selection between bands (the same idea as the treemap's neighbour search); Enter opens a folder, Right expands, Left collapses.
- **Remembering the view.** The last view (treemap or graph) is remembered in the settings, using the settings of PR #8.
- **File-type color of a folder** is computed lazily for the folders on screen and cached, on a worker thread, because a per-folder table for 5 million nodes costs too much memory.
- **When the graph is available.** Only on a finished scan, as the menu is. While a scan runs the switch shows the treemap's existing status.
- **Accessibility.** The tree stays the accessible form of the data. The graph exposes a short text summary and the selected band's name and size.

## More calls, after a cold read (all mine)

- **View folder.** The graph's left column is the shared view folder (the scan root at first). A double-click or Enter on a band changes the view folder for both views, and the crumbs move both. Re-rooting clears the expanded set; navigating up does too.
- **Sizes.** Band width uses the same field as the treemap: the logical size (`Node::size`), counted the same way (a hard-linked file once, links as zero-size entries). One definition for both views.
- **"Other" threshold.** A band merges into "other (N)" when it is under 3 % of its own parent band **or** under 6 pixels tall, at every depth (the threshold is not tied to the view folder, so expanding a folder in a large tree still shows its real subfolders). A parent's flow is always the full sum of its children (subfolders, the "files" band and "other"), so widths are conserved.
- **The "files" and "other" bands.** They are not folders: they cannot be selected as folders, they have no context menu and no Delete. A click on a "files" band selects its folder. A click on "other (N)" shows its members as normal bands in that column. Their tooltips give the count and the size. Age of "other" = the newest age of its members; file type of "other" = the type with the most bytes across its members; file type of "files" = the type with the most bytes among that folder's own files.
- **Timestamps.** The modified time is the last-write time: on the MFT path the `$STANDARD_INFORMATION` last-write time, on the walker path the last-write time from the directory listing. Both give the same meaning. The tree keeps 4 bytes per node (for example whole seconds since 2000-01-01, unsigned), which is within the 8-byte limit. An entry whose time cannot be read has no age; a folder with no known time inside is drawn in a fourth class, "age unknown" (gray), with its own legend entry.
- **After a delete.** `Tree::detach` keeps sizes and counts exact as today. The "newest change inside" of an ancestor is left as scanned (it can be older-looking than the truth by at most the deleted item's time); the plan notes this as a known limit and the rescan fixes it.
- **Settings.** PR #8 (settings) is merged into `main`, so the stored view choice uses it directly. The graph branch starts from `main`.
- **Performance budget.** Opening or collapsing a folder, and switching views, redraw in under 100 ms on the benchmark scan (excluding the file-type colors, which fill in later without blocking). Reference scans: the benchmark drive of `docs/BENCHMARK.md` and a synthetic 5-million-node tree in tests.
- **Focus and keys.** The view switch and the color toggle are ordinary buttons in the tab order, before the map. Inside the graph: arrows move the selection, Right expands, Left collapses, Enter opens, Delete and Shift+Delete delete. The accessibility summary is a short sentence ("Folder graph of <folder>: N bands, largest <name>, <size>"), not exact wording.

## Left to the builder

- The layout algorithm (band order, spacing, how a flow leaves a parent band, label placement and collision), and the exact "other" rule near the 3 % line.
- The timestamp encoding in the tree (for example seconds or days since a fixed date in 4 bytes) and where the roll-up runs, and how `Tree::detach` keeps the newest-modification values correct (or leaves them as scanned, with the reason).
- The drawing details: curve shape, colors for the dark and light themes, how the age and file-type palettes meet the treemap's.
- How the expanded set is stored and what happens to it on a delete.
- How to test the drawing (pure layout tests; the picture is a manual check) and how to measure the scan cost.

## Open items for the owner

None. The calls above are the ones to check while reading, the most important being: the age steps (6 and 24 months), age as "newest change inside", the 5 % scan-time limit, and remembering the chosen view.

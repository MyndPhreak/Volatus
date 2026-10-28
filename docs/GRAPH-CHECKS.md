# Flow graph: checks for the owner

These checks need a person at the keyboard, or an elevated terminal. An agent cannot do them. Do each step, then write what you saw in the **Result** line. Paste the filled list into the pull request.

Use a release build (`cargo build --release -p app`). Use a scan that has a few big folders and many small ones. A temporary folder is best for the delete steps (M7, M14, M15). The text of the views is in [USAGE.md](USAGE.md#graph).

Not yet verified by anyone: the elevated scan cost (M10) and the real-drive timing (M8).

## M1 Remember the view

1. Start Volatus. The treemap shows.
2. Scan a folder. Choose **Graph** in the top bar. Close Volatus.
3. Start Volatus again and scan. The graph shows when the scan ends.
4. Start a scan, and look at the **Treemap | Graph** switch while it runs. It must be gray and not respond. Check that `%APPDATA%\Volatus\settings.json` has `"view": "graph"`.

Result:

## M2 Bands

Scan a folder with a few big subfolders and many small ones.

1. Look for the bands, a band named "other (N)" and a band named "files".
2. Compare the heights of two bands that have sizes 1 to 2. The big one must be about twice as tall.
3. Nothing is stretched: no small band is taller than its size says. Click "other (N)": its folders show as bands. Folders under 6 pixels stay in "other".

Result:

## M3 Open, close and scroll

1. Open a folder with its expander (**+**). Close it (**-**).
2. Select a folder and press Right, then Left.
3. Open 6 levels. The view scrolls by whole columns, and four columns show at most.
4. Hold Shift and turn the mouse wheel. The columns scroll back.

Result:

## M4 Light and dark

In the settings, choose Light, then Dark. For each, look at the bands, the ribbons, the labels and the age legend. All of them must be easy to read.

Result:

## M5 File type colors

1. Choose **File type** in the **Color** switch.
2. The colors fill in with no freeze. The window stays usable.
3. The colors match the chips in the footer.
4. Choose **Age** again. The legend shows four entries.

Result:

## M6 Menu

1. Right-click a folder band. The menu is the menu of that folder: the Volatus items (with **Delete**), then the Windows items.
2. Right-click a "files" band, an "other" band and empty space. No menu opens.
3. Select a folder band. Press the Menu key. Press Shift+F10. The menu opens under the band.

Result:

## M7 Delete

1. Make a temporary folder with files in it. Scan it. Choose a folder band in the graph.
2. Press Delete. The usual dialog shows. Cancel it.
3. Press Shift+Delete. The dialog for the permanent delete shows. Confirm.
4. After the delete, the band is gone. The parent bands are smaller. Folders you expanded before are still expanded.

Result:

## M8 Timing on a whole drive (elevated)

1. Open PowerShell as Administrator. Start `volatus.exe C:\ --timing`.
2. After the scan, open a folder, close it and switch Treemap and Graph. Do it ten times.
3. Every `Graph <seq>: <ms> ms.` line on stderr must be under 100 ms.
4. Write the highest value in the empty row of [BENCHMARK.md](BENCHMARK.md#flow-graph-timing).

Result (highest value, and the date):

## M9 Tooltip

1. Put the pointer on a band. The tooltip shows the path, the size, the share, the number of files and the date of the last change.
2. Put the pointer on a "files" band and on an "other" band. The tooltip says "Files in ..." or "Other folders in ...".
3. Click, press a key, or scroll. The tooltip hides.

Result:

## M10 Scan cost (elevated)

The scan now stores a time for each entry. The limit is 5 % more scan time on the MFT path. This is not yet measured. The commands are in [BENCHMARK.md](BENCHMARK.md#scan-with-entry-times).

1. In a normal terminal, build the bench in a checkout of `origin/main` and in this branch: `cargo build -p scan --release --example bench`.
2. In PowerShell as Administrator, run:

   ```powershell
   powershell -ExecutionPolicy Bypass -File docs\bench-ab.ps1 -Main <main checkout> -Branch <this checkout> -Root C:\ -Method mft
   ```

   The script ends with exit code 1 when the branch is more than 5 % slower.
3. In the same terminal, run the live parity test:

   ```powershell
   cargo test -p scan --test parity -j 1 -- --ignored
   ```

Result (branch and main medians of `total`, the difference in %, and ok or failed for the parity test):

## M11 Ribbons and labels

The ribbons are 40 % opaque in the dark theme and 50 % in the light theme. Check the labels over the ribbons in both themes, at 100 % and at 200 % scale. Every label must be easy to read.

| Theme | Scale | Readable? |
|---|---|---|
| Light | 100 % | |
| Light | 200 % | |
| Dark | 100 % | |
| Dark | 200 % | |

Result:

## M12 Crumbs and the view switch

1. Choose **Graph**. Use the crumbs to change the open folder several times.
2. Choose **Treemap**.
3. The treemap shows at once at the right size. There is no old, enlarged picture and no unwanted zoom.

Result:

## M13 Keyboard only

Do not use the mouse.

1. Press Tab until the graph has the focus ring.
2. With no folder band selected, press Right. The first folder band is selected. Press Down and Up to move between bands.
3. Press Right: the folder expands. Press Left: it closes.
4. Press Enter: the folder becomes the open folder. Press Backspace: you go up one folder.
5. Select a folder band. Press Delete. The delete dialog opens. Press Esc.

Result:

## M14 Delete from a "files" band

1. Click a "files" band. The band's folder is selected.
2. Press Delete.
3. The dialog names the **folder**, not the files. Cancel the dialog.

Result:

## M15 The graph after a delete

1. Choose **Graph**. Expand a folder that is a temporary folder.
2. Delete that folder (Delete, then confirm).
3. The graph is drawn again without the folder. The tree and the treemap also do not show it.

Result:

# Volatus: how to use it

Volatus shows which folders and files use the space on a drive.

## Start screen

- The window opens on the start screen. It shows one button for each ready local drive, with the
  free space and a bar that shows the used space.
- Click a drive button to scan the whole drive.
- Click "Choose folder" to scan one folder.
- Drives made with `subst` are not shown. Network drives are not shown.
- If a scan stops or fails, the window goes back to the start screen and shows why.

## Results screen

- The top bar shows the scanned drive or folder and a summary of the scan.
- "Cancel" stops a scan at once.
- "New scan" goes back to the start screen and removes the result from memory.
- The file tree on the left shows the folders and files, with size, percent of the parent folder
  and number of files. Click a column header to sort by it.
- The treemap shows each folder as a frame with its name and size. Each file is a tile in the
  colour of its file type. A bigger tile is a bigger file.
- Put the pointer on a tile or a frame to see its full path and size, and the file type or the
  number of files.
- Click to select. Double-click a folder to open it. Double-click a file to open its folder.
- The crumbs above the treemap show the folders from the scan root to the open folder. Click a
  crumb to open that folder.
- The footer shows the file types that use the most space, with their percent of the scan.
- The tree and the treemap always show the same selection.

## Keyboard

Press Tab to move to the next control, and Shift+Tab to move back.

| Where | Key | Result |
|---|---|---|
| Drive buttons, crumbs | Enter or Space | Scan the drive, or open the folder |
| Column headers | Enter or Space | Sort by the column. Again: change the order |
| File tree | Up, Down | Select the row above or below |
| File tree | Right | Expand the folder, or select its first item |
| File tree | Left | Collapse the folder, or select its parent folder |
| File tree | Space | Expand or collapse the folder |
| File tree | Enter | Open the selected item in the treemap |
| Treemap | Arrow keys | Select the next tile or frame in that direction |
| Treemap | Enter | Open the selected folder, or the folder of the selected file |
| Treemap | Backspace | Go up one folder |

## Administrator rights

- Without administrator rights, Volatus reads the folders one by one. This is slower.
- With administrator rights, a whole-drive scan reads the NTFS file table (MFT). This is much
  faster. A folder scan always reads the folders one by one.
- After a whole-drive scan without administrator rights, the top bar shows "Restart as
  administrator". Windows then asks for permission. The new window scans the same drive again.
- When a listed drive is NTFS, the start screen also shows "Restart as administrator". The new
  window opens on the start screen.

## Command line

```
volatus.exe [folder] [--timing] [--method auto|mft|walk] [--threads N]
```

- `folder`: scan this drive or folder at start. The options and the folder can be in any order.
- `--timing`: add the scan times to the top bar and write them to stderr. See
  `docs/BENCHMARK.md` for what each time means.
- `--method auto|mft|walk`: `auto` is the default. `mft` reads the NTFS file table and `walk` reads
  the folders one by one. A scan with `mft` does not fall back to `walk`. If it cannot run, the
  top bar says that it needs administrator rights and an NTFS drive.
- `--threads N`: use N threads. The most is 4 for each logical CPU. The top bar shows the number.
- `--method=mft` and `--threads=4` also work. Values are not case sensitive. If an option is given
  again, the last good value is used. A bad value or an unknown option is ignored, with a notice.
- `--`: what follows is a folder, also when it starts with `--`.
- All options apply to every scan in the run, and to "Restart as administrator".

Examples:
```
volatus.exe C:\Users\mynd\DEV --timing
volatus.exe D:\ --method walk --threads 8
```

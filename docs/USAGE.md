# Volatus: how to use it
Volatus shows which folders and files use the space on a drive.

## Start screen
- The window opens on the start screen. It shows one button for each ready local drive, with the free space and a bar that shows the used space.
- Click a drive button to scan the whole drive. Click "Choose folder" to scan one folder.
- Drives made with `subst` are not shown. Network drives are not shown.
- If a scan stops or fails, the window goes back to the start screen and shows why.

## Results screen
- The top bar shows the scanned drive or folder and a summary of the scan.
- "Cancel" stops a scan at once. "New scan" goes back to the start screen and removes the result from memory.
- The file tree on the left shows the folders and files, with size, percent of the parent folder and number of files. Click a column header to sort
  by it.
- The treemap shows each folder as a frame with its name and size. Each file is a tile in the colour of its file type. A bigger tile is a bigger
  file.
- Put the pointer on a tile or a frame to see its full path and size, and the file type or the number of files.
- Click to select. Double-click a folder to open it. Double-click a file to open its folder.
- The crumbs above the treemap show the folders from the scan root to the open folder. Click a crumb to open that folder.
- The footer shows the file types that use the most space, with their percent of the scan. The tree and the treemap always show the same selection.

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
| File tree, treemap, crumbs | Menu key or Shift+F10 | Open the context menu of the selected item |
| Settings gear | Enter or Space | Open or close the settings flyout |
| Settings flyout | Esc | Close the flyout |
| Settings flyout | Enter or Space | Choose the item, or turn the switch on or off |

## Context menu
- Right-click a tile, a frame, a tree row or a crumb. Or select the item and press the Menu key or Shift+F10. Shift+right-click adds the extended
  verbs. The menu works only for a finished scan.
- The menu shows the Volatus items first: Explore Folder, Command Prompt Here, Copy Path and, for a folder, Show this folder. The items of Windows
  for that file or folder follow. They depend on your shell extensions. The first menu can take up to one second while Windows loads them.
- Delete is the Delete of Windows. Windows asks you to confirm. Shift gives a permanent delete. After a delete, the item leaves the map and the tree
  and the sizes change. Volatus does not scan again.
- If the item changed on disk after the scan, the menu shows only "This item changed on disk. Rescan." This also covers a name that ends in a dot or
  a space, or has unusual characters.
- "Files changed after this scan." shows in the top bar when another action removes or changes an item. The sizes can be wrong. Select "Rescan" to
  scan the same folder again.
- Limit: a hard-linked file counts once. Delete one name, and the whole counted size leaves.

## Settings
- The gear is at the top-right of the window. It is on the start screen and on the results screen.
- Click the gear to open the settings flyout. The flyout opens on the right side. The map stays
  visible. Press Esc or click the gear again to close the flyout.
- **Theme**: "Follow Windows", "Light" or "Dark". "Follow Windows" uses the Windows theme and
  changes when the Windows theme changes. The other two choices keep their scheme.
- **Units**: "Dynamic", "Byte", "KB", "MB", "GB" or "TB". Every size in the window uses this unit.
  Each step is 1024 times the one before. "Dynamic" selects the unit that fits each size. "Byte"
  shows the exact number of bytes, with separators. With KB, MB, GB or TB, a size below 0.05 of
  that unit shows as "< 0.1" and the unit, for example "< 0.1 MB".
- **Show**: a switch for each part of the window. All parts show by default.
  - "Header": the scan title and the summary text in the top bar. The buttons stay.
  - "Folder tree": the file tree on the left. The treemap then uses its space.
  - "File type strip": the bar of file types in the footer.
  - "File type chips": the file type chips in the footer. The footer does not show when both the
    strip and the chips are hidden.
  - "Tile labels": the names on the tiles and the folder frames. The name of the tile under the
    pointer still shows. The tooltips stay.
- A choice takes effect at once. Volatus saves each change in `%APPDATA%\Volatus\settings.json`
  and reads this file at start.
- If Volatus finds a wrong or damaged field in the file, it keeps the file as `settings.json.bad`
  in the same folder. Volatus then uses the default settings for the fields that it cannot read.
  If the whole file is not valid, all fields use the default settings. The next change replaces
  `settings.json`.
- If Volatus cannot keep a copy of the file, it does not save any setting. Your choices still
  apply to the window until you close it. The flyout shows "Settings could not be saved."
  Volatus also does not save when it cannot find the `%APPDATA%` folder or cannot read the file at
  all. Both give the same message.

## Administrator rights
- Without administrator rights, Volatus reads the folders one by one. This is slower.
- With administrator rights, a whole-drive scan reads the NTFS file table (MFT). This is much faster. A folder scan always reads the folders one by
  one.
- After a whole-drive scan without administrator rights, the top bar shows "Restart as administrator". Windows then asks for permission. The new
  window scans the same drive again.
- When a listed drive is NTFS, the start screen also shows "Restart as administrator". The new window opens on the start screen.

## Command line
```
volatus.exe [folder] [--timing] [--method auto|mft|walk] [--threads N]
```

- `folder`: scan this drive or folder at start. The options and the folder can be in any order.
- `--timing`: add the scan times to the top bar and write them to stderr. See `docs/BENCHMARK.md` for what each time means.
- `--method auto|mft|walk`: `auto` is the default. `mft` reads the NTFS file table and `walk` reads the folders one by one. A scan with `mft` does
  not fall back to `walk`. If it cannot run, the top bar says that it needs administrator rights and a folder on a local NTFS drive.
- `--threads N`: use N threads. The most is 4 for each logical CPU. The top bar shows the number.
- `--method` and `--threads` take the next word as their value. Put the folder first or after `--`. A folder after `--method` or `--threads` becomes
  its value, and the option gets a notice.
- `--method=mft` and `--threads=4` also work. Values are not case sensitive. If an option is given again, the last good value is used. A bad value
  or an unknown option is ignored, with a notice.
- A folder that ends in a stray `"`, such as `C:"` from `"C:\"`, is read as `C:\`.
- `--`: what follows is a folder, also when it starts with `--`.
- All options apply to every scan in the run, and to "Restart as administrator".
- Examples: `volatus.exe C:\Projects --timing`, `volatus.exe D:\ --method walk --threads 8`.
- To install with no window and without the Explorer menu, run this in a terminal that runs as administrator:
  `msiexec /i volatus-0.1.0-x86_64.msi /qn EXPLORERMENU=0`. Upgrades keep it off.

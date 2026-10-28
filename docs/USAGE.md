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

## Graph
The graph is a second view of the same scan. It shows where the data of a folder goes.

### Switch the view
- The results top bar has a switch: **Treemap | Graph**. Treemap is the default.
- Volatus remembers your choice and uses it at the next start. It saves the choice as the `view` key in the settings file.
- The switch is not available while a scan runs. The treemap shows until the scan ends.
- Both views use the same scan, the same selection and the same open folder.

### Layout
- The open folder is the band on the left. Its folders are bands to the right of it. A ribbon joins each folder to its parent.
- The height of a band is the size of the folder. The height is exactly proportional to the size. Volatus never makes a small band taller to show it.
- A folder is merged into one band named "other (N)" when it is under 3 % of its parent folder, or when its band would be under 6 pixels tall. N is the number of merged folders. A band named "other files" holds files that are too small to draw on their own.
- A band named "files" holds the files that are directly in a folder. The graph never draws a single file.
- Click "other (N)" to show its folders as their own bands. A folder that is still under 6 pixels tall stays in "other (N)".
- The name and the size of a band show to the right of it.

### Open and close folders
- A folder that has subfolders shows an expander on its band: **+** to expand and **-** to collapse. The subfolders then show in a new column to the right, in the same view. Click the expander again to close them.
- You can also press **Right** to expand the selected folder and **Left** to collapse it.
- Press **Enter** or double-click a folder band to make that folder the open folder. This changes the treemap too.
- The crumbs above the graph are the same as the crumbs of the treemap. Click a crumb to open that folder. Press **Backspace** to go up one folder.
- Up to four columns fit in the window. If you open deeper levels, the graph scrolls by whole columns. Use the horizontal mouse wheel, or hold **Shift** and turn the wheel. The graph also scrolls by itself to show a new column or the selected band.

### Colors
- The bar above the graph has a **Color** switch: **Age** (the default) or **File type**. The choice lasts until you close Volatus.
- **Age** colors a band by the date of the newest change anywhere inside the folder. The legend in the bar has four entries:
  - "Changed in the last 6 months"
  - "Changed 6 to 24 months ago"
  - "Changed more than 24 months ago"
  - "Date not known"
- **File type** colors a band by the file type that uses the most space in it. The chips in the footer are the legend. If you hide the chips in the settings, the graph has no file type legend.
- The colors for file types can fill in a moment after the graph shows. The window stays usable.
- A folder gets its date from the newest file or folder inside it. The date of the folder itself is not used.
- After a delete, the sizes are exact at once. The dates of the parent folders stay as they were at the scan, until you scan again. A date can look older than it is.
- Volatus reads the dates while it scans. A scan that reads the folders one by one takes them from the directory listing. A scan of the NTFS file table takes them from the file records. Neither needs more rights than the scan itself. On a drive that is in use, the two sources can give a different date for a file that is open.

### Select, menu and delete
- The tree, the treemap and the graph always show the same selection. Click a band to select it. The tree and the treemap select the same folder.
- A click on a "files" band selects the folder that holds the files, not the files.
- Right-click a folder band to open the same menu as in the treemap. It has the Volatus items, with **Delete**, and then the items of Windows. You can also select a folder band and press the Menu key or Shift+F10. The menu opens under the band.
- "files", "other (N)" and empty space have no menu.
- Press **Delete** or **Shift+Delete** to delete the selected folder band. The dialog and all rules of the [Delete](#delete) section apply. Delete works only when the selection is a folder band that shows in the graph.
- Be careful: a click on a "files" band selects its folder. Delete then deletes the whole folder, not only the files. The dialog shows the name of the folder.

### Tooltip
- Put the pointer on a band to see its tooltip. It shows the path, the size, the share of the parent folder, the number of files and the date of the last change. The date is "not known" when Volatus could not read it.
- The tooltip hides when you click, press a key or scroll.

## Delete

Volatus can delete the file or folder that you select. Delete works on one item at a time.

- To delete, right-click a file or folder in the tree, in the treemap or in the crumbs and choose
  **Delete** in the Volatus items at the top of the menu. You can also select the item and press
  **Delete**. Volatus then asks you to confirm. After you confirm, it moves the item to the Recycle
  Bin.
- To delete permanently, hold **Shift** when you choose **Delete**, or press **Shift+Delete**.
  After you confirm, Volatus removes the item from the disk. You cannot undo a permanent delete.
- Volatus reads the Shift key at the moment that you choose **Delete** in the menu. If Windows does
  not say clearly that Shift is down, Volatus uses the Recycle Bin.
- The dialog shows the name, the path and the size of the item. It also shows the number of files
  and folders, from the scan. A link in the dialog switches between the Recycle Bin and the
  permanent delete.
- In the permanent delete dialog, **Cancel** is the default button. In the Recycle Bin dialog,
  **Move to Recycle Bin** is the default button.
- Delete is not available while a scan runs.

## Recycle Bin and permanent delete

- The Recycle Bin does not free disk space. The space is free only after you empty the Recycle
  Bin.
- The permanent delete frees the space at once. It is fast on folders with many files. See
  [BENCHMARK.md](BENCHMARK.md) for the measured times.
- During a permanent delete, the dialog shows the number of items and the size removed so far.
  **Stop** ends the delete after the item in progress. The items that Volatus already removed stay
  removed.

## What Volatus does not delete

- Volatus does not delete a drive root.
- Volatus does not delete the Windows folder or any item in it.
- Volatus does not delete the Program Files, Program Files (x86), ProgramData and user profile
  folders themselves. It can delete the items inside them, but not inside Windows.
- Volatus deletes items on local drives only. It does not delete network (UNC) paths.
- Volatus does not delete an item that the Recycle Bin cannot take. For example, the item can be
  too big, the Recycle Bin can be turned off, or the shell cannot handle the path. The dialog says
  so. To remove the item, use the permanent delete.
- Volatus removes a junction or a symbolic link as a link. The item that the link points to stays.
- Volatus checks the item again just before it deletes. If the item changed after the scan, the
  dialog asks you to open it again.

## Items that Volatus cannot delete

- Volatus skips an item that another program uses or that you have no permission to delete. It
  deletes the other items. A folder that still holds a skipped item stays.
- At the end, the dialog lists each skipped item with the reason.
- If Volatus skipped items, the sizes in the list can be out of date. The dialog shows **Scan
  again**.
- If an item needs administrator rights, the dialog shows **Restart as administrator**. Then
  delete the item again.

## After a delete

- Volatus updates the scan in place. The item and its size leave the tree, the treemap and the
  parent folders. You do not need to scan again, unless Volatus skipped items.
- While Volatus updates the list after a delete, the dialog can say to try again in a moment.

## Safety of the permanent delete

- The permanent delete uses open folder handles. It does not follow a link. If a folder becomes a
  link during the delete, the delete cannot reach items outside the folder that you chose.
- Limits: the permanent delete tries a newer delete mode first. If the drive does not support it,
  Volatus uses the older mode. This fallback is not tested on FAT32 and exFAT drives or on network
  shares. The safety described above is not verified on network shares.

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
| Graph | Up, Down | Select the next folder band above or below |
| Graph | Right | Expand the selected folder. If it is open, select its first subfolder. With no folder band selected, select the first one |
| Graph | Left | Collapse the selected folder, or select its parent folder |
| Graph | Enter | Make the selected folder the open folder |
| Graph | Backspace | Go up one folder |
| File tree, treemap, graph, crumbs | Menu key or Shift+F10 | Open the context menu of the selected item |
| Settings gear | Enter or Space | Open or close the settings flyout |
| Settings flyout | Esc | Close the flyout |
| Settings flyout | Enter or Space | Choose the item, or turn the switch on or off |
| File tree, treemap, graph | Delete | Delete the selected item to the Recycle Bin, after you confirm |
| File tree, treemap, graph | Shift+Delete | Delete the selected item permanently, after you confirm |
| Delete dialog | Enter | Use the default button |
| Delete dialog | Esc | Cancel or close the dialog. It does not stop a delete that runs |

## Context menu
- Right-click a tile, a frame, a tree row or a crumb. Or select the item and press the Menu key or Shift+F10. Shift+right-click adds the extended
  verbs. The menu works only for a finished scan.
- The menu shows the Volatus items first: Explore Folder, Command Prompt Here, Copy Path, for a folder Show this folder, and Delete. The items of
  Windows for that file or folder follow. They depend on your shell extensions. The first menu can take up to one second while Windows loads them.
- Delete is the Volatus Delete. The Delete of Windows is not in the menu, so there is only one Delete. Volatus asks you to confirm and uses the
  Recycle Bin. Hold Shift when you choose Delete for a permanent delete. All the rules of the [Delete](#delete) section apply. After a delete, the
  item leaves the map and the tree and the sizes change. Volatus does not scan again.
- If the item changed on disk after the scan, the menu shows only "This item changed on disk. Rescan." This also covers a name that ends in a dot or
  a space, or has unusual characters.
- "Files changed after this scan." shows in the top bar when an item of Windows (for example Cut and Paste, or a shell extension) removes or
  changes an item. The sizes can be wrong. Select "Rescan" to
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
    pointer still shows. The tooltips stay. The graph hides the names of its bands too.
- **Startup**: "Always start as administrator". When turned on, Volatus checks whether it has
  administrator rights each time it starts. If not, it requests elevation through Windows UAC and
  restarts. If elevation is declined, Volatus continues running as a standard user.
- **View** is not in the flyout. The **Treemap | Graph** switch in the results top bar sets it. Volatus saves it as the `view` key (`"treemap"` or `"graph"`) in the same file.
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

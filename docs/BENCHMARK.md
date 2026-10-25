# Benchmark: Volatus against WizTree

Definition of Done M3: "MFT scan at least as fast as WizTree on the same machine, measured, not
claimed". This page gives the exact commands. The MFT path reads the raw volume, so the timed
runs need an elevated (Administrator) prompt.

## What is timed

`scan/examples/bench.rs` calls `scan::scan_with`, the same function the app calls through
`scan::scan`. It is not a copy. Each run prints:

| Field | Meaning |
|---|---|
| `open` | Open the volume, read the boot sector and `$MFT` record 0 (MFT path only) |
| `read` | Busy time in `$MFT` reads (MFT) or directory listings (walker) |
| `parse` | Busy time parsing FILE records (MFT only) |
| `rollup` | Link parents and add up folder sizes |
| `total` | Wall time of the whole `scan_with` call, measured by the bench |

On the MFT path a reader thread reads the next chunk while the other threads parse the last
one, so `read + parse` can be more than `total`. That is the overlap working.

```text
cargo run -p scan --release --example bench -- <root> [--runs N] [--method mft|walk]
    [--chunk-mib N] [--threads N] [--csv out.csv] [--json]
```

- No `--method`: MFT, then the walker if the MFT path fails (the app's behaviour).
- `--method mft`: MFT only. Without elevation it stops with "Access is denied" instead of timing
  the walker.
- `--chunk-mib N`: bytes per `$MFT` read (default 32). `--threads N`: rayon pool size (default:
  one per logical CPU for the MFT; one fewer, at least one, at below-normal priority for the walker).
- `--csv out.csv` appends one row per run. `--json` prints one JSON object instead of the table.

## Steps for the user (elevated)

1. In a **normal** (not elevated) terminal, build once. This keeps `target\` owned by your user:

   ```powershell
   cd C:\Users\mynd\DEV\storagetool
   cargo build -p scan --release --example bench
   ```

2. Close WizTree and other programs that use the disk a lot. Keep the PC on mains power.

3. Open **PowerShell as Administrator** and run:

   ```powershell
   cd C:\Users\mynd\DEV\storagetool
   powershell -ExecutionPolicy Bypass -File docs\bench.ps1 -Drive C -Rounds 5 -Sweep
   ```

   The script ([docs/bench.ps1](bench.ps1)):
   - records the CPU, RAM, disk, Windows build, WizTree version and the `$MFT` size
     (`fsutil fsinfo ntfsinfo C:`);
   - times WizTree on a one-file folder 3 times: its fixed cost (start, window, export, exit);
   - does one warm-up run of each tool (not counted);
   - alternates the two tools for `-Rounds` rounds;
   - with `-Sweep`, times chunk sizes 1-64 MiB and thread counts 2-16 (3 runs each).

   It writes `bench-results\<time>\results.txt` and `storagetool.csv`. Paste `results.txt` back.

4. Optional single commands, in the same elevated PowerShell:

   ```powershell
   # storagetool, MFT only, 5 runs, per-phase table
   .\target\release\examples\bench.exe C:\ --method mft --runs 5 --csv bench-results\manual.csv

   # WizTree, timed by PowerShell: full CLI run with a small export (top folders only)
   Measure-Command { Start-Process -FilePath 'C:\Program Files\WizTree\WizTree64.exe' `
       -ArgumentList 'C:', '/export="C:\Users\mynd\DEV\storagetool\bench-results\wiztree.csv"', `
       '/admin=1', '/exportfiles=0', '/exportmaxdepth=1' -Wait } | Select-Object TotalMilliseconds

   # M2 parity on the live volume (MFT totals equal the walker's on a scratch folder)
   cargo test -p scan --test parity -- --ignored
   ```

   (Elevated. Two live tests: the scratch-folder MFT-against-walker parity, and a whole-volume
   MFT scan of the system drive that checks counts and that sizes add up.)

   Pass `C:\` (with the backslash) to the bench: `C:` alone means "the current folder on C:".
   Pass `C:` (no backslash) to WizTree: inside quotes, `C:\"` would escape the closing quote.

## WizTree command line (captured from WizTree 4.31 on this machine)

`WizTree64.exe <drive or folder> /export="<file.csv>" /admin=1 /exportfiles=0 /exportmaxdepth=1`

- `/export=` scans, writes the CSV and exits. No window stays open. The process exit code is 0.
- `/admin=1` uses the MFT path (needs elevation). `/admin=0` walks folders like our fallback.
- `/exportfiles=0` leaves files out of the CSV. `/exportmaxdepth=1` keeps only the root and
  its top folders. Without them a full `C:` export has one row per file (8.6 million rows on
  this machine), and writing it would be part of WizTree's time.
- These options were checked unelevated on a temporary folder and on `C:\Program Files`
  through PowerShell. From Git Bash they are ignored: MSYS rewrites arguments that start with
  `/` as paths. Use PowerShell.
- WizTree's CLI is a GUI program: the time includes its start-up and window. The script
  measures that fixed cost on a one-file folder and prints "WizTree minus fixed". Report both.

## Warm and cold cache

The volume is opened without `FILE_FLAG_NO_BUFFERING`, so `$MFT` reads go through the Windows
file cache. WizTree's reads do too. The first run after a restart reads from the disk (cold);
later runs mostly read from RAM (warm). The script does one warm-up run of each tool and then
alternates them, so both are compared warm.

For a cold number, restart Windows, open an elevated PowerShell and run **one** tool once
(`bench.exe C:\ --method mft --runs 1`). Restart again and run the other once. Do not compare a
cold run with a warm run.

## Timing in the app (`--timing`)

Start the app with `--timing` (before or after the folder). After a scan the banner ends with
`Timing: scan 5.3 s, after scan 0.2 s, total 5.5 s.` and the same line goes to stderr.
"scan" is the scan itself. "after scan" is the time from the moment the worker's scan returns until the treemap is shown, so the wait until the window takes the result is counted (and window start-up too, when a scan ends before the window is up). "total" is the
time from the Scan click. "Restart as administrator" keeps the flag.

## Results

M3 run by the user, 2026-10-22: `docs\bench.ps1 -Drive C -Rounds 5 -Sweep`, elevated. Ryzen 9
7950X3D, 95 GB RAM, Corsair MP600 PRO NH, Windows 11 build 26300, WizTree 4.31. Median of 5
rounds. Log: `.claude/work/storagetool-wiztree-clone/logs/P4-M3-user-run.txt`.

| Date | Drive / disk | Files | storagetool in-process | storagetool wall | WizTree wall | WizTree fixed cost | WizTree minus fixed | Cache |
|---|---|---|---|---|---|---|---|---|
| 2026-10-22 | C: / MP600 PRO NH | 5,068,954 (1,281,226 folders, 0 skipped) | 6,716 ms | 6,845 ms | 23,216 ms | 1,024 ms | 22,192 ms | warm |
| | | | | | | | | cold (not run) |

storagetool is about 3.3 times faster than WizTree on this drive (wall 6.8 s against 23.2 s).
Phases of round 2 (ms): open 520, read 3257, parse 2266, rollup 2700.

Tuning at the 4 MiB default (in-process total, median of 3, ms):

| chunk MiB | 1 | 2 | 4 | 8 | 16 | 32 | 64 |
|---|---|---|---|---|---|---|---|
| total | 10775 | 8496 | 7153 | 6258 | 5584 | 5264 | 5160 |

| threads | 2 | 4 | 8 | 16 | default |
|---|---|---|---|---|---|
| total | 7.2-7.7 s for every value: no effect | | | | |

Default chunk changed from 4 MiB to 32 MiB on this evidence (P4 fix round 2). Chunk size is the
only knob that mattered: 32 MiB is 26% faster than 4 MiB, and 64 MiB gains only another 2%.
The reader keeps 3 buffers, so the default uses 96 MiB while an MFT scan runs. The walker does
not use the chunk.

Caveats:

- Warm cache only. The cold-cache run (restart, one run of each tool) was not done.
- WizTree ran in `/admin=1` MFT mode, so both tools read the whole `$MFT`.
- The WizTree export was `/exportmaxdepth=1`, files excluded, so its time has little CSV writing.
- One machine, one drive, one SSD.

M3 passes when "storagetool wall" is less than or equal to "WizTree wall", warm, on the same
drive. "storagetool in-process" against "WizTree minus fixed" compares the scans alone.
(The table above uses the name from before the rename to Volatus. The tool is the same.)

### The app's `--timing` line over time (elevated, `C:\`, 5.07 M files)

All values are the user's pasted numbers, one run each unless noted. Seconds.

| Date | Build | Run | scan | total | Steps |
|---|---|---|---|---|---|
| 2026-10-22 | P5, chunk default 4 MiB | 1st (cold) | 17.3 | 18.0 | not shown by the app yet |
| 2026-10-22 | P5, chunk default 4 MiB | 2nd (warm) | 7.2 | 7.9 | not shown by the app yet |
| 2026-10-22 | P6, per-step line, 32 MiB | 1st | 9.6 | 10.2 | open 3.7, read 2.2, parse 1.6, rollup 3.5 |
| 2026-10-22 | P6, per-step line, 32 MiB | 2nd | 6.4 | 7.1 | open 0.8, read 2.0, parse 1.4, rollup 3.5 |
| 2026-10-23 | P7, faster roll-up (`volatus.exe`) | 1st | 6.3 | 6.9 | open 2.4, read 2.1, parse 1.6, rollup 1.6 |
| 2026-10-23 | P7, faster roll-up (`volatus.exe`) | 2nd | 5.0 | 5.6 | open 0.4, read 2.1, parse 1.6, rollup 2.4 |

Notes:

- The roll-up step went from about 3.5 s to 1.6 s on the first P7 run. On the second run it was
  2.4 s with the same code. The cause is not known. No measurement explains it.
- The step times do not add up to `scan`. The reader thread reads the next chunk while other
  threads parse the last one, so `read` and `parse` are busy times that overlap.
- The P6 rows are the user's pasted `--timing` lines from the first run of the new per-step line.
- The unelevated walker took 57.1 s in the app (total 57.3 s) and 40.8-43.7 s in the bench
  (see the section below). The two numbers come from different runs and different programs.
  WizTree with `/admin=0` took 217 s (one run).

### Cold start: the first scan after a new exe or a boot

The first elevated scan with a new exe, or after a boot, spends 2-11 s in `open`. A later run
spends about 0.4-0.5 s. The same drop showed in the bench (11.9 s, then 0.5 s, each run a new
process) and in the app (17.3 s, then 7.2 s). The `open` step only does `CreateFile` on the
raw volume, two small reads and the decode of the `$MFT` runs. Disk speed, memory and thread
pool start-up do not explain 11 s (see `.claude/work/storagetool-wiztree-clone/facts-P6.md`).
The cause is **not verified**. A first-time check of the new exe or of the raw volume open by
Windows Defender or another filter driver is the suspect. A probe patch exists in the work
folder (`proofs/P6-open-probe.diff`); it has not been run elevated.

### Live parity tests (elevated)

`cargo test -p scan --test parity -- --ignored` passes on the live `C:\` volume:

| Date | Build | Result | Time |
|---|---|---|---|
| 2026-10-22 | before roll-up speed-up | `parity_live_volume_mft_matches_walker` and `live_whole_volume_mft_scan_is_consistent` ok | 41.8 s |
| 2026-10-23 | after roll-up speed-up | same two tests ok | 14.8 s |

The first M2 run on 2026-10-22 (the scratch-folder test alone, debug build) passed in 62.0 s.

### Limits of these numbers

- One machine, one drive, one SSD. Different disks, file counts or CPUs can change the result.
- Warm and cold caches are mixed in the app rows. Only the 6.7 s against 23.2 s comparison is
  a controlled warm run with alternating tools. The cold run for both tools was not done.
- WizTree ran with `/admin=1 /exportfiles=0 /exportmaxdepth=1`. Its time includes its start-up
  and window (fixed cost 1.0 s).
- Volatus counts a hard-linked file once. WizTree counts each name. Folder sizes and file
  counts therefore differ on purpose, and the unelevated counts (5.0 M against 7.2 M files)
  are not a like-for-like pair.
- The timing numbers are values the user pasted. They were not re-measured for this page.

## Reference numbers without elevation (2026-10-22, builder)

Not M3 evidence (no MFT path), but the same tools on the same machine: Ryzen 9 7950X3D
(32 logical CPUs), 95 GB RAM, Corsair MP600 PRO NH, Windows 11 build 26300, WizTree 4.31.

| Target | storagetool walker (in-process) | WizTree `/admin=0` (wall) |
|---|---|---|
| `C:\Program Files` (824k files) | 1.6-2.4 s warm, 9.7 s first run | 5.1 s (3 runs) |
| one-file folder | 0.03 s | 1.0-2.0 s (fixed cost) |
| `C:\` | 40.8-43.7 s (3 runs; 5.0M files, 1.2M folders, 217 unreadable) | 217 s (1 run; 7.2M files, 1.4M folders) |

The two tools do not count the same items on `C:\` without elevation (5.0M against 7.2M files).
That gap is open (see the P4 report, FOLLOW-UPS); compare the MFT runs: there both tools read the whole
`$MFT`.

## Deviations

- The overlapped `$MFT` read replaced the old read loop with no feature flag (user approved).
  The unchanged P2 record and parity suites run through the new path.
- Only a whole-drive root (for example `C:\`) uses the MFT. A sub-folder is always walked,
  because the MFT path counts a hard-linked file only under its first name. `--method mft`
  still forces the MFT for any root, with no fallback.

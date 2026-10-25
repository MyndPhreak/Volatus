# Volatus: plan

## Summary
A Rust, WizTree-style disk usage analyzer for Windows. Speed comes from reading the NTFS `$MFT` directly from the raw volume (needs Administrator) instead of walking directories. Without elevation it falls back to a parallel directory walk. UI is Slint (Fluent style) with a Mica backdrop, a sortable tree, and a treemap. Built clean-room from public NTFS documentation; WizTree's binary is not decompiled.

## Architectural Decisions
| Decision | Reason |
|---|---|
| Workspace: `scan` (lib), `treemap` (lib), `app` (Slint bin) | Scanner and layout are testable without a GUI. |
| MFT reader opens `\\.\X:`, reads `$MFT` via its own data runs, large sequential aligned reads, record parse in parallel (rayon) | I/O is sequential; parsing is CPU-bound and splits cleanly. |
| Flat arena `Vec<Node>` with `u32` indexes and an interned-name buffer | Millions of files: no per-node allocation, cache-friendly size roll-up. |
| Fallback walker uses `FindFirstFileExW` (large fetch) + rayon | Reuses the platform API; no extra scanning crate. |
| Both scanners emit the same `Tree`; progress via a channel | One consumer path; the UI never knows which scanner ran. |
| Treemap: own squarified layout, drawn into a `SharedPixelBuffer` shown as a Slint `Image` | Thousands of Slint rectangles are slow. One bitmap is fast. |
| Mica: `window-vibrancy::apply_mica` on the HWND, Slint window `background: transparent`, Fluent style, winit backend | Native Windows 11 backdrop, no custom compositor. |
| Elevation: manifest `asInvoker`; a "Scan faster (restart as admin)" button re-launches via `runas` | The app stays usable without admin. |

### Main screen redesign (2026-10-23/24, plan `docs/superpowers/plans/2026-10-23-volatus-main-screen.md`)
| Decision | Reason |
|---|---|
| Treemap is nested, labelled glass frames: `layout_framed` gives each folder a label band and an inset, with its own scale per folder; `render_framed` draws translucent frames and rounded, lit file tiles into one RGBA bitmap on a worker | Folders show as groups, as in the approved mockup. One bitmap stays fast; the UI thread never lays out or draws it. |
| Folder and file names are Slint `Text` labels over the bitmap (largest first, one cap of 650 for both, measured at 4K and 200%) | Crisp text that a frame cannot cut. The cap keeps the label update of a relayout under 50 ms. |
| Tiles under 24 logical px on the short side get cushion shading (bright top left, dark bottom right); bigger tiles keep the glass highlight | Thousands of small tiles stay easy to tell apart. |
| Start screen: one glass button per ready fixed or removable drive (drive list on a worker, `SEM_FAILCRITICALERRORS`), plus "Choose folder" | An empty card reader or a sleeping disk cannot block the window or show a system dialog. |
| `subst` drives are not shown (`QueryDosDeviceW`) | A `subst` drive is a folder of another drive; a button for it looks like a whole drive. |
| Tile colour is a fixed file-type category of the name; the footer shows the biggest types with their share, from a worker pass that is dropped when it is late | The colour is known at the first frame; footer and tiles use the same colours. |
| Hover lift and glow, tooltip, selection outline and zoom are Slint layers over the bitmap; hover changes properties only | Overlays are never cut by a frame, and hover never draws the bitmap again. The software renderer gets a bright border, as it draws no shadow. |
| Motion: open zooms in (180 ms ease-out), up zooms out, the first picture of a scan fades in (250 ms); input never waits for a move | The map change is easy to follow, and a fast user is not slowed. |
| Every name the window shows goes through `display_name`: bidi and other invisible characters become U+FFFD | A file name cannot show other text than it holds (for example a reversed extension). |
| The mimalloc allocator was measured against the start-up stall and not adopted | It met the bar in 0 of 10 runs and used 2.2 to 2.9 times the private memory. |
| With no `--threads`, the walker runs on one thread fewer than the logical CPUs, at below-normal priority; the MFT path is unchanged | The window keeps a CPU to draw while a walk runs. Scan time grows by 0.6% on C:. |
| A folder given on the command line is scanned after the window's first frame (2 s fallback timer); the window then opens on the results screen, not on the start screen | The window draws before the walker takes the CPUs. |
| No runtime flag for the redesign (owner decision) | The owner approved a replacement; rollback is a per-phase `git revert`. |

## Failure behaviour (explicit)
- Volume not NTFS, no admin, or MFT parse error on a record: log, skip that record, count it in `ScanStats.skipped`. If the volume header or `$MFT` run list fails, fall back to the walker and show a banner. Never show partial totals as complete.
- Fixup (update sequence) mismatch: record is dropped and counted. Never trusted.
- Volume changing during scan: result is a snapshot; the UI shows the scan time.
- Nothing is ever deleted by the scanner. Delete actions (later) go to the Recycle Bin only.

## Files
| File | Purpose |
|---|---|
| `Cargo.toml` | Workspace |
| `scan/src/lib.rs` | `Tree`, `Node`, `Scanner` trait, `ScanStats`, progress |
| `scan/src/mft.rs` | Raw volume, `$MFT` runs, record parse, fixups, attribute list, hardlinks |
| `scan/src/walk.rs` | Fallback parallel walker |
| `scan/src/rollup.rs` | Parent linking and size roll-up |
| `treemap/src/lib.rs` | Squarified layout + rasterizer |
| `app/src/main.rs`, `app/ui/main.slint` | Window, tree list, treemap, scan controls, Mica |
| `app/build.rs`, `app/app.manifest` | Slint compile, manifest |
| `docs/USAGE.md` | What the screens do, keys, administrator rights, command line |

## API stubs (land compiling, bodies `unimplemented!`/empty)
```rust
pub struct NodeId(pub u32);
pub struct Node { pub parent: Option<NodeId>, pub name: NameRef, pub size: u64, pub allocated: u64, pub is_dir: bool, pub first_child: Option<NodeId>, pub next_sibling: Option<NodeId> }
pub struct Tree { /* arena */ }
impl Tree { pub fn root(&self) -> NodeId; pub fn children(&self, n: NodeId) -> impl Iterator<Item = NodeId> + '_; pub fn path(&self, n: NodeId) -> PathBuf; }
pub struct ScanStats { pub files: u64, pub dirs: u64, pub skipped: u64, pub elapsed: Duration, pub method: Method }
pub enum Method { Mft, Walk }
pub fn scan(root: &Path, progress: Sender<Progress>, cancel: &AtomicBool) -> Result<(Tree, ScanStats), ScanError>;
pub fn layout(tree: &Tree, root: NodeId, w: u32, h: u32) -> Vec<Rect>;   // treemap crate
pub fn render(rects: &[Rect], w: u32, h: u32) -> Vec<u8>;                 // RGBA
```
No HTTP surface and no persisted documents in v1, so no versioned schema.

## Test Plan
`scan/tests/mft_records.rs` (synthetic MFT record byte fixtures)
- valid record with `$FILE_NAME` + `$DATA` -> name, parent, size parsed
- fixup mismatch -> record rejected, `skipped` +1
- record not in use -> ignored
- resident `$DATA` -> size = resident length
- non-resident `$DATA` -> size from real size field, allocated from allocated field
- `$ATTRIBUTE_LIST` with extension record -> sizes merged into base record
- hardlink with 2 names -> size counted once
- DOS 8.3 name + Win32 name -> only Win32 name kept
- sparse/compressed stream -> allocated differs from size
`scan/tests/rollup.rs`
- nested dirs -> parent size = sum of children
- orphan parent ref -> attached under `$Orphan`, counted
- cycle in parent refs -> terminates, counted as skipped
`scan/tests/walk.rs` (temp dir)
- tree of known file sizes -> totals match
- unreadable dir -> skipped, scan continues
- cancel flag set -> returns `Cancelled`, no partial tree
- junction / symlink -> not followed
`scan/tests/parity.rs`
- same temp tree via walker and via parsed fixture MFT -> equal totals (parity test)
`treemap/tests/layout.rs`
- rect areas proportional to size (within 1 px)
- no overlaps, all inside bounds
- zero-size and single-node inputs -> no panic, no rects for zero size
- deterministic: same input -> same output
`app` tests: **test-first exempt** for Slint UI and Mica. Covered by manual steps M1-M4 below. The live-volume MFT read is also exempt (needs admin and a real disk). Covered by M2, plus a gated `#[ignore]` integration test that compares totals with the walker on a scratch folder.

## Out of scope (v1)
Non-NTFS volumes on the fast path (they use the walker), USN-journal live updates, file deletion/move UI, duplicate finder, network drives on the fast path, installer, macOS/Linux.

## Definition of Done
- [ ] `cargo test` passes for `scan` and `treemap` (named tests above)
- [ ] `cargo clippy -- -D warnings` clean
- [ ] M1: non-admin launch scans a folder via the walker, banner says "Standard scan"
- [ ] M2: admin launch scans `C:` via MFT; totals match the walker on a scratch folder within 0 bytes
- [ ] M3: benchmark on `C:` recorded in README; MFT scan is at least as fast as WizTree on the same machine (measured, not claimed)
- [ ] M4: window shows Mica backdrop and Fluent controls in light and dark theme
- [ ] Cancel button stops a scan in under 1 s
- [ ] Unsupported volume or access denied shows a message, never a crash or silent empty result

## Acceptance Criteria
- Scanning a large NTFS volume with admin rights completes by reading the MFT, not by walking directories.
- The same scan without admin completes with the walker and tells the user why it is slower.
- Folder sizes equal the sum of their contents; hardlinked data counts once.
- The treemap and the tree show the same selection and sizes.

## Phases
1. **Contract + red tests**: workspace, stubs, failing tests (commit 1), then `rollup` + `walk` + `treemap` (commit 2).
2. **MFT fast path**: record parser tests first, then reader, parity test.
3. **Slint UI**: Fluent + Mica window, tree, treemap, scan/cancel, elevation button.
4. **Benchmark + polish**: measure against WizTree, tune read size and thread count.

## Open items
- Slint skill and docs MCP are now loaded (Slint 1.18.1). Fluent is Slint's default style. Mica needs a transparent root `Window` background; the exact Slint hook to get the HWND for `window-vibrancy` is verified in Phase 3 with a throwaway spike before building the UI.

# storagetool: plan

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

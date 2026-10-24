plan-version: 1

# Volatus main screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace "basic lists and squares" with the approved screen: start screen with drive buttons, then tree sidebar + nested, labeled glass treemap + file-type footer, with hover glow, sliding selection and zoom.
**Architecture:** The bitmap stays one RGBA image drawn on a worker (`treemap` crate), now with frame chrome and file-type colours. Everything crisp or interactive (labels, outline, hover glow, tooltip, zoom) is a Slint layer above that image, so nothing in the bitmap can cut it off and hover never redraws it.
**Tech Stack:** Rust 2024, Slint 1.18.1 (Fluent, winit), window-vibrancy 0.8 (Mica), windows-sys 0.61.
**Spec:** `docs/superpowers/specs/2026-10-23-volatus-main-screen-design.md` (read with this plan). Visual reference: `.superpowers/brainstorm/50113-1792785371/content/{layout-v2,style,layout}.html`.

## Global Constraints
- Versions stay: Slint 1.18.1 (`app/Cargo.toml:14,19`), windows-sys 0.61 (`app/Cargo.toml:24`). No new crate. Only new windows-sys features: `Win32_Storage_FileSystem`, `Win32_System_WindowsProgramming`, `Win32_System_Diagnostics_Debug` (P5).
- The `scan` crate does not change. Whole-drive-only MFT routing (`scan/src/lib.rs:176-182`) stays.
- The UI thread never runs layout, render, the file-type pass or drive queries. Stale worker results are dropped by `(generation, layout_seq)` (`app/src/main.rs:216-221`).
- Must keep working: tree keys (`main.rs:440-469`), header sort keys (`main.slint:15-48`), treemap keys (`main.rs:472-485`), focus rings on all three; immediate Cancel (`model.rs:124-132`); async Browse (`main.rs:603-620`); `--timing` line, completed on the first treemap shown (`main.rs:245-253`); "Restart as administrator" only when `offer_elevation` (`model.rs:286-288`); autoscan argument (`main.rs:680-683`).
- Light and dark theme follow the system (`Palette.color-scheme`). Mica fallback stays (`main.rs:673-676`).
- All new UI text is ASD-STE100 (short, active, approved words). Each task lists its strings.
- `treemap::layout` / `render` and their tests stay (spec); the app stops calling them.
- Baseline: `cargo test --workspace` -> 294 passed, 2 ignored (this branch, 2026-10-23). Every phase ends with 0 failed and no new compiler warnings.

## Review Focus
1. **Selection outside the view** (tree click on a node in another branch, or a node too small to draw): the treemap moves its view or outlines the nearest drawn folder; tree and map never disagree. Pinned by P2.T4.
2. **Double-click while a layout is in flight, or a new scan/cancel during drill-in**: the open uses the rects that match the shown image; late layouts and late file-type results are dropped. Pinned by P2.T3 (`open_target` stale-rects line) and P4.T2 (gate = `ScanControl::is_current`, already pinned by the `scan_control_late_events_*` tests in `model.rs`).
3. **200% DPI and a tiny window**: header, pad and labels scale with the factor; a frame too small for a header gets none; a 1x1 map does not panic. Pinned by P2.T1, P3.T1.
4. **Odd names**: no dot, leading dot, trailing dot, upper case, long or non-ASCII extension, very long names: classified the same way every time; labels elide. Pinned by P4.T1, P3.T1.
5. **Empty or odd scans**: drive with no files, folder of only zero-size files, one huge file, drive list empty or denied: the map shows a message or one tile, the footer says so, the start screen still offers "Choose folder". Pinned by P2.T1, P4.T3, P5.T1.
Also pinned: 6 M-node file-type pass time (P4.T2), light-theme palette contrast (P4.T1).

---

## Summary
The scan is fast; the screen is not. This work adds a framed squarified layout (children inside a padded inner rect under a label band), a glass bitmap renderer with rounded, highlighted file-type tiles, a Slint label layer for frames big enough, a file-type footer fed by a worker pass, a start screen with one glass button per ready drive, the results layout (sidebar, breadcrumb, footer), overlays (hover lift/glow, tooltip, sliding outline) and zoom/fade motion. Phase 1 is a throwaway spike that settles renderer behaviour on this machine before any overlay or glass code is written.

## Architectural Decisions
| Decision | Reason |
|---|---|
| `layout_framed` beside `layout`, returning the same `Rect` type | `hit_test`, `selection_bounds`, `neighbour_rect` (`tree_view.rs:132-139`, `nav.rs:60-86`) only need depth-first, nested rects. Their "framed counterparts" are the same functions with new tests on framed fixtures (ponytail rung 2). A new function only if a framed test fails. |
| Chrome is derived, not stored: `Chrome::has_header(&Rect)` / `inner(&Rect)` | No parallel metadata vector; render, labels and tests compute the same answer from the rect. |
| Per-parent scale (`inner area / parent size`) instead of one global scale | Frames remove area, so areas are proportional only among siblings (spec "approximately"). With `Chrome::NONE` the output equals `layout` within 1e-9 (parity test). 1 px pruning kept. |
| Tile colour = fixed file-type category from the name, at draw time | Colour is known for the first frame, without the totals pass (spec: about 1 s). See OD2. |
| File-type totals: one worker pass after `install`, result tagged with scan generation | Off the critical path of the first treemap and the `--timing` line. |
| Labels, outline, hover, tooltip, zoom are Slint elements in one `TreemapView` component (`ui/treemap.slint`) stacked over the `Image` | Crisp text; overlays draw above the whole bitmap by construction; hover changes properties only, never the bitmap. |
| Hover via `TouchArea.pointer-event` kind `move` | `moved` fires only while pressed (`i-slint-core-1.18.1/items/input_items.rs:179-191`). |
| Screen choice is a pure function of `ScanState`; "New scan" = `ScanControl::reset()` Done -> Idle | One source of truth; 5-state table test. |
| Drive list on a worker with `SetThreadErrorMode(SEM_FAILCRITICALERRORS)` | Empty card readers and sleeping disks must not block the UI or show a system dialog. |
| No runtime flag for the redesign | OD1. Rollback is a per-phase `git revert`. |

## Files
| File | Change |
|---|---|
| `treemap/src/lib.rs` | `Chrome`, `Theme`, `layout_framed`, `render_framed`, `label_spots`, `zoom_frame`. `layout`/`render` untouched. |
| `treemap/src/filetype.rs` (new) | `extension`, `Category`, `category_of`, `colour`, `TypeTotal`, `type_totals`. |
| `treemap/tests/framed.rs`, `treemap/tests/filetype.rs` (new) | Layout, render, labels, zoom, file-type tests. |
| `app/src/tree_view.rs` | + `open_target`, `view_for_selection`, `outline_node`, `frame_label`, `crumbs`, `tooltip_lines`; - `drill_target` (+ its 3 tests). |
| `app/src/nav.rs` | - `enter_target` (+ its tests; replaced by `open_target`). |
| `app/src/footer.rs` (new) | `Chip`, `chips`. |
| `app/src/drives.rs` (new) | `DriveKind`, `DriveInfo`, `keep`, `list`, `DriveButton`, `drive_buttons`. |
| `app/src/model.rs` | `ScanControl::reset`, `shows_start`; Idle banner text (test at `model.rs:542-544` updated). |
| `app/src/main.rs` | Glue: relayout, select/open/up, hover, footer pass, drive list, screen flow. |
| `app/ui/main.slint` | Start/results switch, sidebar, top bar, footer; path box, ComboBox, Scan, Up removed. |
| `app/ui/treemap.slint`, `app/ui/start.slint`, `app/ui/theme.slint` (new) | `TreemapView` (map + layers), `StartScreen`, `Glass` global tokens. |
| `app/Cargo.toml` | windows-sys features (P5). |
| `PLAN.md` | Treemap/UI rows in Architectural Decisions (P9). |

## API Stubs & Contracts
Land each phase's signatures as compiling stubs (`todo!()`/empty `Vec`) in that phase's first red commit.
```rust
// treemap (P2)
pub struct Chrome { pub header: f64, pub pad: f64 }            // physical px
impl Chrome { pub const NONE: Chrome; pub fn scaled(scale: f64) -> Chrome /* 16*s, 2*s */;
  pub fn has_header(&self, r: &Rect) -> bool /* header>0 && r.w >= 3*header && r.h >= 2*header */;
  pub fn inner(&self, r: &Rect) -> (f64, f64, f64, f64) /* header: (x+pad, y+header, w-2pad, h-header-pad);
     else if w,h >= 4*pad: pad all sides; else r itself. View root: pad rule, never a header */ }
pub enum Theme { Dark, Light }
pub fn layout_framed(tree: &Tree, root: NodeId, w: u32, h: u32, chrome: Chrome) -> Vec<Rect>;
pub fn render_framed(tree: &Tree, rects: &[Rect], w: u32, h: u32, chrome: Chrome, theme: Theme) -> Vec<u8>; // straight RGBA
pub fn label_spots(tree: &Tree, rects: &[Rect], chrome: Chrome, min_w: f64, cap: usize) -> Vec<Rect>; // P3: header bands
pub fn zoom_frame(target: &Rect, w: f64, h: f64) -> Option<(f64, f64, f64, f64)>;           // P8
// treemap::filetype (P4)
pub fn extension(name: &str) -> Option<&str>;   // 1..=12 ASCII alnum after the last '.', not a leading-dot-only name
pub enum Category { Video, Image, Audio, Archive, Program, Document, Code, DiskImage, GameData, Data, Other }
pub fn category_of(name: &str) -> Category;     pub fn colour(c: Category, theme: Theme) -> [u8; 3];
pub struct TypeTotal { pub ext: String /* lower case, "" = none */, pub bytes: u64, pub files: u64 }
pub fn type_totals(tree: &Tree, root: NodeId) -> Vec<TypeTotal>; // bytes desc, then ext asc
// app
tree_view::open_target(tree: &Tree, node: NodeId) -> Option<(NodeId /*open*/, NodeId /*select*/)>
tree_view::view_for_selection(tree: &Tree, view_root: NodeId, selected: NodeId) -> NodeId
tree_view::outline_node(tree: &Tree, rects: &[Rect], selected: NodeId) -> Option<NodeId>
tree_view::frame_label(tree: &Tree, node: NodeId) -> (String /* "Local\\" */, String /* "306.0 GB" */)
tree_view::crumbs(tree: &Tree, view_root: NodeId) -> Vec<(Option<NodeId>, String)>  // None = "…"
tree_view::tooltip_lines(tree: &Tree, counts: &[u64], node: NodeId) -> (String, String)
footer::chips(totals: &[TypeTotal], total: u64, max: usize) -> Vec<Chip { label, percent, share: f32, category }>
drives::keep(drive_type: u32, ready: bool) -> Option<DriveKind>;  drives::list() -> Vec<DriveInfo>
drives::drive_buttons(d: &[DriveInfo], is_admin: bool) -> Vec<DriveButton { root, title, detail, used: f32, note }>
model::ScanControl::reset(&mut self) -> bool;  model::shows_start(state: &ScanState) -> bool
```
Slint structs (exported from `main.slint`): `FrameLabel { x, y, width, height: length, name, size: string }` (P3), `TypeChip { label, percent: string, share: float, color: color }` (P4), `DriveButton { root, title, detail, note: string, used: float, size-known: bool }` (P5), `Crumb { text: string, index: int }` (P6, index -1 = not clickable). Window properties/callbacks added: `out dark`, `map-empty` (P2); `labels` (P3); `type-chips`, `types-counting` (P4); `drive-buttons`, `drives-loading`, `start-screen`, `show-new-scan`, `scan-drive(string)`, `choose-folder()`, `new-scan()` (P5); `crumbs`, `crumb-open(int)` (P6); `hover-visible`, `hover-x/y/width/height`, `hover-color`, `hover-lift`, `tip-title`, `tip-detail`, `treemap-hover(float, float)`, `treemap-leave()` (P7); `zoom-image`, `zoom-x/y/width/height` (P8). Removed in P5/P6: `drives`, `path`, `scan(string)`, `browse()`, `reset-drive()`, `treemap-title`, `treemap-can-go-up`, `treemap-up()`. No out-of-process clients; no persisted state.

## Invocation paths
- **Open** (`open_node(ui, node)` -> `open_target`): map double-click (`main.rs:544`), map Enter (`main.rs:478`), tree Enter (`main.rs:466`), crumb click (P6). **Up**: map Backspace (`main.rs:479`), crumb click.
- **Select** (`select(ui, node)`, `main.rs:256`, which applies `view_for_selection` + `outline_node`): map click, map arrows, tree click, tree arrows, open/up.
- Each path is a numbered manual step in its phase; the shared funnel functions carry the unit tests. Equivalence: P2 code review checks every path above calls `open_node`/`select` (grep in DoD).

## Failure behaviour
| Dependency | Not configured / empty | Failed | Direction |
|---|---|---|---|
| Drive list (Win32) | No ready drives -> "No drives found. Choose a folder." | `GetLogicalDrives` = 0 -> same; drive not ready -> omitted; space query fails -> button shows "Size unknown." and still scans | Degrade; the scan reports its own error. |
| File-type pass | Zero bytes -> footer "No file sizes to show." | Cannot fail (pure); late result of an old scan dropped by generation | Footer stays "Counting file types." until it arrives. |
| Layout/render worker | Empty rects -> map "No files with a size in this folder." | Late result dropped (unchanged) | Old image stays until the new one arrives. |
| Box shadow on the runtime renderer | n/a | Spike says it does not draw | Spec fallback: brighter border + colour tint, no blur. |
| Mica | n/a | `apply_mica` Err (unchanged) | Opaque Fluent background; glass tokens still readable. |

## Flag
None (OD1): the redesign replaces the screen as the owner approved. Parity anchors instead: `layout_framed(.., Chrome::NONE)` == `layout` (P2.T1); every existing keyboard, Cancel, Browse, `--timing` and elevation behaviour is a manual step in the phase that touches it.

## Framework identifiers (captured in P1, copied into later tests/code verbatim)
Renderer actually used by default (skia, femtovg and software are all in `Cargo.lock`; software draws no box shadow, `i-slint-renderer-software-1.18.1/lib.rs:3059-3065`); `PointerEventKind` on hover without a press; the click sequence of a double-click (`clicked` x2 then `double-clicked`, `input_items.rs:160-163`); `Palette.color-scheme` in light, dark and after a live theme switch; `scale_factor()` with `SLINT_SCALE_FACTOR=2`.

## Test Plan
Standard cycle per task: red commit `Add failing tests for <area>` (stubs + tests; fails on an assertion) -> `cargo test -p <crate> <filter>` shows the listed tests FAIL -> green `Implement <area>` -> `cargo test --workspace` 0 failed -> commit.
```
treemap/tests/framed.rs                                          (P2, P3, P8)
  - zero chrome: same nodes, same order, coords within 1e-9 of layout()  [sample, tiny_files, nested]
  - children lie inside the parent's inner rect; no child in a header band
  - sibling areas proportional to size within one parent (rel. 1e-6)
  - has_header boundaries: h = 2*header-0.01 -> false, = 2*header -> true; w = 3*header-0.01 -> false
  - frame below 4*pad: children fill it with no pad
  - one huge file: one rect = root inner rect;  all-zero sizes -> empty;  w or h 0 -> empty;  1x1 -> no panic
  - folder listed before its descendants; same input -> same output
  - million_files at 1600x900: rect count <= pixel count
  - Chrome::scaled(2.0) -> header 32, pad 4; rects at 2x lie inside 2x inner rects
  - render: no rects -> all transparent; file tile centre alpha 255; header band 0<alpha<255
  - render: nested header alpha > outer header alpha; border pixel lighter than fill (Dark)
  - render: tile >= 12 px has a cut corner pixel; tile top row lighter than bottom row; Light fill != Dark fill
  - label_spots: header + w>=min_w -> one spot (x+pad, y, w-2pad, header); narrower -> none; no header -> none; file -> none
  - label_spots: more than cap -> exactly cap, the largest areas, depth-first order; cap 0 -> empty; 2x -> height 32
  - zoom_frame: full-map target -> (0,0,W,H); target (W/2,H/2,W/2,H/2) -> (-W,-H,2W,2H); zero-size target -> None
treemap/tests/filetype.rs                                        (P4)
  - extension table: "a.TXT"->"TXT", "x.tar.gz"->"gz", "noext"->None, ".gitignore"->None, "file."->None,
    "a.abcdefghijklm"(13)->None, "p.jp g"->None, "日本.txt"->"txt", "x.ÄBC"->None, "a.b-c"->None
  - category_of: one known ext per category, upper case same as lower case, unknown -> Other, no ext -> Other
  - every Category has a distinct colour per theme; Other is grey (r=g=b)
  - every non-Other colour has contrast >= 3:1 vs backdrop #202020 (Dark) and #f3f3f3 (Light)
  - type_totals: sums bytes and files per lower-case ext; ".DLL" and ".dll" one entry; no ext -> ""
  - type_totals: zero-size files counted with 0 bytes; folders never counted; sorted bytes desc then ext asc
  - #[ignore] release: 6 M files, 2,000 exts -> under 2.0 s (prints time)
app/src/tree_view.rs                                             (P2, P3, P6, P7)
  - hit_test on framed sample: header band -> folder; pad gap -> folder; tile -> file
  - selection_bounds / neighbour_rect on framed sample: same answers as on the plain grid fixtures
  - open_target table: folder with content -> (f,f); empty folder -> None; file in sub -> (parent,file);
    file under root -> (root,file); root -> (root,root)
  - open_target with a node outside the current view (stale rects) -> opens that node's folder
  - view_for_selection: descendant -> view; view itself -> view; ancestor -> its parent; cousin folder -> its parent;
    cousin file -> its parent; scan root -> root
  - outline_node: drawn -> itself; pruned -> nearest drawn ancestor; nothing drawn on the chain -> None
  - frame_label: ("Local\\", "306.0 GB"); unicode name kept unchanged
  - crumbs: "C:\" root -> "C:"; folder scan root keeps its full path; 6 levels -> all; 9 levels -> first, "…", last 4
  - tooltip_lines: file -> (path, "12.0 MB. DLL file."); no ext -> "… File with no extension."; folder -> "306.0 GB. 1,234 files."
app/src/footer.rs                                                (P4)
  - top `max` by bytes then "Other" with the rest; no "Other" when all fit; "" ext -> "No extension"
  - percent via format_percent; total 0 -> empty vec; share sums to 1.0 (±1e-6) when total > 0
app/src/drives.rs                                                (P5)
  - keep: 7 drive types x ready -> only FIXED/REMOVABLE and ready kept (14 cases)
  - drive_buttons: label "" Fixed -> "C: Local Disk"; "" Removable -> "E: USB drive"; "Data" -> "D: Data"
  - detail "412.0 GB free of 1.8 TB"; space None -> "Size unknown.", used 0.0; total 0 -> used 0.0; free > total -> 0.0
  - note "Faster as administrator." only when !is_admin; sorted by letter; empty in -> empty out
  - list() includes %SystemDrive%\ (smoke, Windows only)
app/src/model.rs                                                 (P5)
  - shows_start: Idle, Cancelled, Error -> true; Scanning, Done -> false (5 cases)
  - reset: Done -> Idle, true; Scanning, Idle, Cancelled, Error -> false, unchanged (5 cases); generation unchanged
  - banner_text Idle -> "Select a drive or a folder to scan."
```
**Exempt from test-first (visual or Slint glue), covered by numbered manual steps with dark + light screenshots in `.claude/work/storagetool-wiztree-clone/logs/P8-<phase>-{dark,light}.png`:** P1 spike; all `.slint` markup; Win32 calls inside `drives::list`; glue in `main.rs`; animation timing. Keyboard and focus behaviour of new controls is checked by a Tab/Enter/Space walk in each phase (no Slint test backend is set up in this repo).

## Out of Scope
File operations, search, duplicates, settings, colour-by-age (spec non-goals). Deleting `treemap::layout`/`render` (later cleanup). Reduced-motion setting (follow-up). Per-folder footer totals. Network drives. Changes to the `scan` crate.

## Definition of Done
- [ ] D1 `cargo test --workspace`: 0 failed, all Test Plan lines present (P2-P9).
- [ ] D2 First treemap within 1 s of scan end on C: (`--timing` "after scan" <= 1.0 s, user elevated run, M-P4.4).
- [ ] D3 Open/up/select/hover show their first changed frame without a visible wait (M-P7.3, M-P8.2).
- [ ] D4 Nested labeled glass frames, rounded highlighted tiles, hover glow above all frames, both themes (screenshots M-P3.2, M-P7.1, M-P9.3; owner sign-off).
- [ ] D5 Tree and map show the same selection on every select path (P2.T4 tests, M-P2.3).
- [ ] D6 Footer chips: biggest types, percentages, colours equal tile colours (footer tests, M-P4.3).
- [ ] D7 Start screen: one button per ready fixed/removable drive, "Choose folder", Cancel/failure back to it with a message, "New scan" (drives/model tests, M-P5.1-5).
- [ ] D8 Kept behaviours: keyboard + focus rings, immediate Cancel, async Browse, `--timing`, elevation rule, autoscan (M-P5.6-8, M-P6.2).
- [ ] D9 Every new control has an accessible name (M-P9.2 with Narrator).
- [ ] D10 Hover never calls `relayout` (P7 review: grep handler) and the bitmap is not redrawn on hover (M-P7.3).

## Acceptance Criteria
- A1 (D2, D4) After a scan the window shows nested, labeled frames and the tree within about 1 s, and stays responsive.
- A2 (D3) Open, up, select and hover respond at once.
- A3 (D5) A selection made anywhere is shown in both the tree and the map.
- A4 (D6) The footer names the biggest file types with percentages in the tile colours.
- A5 (D7, D8) The window starts on drive buttons; every existing keyboard, Cancel, Browse, timing and elevation behaviour still works.

---

## Phases
Dispatch: **opus/high** for P1 (renderer), P2.T2 (raster perf + alpha), P3 (label layer perf), P4.T2 (6 M-node pass). Others sonnet/medium. P1 must finish before P2.T2; its fallbacks feed P7/P8 (the chief re-plans if a fallback changes a contract).

### P1 Spike: glass, glow, outline, tooltip on the real renderer (throwaway)
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P1.T1 | Answer Q1-Q8, record evidence and fallbacks | probe crate `.claude/work/storagetool-wiztree-clone/probes/p8-spike/` (untracked, not kept); result `logs/P8-spike.md` | exempt (spike) | default renderer unknown |

Q1 default renderer (run default and `SLINT_BACKEND=winit-skia|winit-femtovg|winit-software`). Q2 RGBA image with alpha 15 over Mica: blends, not black (straight vs premultiplied). Q3 `drop-shadow-blur` glow on a lifted Rectangle above the Image, dark + light. Q4 `animate x, y, width, height` outline slide, 120 ms. Q5 hover: `pointer-event` move without press; leave via `has-hover`. Q6 300 `Text` labels in a `for`, model replaced 10x: visible stall? Q7 `Palette.color-scheme` light/dark and live switch. Q8 double-click event order; `SLINT_SCALE_FACTOR=2` scale. Exit: every Q has a screenshot or log line; failed items name the fallback.

### P2 Framed treemap, glass raster, open and selection rules
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P2.T1 | `Chrome`, `layout_framed` | `treemap/src/lib.rs` (after `:51`), `treemap/tests/framed.rs` | framed lines 1-9 | float drift at prune edge |
| P2.T2 | `Theme`, `render_framed`: glass frames (fill, 1 px light border, top highlight), rounded tiles with top highlight, 1 px gap; tile colour = existing hash `colour` (`lib.rs:137`) until P4 | `treemap/src/lib.rs`, `framed.rs` | render lines | opus: alpha blend, 4K time |
| P2.T3 | App draws framed (`main.rs:212-214` -> `layout_framed`/`render_framed`, `Chrome::scaled(scale)`, theme from `out property <bool> dark`, `changed dark` -> relayout); `open_target` replaces `drill_target`/`enter_target` in all open paths via `open_node`; `go_up` selects the folder it left; empty map text | `tree_view.rs:143-152,508-532`, `nav.rs:89-92`, `main.rs:417-437,544-558`, `main.slint`, `treemap.slint` (map moved from `main.slint:314-359`) | hit_test framed, open_target lines | none |
| P2.T4 | `select()` applies `view_for_selection` (relayout if view changes) and outlines `outline_node` | `tree_view.rs`, `main.rs:180-191,256-271` | view_for_selection, outline_node lines | none |

Strings: "No files with a size in this folder." Manual: M-P2.1 C: and a folder scan show frames in dark and light. M-P2.2 double-click folder, file, header band; Enter in map and tree; Backspace. M-P2.3 tree click on a node in another branch: map moves there with outline. M-P2.4 resize to minimum and back; `SLINT_SCALE_FACTOR=2`.

### P3 Frame labels
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P3.T1 | `label_spots` (min_w 40 logical, cap 300), `frame_label` | `treemap/src/lib.rs`, `framed.rs`, `tree_view.rs` | label_spots, frame_label lines | none |
| P3.T2 | `labels: [FrameLabel]` layer in `TreemapView` (11 px, name + grey size, `overflow: elide`); set in `show_layout` (`main.rs:226-241`) in logical px | `main.rs`, `treemap.slint` | exempt (visual) | opus: layer perf |

Manual: M-P3.1 C: labels stack and indent like `layout-v2.html`; small frames unlabeled. M-P3.2 screenshots dark + light. M-P3.3 resize drag on C: stays smooth (debounce unchanged). M-P3.4 a 200-character folder name elides.

### P4 File types: colours, totals pass, footer
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P4.T1 | `filetype` module; `render_framed` colours tiles by `category_of` (hash `colour` no longer used by `render_framed`) | `treemap/src/filetype.rs`, `lib.rs`, `tests/filetype.rs` | extension, category, colour, contrast lines | palette tuning |
| P4.T2 | `type_totals`; app runs it on a worker after `install` (`main.rs:274-291`), result applied only if `scan.is_current(generation)` | `filetype.rs`, `main.rs` | type_totals lines + ignored 6 M test | opus: 6 M names |
| P4.T3 | `footer::chips` (max 8 + Other); footer strip + chips in the current layout; `types-counting` | `app/src/footer.rs`, `main.rs`, `main.slint` | footer lines | none |

Palette (dark, from `layout-v2.html:22`, `style.html:24`): Program #3b8fe6, Archive #e8872b, Data #c9c42a, GameData #2fd6b8, Video #d94b8c, Image #9b5de0, Code #8dbb3a, Document #a3a8f0 (mockup #e0e0e0 is too close to grey), Audio #e5533a, DiskImage #4aa3b0, Other #6e6e6e; chips and hover glow recoloured on `changed dark`; light variants tuned until the contrast test passes. Strings: "Counting file types.", "No file sizes to show.", "No extension", "Other". Manual: M-P4.1 chip colours equal tile colours. M-P4.2 the treemap appears before the footer fills. M-P4.3 screenshots. M-P4.4 user elevated `volatus.exe --timing C:\`: "after scan" <= 1.0 s.

### P5 Start screen and window flow
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P5.T1 | `drives` module; `list()` on a worker at start-up and on each return to the start screen | `app/src/drives.rs`, `app/Cargo.toml`, `main.rs:387-396` (replaces `drive_roots`) | drives lines | sleeping disk delay |
| P5.T2 | `shows_start`, `ScanControl::reset`, Idle banner | `model.rs:86-148,292-294,542-544` | model lines | none |
| P5.T3 | `StartScreen` (glass drive buttons, "Choose folder"); `scan-drive` -> `start_scan`; `finish_browse` starts the scan (`main.rs:623-629`); results top bar: Cancel, "New scan", "Restart as administrator" (path from `scan_root`, `main.rs:586`); path box/ComboBox/Scan removed (`main.slint:131-176`) | `start.slint`, `theme.slint`, `main.slint`, `main.rs` | exempt (glue/visual) | none |

Strings: "Select a drive or a folder to scan.", "Choose folder", "New scan", "Looking for drives.", "No drives found. Choose a folder.", "Size unknown.", "Faster as administrator.", "Local Disk", "USB drive". Manual: M-P5.1 buttons match Explorer's drives. M-P5.2 click C: scans. M-P5.3 Cancel at once -> start screen "Scan stopped. There is no result." M-P5.4 scan of a denied folder -> start screen with the error. M-P5.5 "New scan" -> start screen, memory falls. M-P5.6 Choose folder: dialog does not block the window; cancel changes nothing. M-P5.7 non-admin whole-drive scan offers restart; UAC relaunch rescans with `--timing` kept. M-P5.8 Tab/Enter/Space reach every drive button.

### P6 Results layout and glass theme
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P6.T1 | `crumbs` + crumb bar replacing Up/title (`main.slint:301-313`); `crumb-open(int)` -> `open_node` | `tree_view.rs`, `treemap.slint`, `main.rs` | crumbs lines | none |
| P6.T2 | Sidebar tree (4 columns kept, OD3), map, full-width footer, top bar; `Glass` tokens; window min 960x600 | `main.slint`, `theme.slint` | exempt (visual) | none |

Manual: M-P6.1 matches `layout-v2.html` in both themes. M-P6.2 tree, header and map keys + focus rings as before. M-P6.3 crumbs open ancestors by mouse and keyboard.

### P7 Overlays
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P7.T1 | Hover: `treemap-hover(x,y)` -> `hit_test`; property update only when the node changes; file tile lifts 2 px with glow in its category colour (style.html:21) or the P1 fallback; `treemap-leave()` clears | `main.rs`, `treemap.slint` | exempt (visual); hit_test already pinned | hover rate |
| P7.T2 | Tooltip: `tooltip_lines`, kept inside the map | `tree_view.rs`, `treemap.slint` | tooltip lines | none |
| P7.T3 | Outline slides: `animate x, y, width, height { duration: 120ms; easing: ease-out; }` | `treemap.slint` | exempt | none |

Manual: M-P7.1 glow over a frame edge is not cut. M-P7.2 tooltip flips at map edges. M-P7.3 hovering across C: for 10 s: no relayout (debug counter or `--timing` unchanged), no lag.

### P8 Motion
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P8.T1 | `zoom_frame`; open zooms the old image into the target (180 ms ease-out), up zooms out from the child's rect; input never waits for the animation | `lib.rs`, `framed.rs`, `main.rs`, `treemap.slint` | zoom_frame lines | none |
| P8.T2 | Tiles fade in (250 ms) on the first image of a scan | `treemap.slint`, `main.rs` | exempt | none |

Manual: M-P8.1 zoom in/out looks right at 100% and 200%. M-P8.2 double-click during a zoom and during a relayout: final view is the last folder clicked.

### P9 Polish, accessibility, docs
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P9.T1 | Cushion shading on tiles under 24 px (style.html:14) | `lib.rs`, `framed.rs` (+ small tile top-left brighter than bottom-right; large tile unchanged) | 2 render lines | none |
| P9.T2 | Accessible names: drive buttons ("Scan C: Local Disk. 412.0 GB free of 1.8 TB."), Choose folder, New scan, crumbs, chips, map value = selected name + size | `.slint` files | exempt | none |
| P9.T3 | `PLAN.md` decision rows; final screenshots | `PLAN.md` | n/a | none |

Manual: M-P9.1 contrast in light theme. M-P9.2 Narrator reads every new control. M-P9.3 owner sign-off on screenshots.

## Open Decisions
- **OD1 Runtime flag.** (a) No flag, revert per phase (recommended: the owner approved a replacement; a flag means two screens maintained, as in the P7 ruling). (b) `--classic` keeps the old screen. Cost if wrong: no runtime fallback; old screen only by `git revert`.
- **OD2 Tile colour rule.** (a) Fixed categories from the name (recommended: colour known at draw time, stable across scans). (b) Per-scan ranks of extensions, as WizTree. Cost if wrong: in (a) two extensions of one category share a chip colour (the mockup shows one colour each); switching is P4.T1 plus one relayout when totals arrive.
- **OD3 Sidebar columns.** (a) Keep Name, Size, %, Files, narrower (recommended: no lost sort keys). (b) Name + Size, as the mockup. Cost if wrong: (a) a smaller map; (b) loses two sort columns.

## Self-review
- Spec coverage: decisions, my calls and builder-left items each map to a task (labels P3, palette P4, drives P5, DPI/min size P2/P6, overlays P7, motion P8, cushion/a11y P9).
- Deliberately left to the builder: exact light palette values (bound by the contrast test), tooltip offset, drop-shadow numbers (bound by P1).
- Phase order differs from the brief: labels (P3) come before file types and layout, because labeled groups are the owner's first goal.
- Reuse over counterparts: framed hit/selection/neighbour are the existing functions under new tests; a new function only if one fails.
- Slint glue and markup have no automated tests (no Slint test backend here); each phase lists numbered manual steps.
- Not verified from code: whether `SLINT_SCALE_FACTOR` and the default-renderer choice behave as assumed; P1 Q1/Q8 capture both.

## Spike results (P1, 2026-10-23; full log logs/P8-spike.md in the work folder)
Default renderer is FemtoVG/OpenGL (no Skia): straight RGBA blends over Mica, glow draws only in an unclipped sibling layer (cut inside `clip: true`), outline slides in 120 ms, `pointer-event` move works without a press, 300x2 labels replaced 10x = 19.7 ms median at 4K/200%, no backdrop blur. Changes: P7 must check at run time for the software-renderer fallback (`set_rendering_notifier` returns Err: no glow, use the brighter-border fallback); light-theme frame colours need more contrast (P2.T2); live theme switch to be checked by hand (M-P6.1).

## Decision 2026-10-23 (owner): labels on every frame that can show some text
The owner wants folder names on the frames "even if they get cut off". P3 changes: label every frame whose band can show at least a few characters (start: width >= 24 logical px, band height from `Chrome`), text elided with an ellipsis, largest frames first; replace the cap of 300 with a cap set by measurement (target: label layer update <= 50 ms per relayout at 4K/200%, report the cap chosen; the spike measured 600 elements at 19.7 ms). Frames too small for any text stay unlabeled. Hover and tooltip show the full name.

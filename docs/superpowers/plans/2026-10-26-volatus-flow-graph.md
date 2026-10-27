# Volatus flow graph Implementation Plan

plan-version: 1

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A second results view, **Graph**, that shows the view folder on the left and its folders as size-proportional bands flowing to the right, expandable in place, coloured by age (newest change inside) or by file type, sharing the scan, selection, view folder, context menu and delete flow with the treemap.

**Architecture:** The scan records one 4-byte last-write time per entry (both scanners) and rolls up "newest inside" per folder in the existing roll-up pass. A new pure crate `flow` turns (tree, view folder, expanded set, pixel size) into bands, flows, hit-tests and a picture, exhaustively unit tested without a window. The app adds a pure `graph_view` module (actions, keys, texts, state) and a thin `graph_ui` module (workers, wiring) that reuse the treemap's layout staleness rules, the tooltip machine, `nav::neighbour_rect`, `open_menu` and `scan_apply::request_delete_of`.

**Tech Stack:** Rust 2024 workspace (`scan`, `treemap`, new `flow`, `app`), Slint 1.18.1, `windows-sys` 0.61, `rayon`. No new external crate.

**Spec:** `docs/superpowers/specs/2026-10-26-volatus-flow-graph-design.md` (binding). Also binding: `2026-10-26-volatus-context-menu-design.md` (menu, addendum: one Volatus Delete) and `2026-10-25-volatus-delete-design.md` (delete flow, `Tree::detach`, `Arc<Data>` rules, pending removal).

## Global Constraints

- Rust workspace (`scan`, `treemap`, `app`), Slint 1.18, Windows 11. The menu, the selection model, the delete flow and the `Arc<Data>` rules (workers read the tree; only the UI thread changes it through `Tree::detach` and the pending-removal mechanism) apply unchanged.
- "The scan must not get measurably slower: the MFT scan of the benchmark drive in `docs/BENCHMARK.md` must stay within 5 % of its current time, and memory must grow by no more than 8 bytes per node."
- "Opening or collapsing a folder, and switching views, redraw in under 100 ms on the benchmark scan (excluding the file-type colors, which fill in later without blocking). Reference scans: the benchmark drive of `docs/BENCHMARK.md` and a synthetic 5-million-node tree in tests."
- "Existing behaviour stays: treemap, tree, footer, crumbs, tooltips, settings (units, theme), the start screen, the installer entries."
- "UI text in plain ASD-STE100. Engineering rules: plan first, tests first for pure logic, manual checks for the drawing, deviations recorded. Nothing is pushed without the owner's go-ahead."
- Agents never use the real mouse or keyboard and never steal focus: GUI checks use headless Slint tests (`i_slint_backend_testing`, accessible labels and `invoke_accessible_default_action`) or the owner's numbered manual steps.
- Build and test with `-j 1` (for example `cargo test -p flow -j 1`). Headless UI tests need the Slint debug info: a debug build, or `SLINT_EMIT_DEBUG_INFO=1` (they are `#[cfg_attr(not(slint_debug_info), ignore = ...)]`, as in `app/src/delete_ui.rs:360`).
- Commits: `Add failing tests for <area>` (compiling stubs, fails on an assertion) then `Implement <area>`, each ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Never push without the owner.

## Review Focus

1. **A folder that holds only files** (no subfolders): the graph shows one "files" band to its right, no "other", and the folder has no expander. Pinned: `layout_folder_with_only_files_is_one_files_band_without_expander` (P2.T3).
2. **200,000 sibling folders of a few bytes each:** one "other (200000)" band, no stall, band count bounded by the pixels. Pinned: `split_200k_tiny_siblings_is_one_other_band` (P2.T2) and the release timing test `budget_5m_nodes_layout_and_render_under_100_ms` (P2.T4).
3. **Delete of an expanded folder (or of an ancestor of one):** no id inside the removed subtree stays in the expanded set, the stale graph is never hit-tested, and the graph redraws. Pinned: `remove_from_expanded_folder_drops_it_and_its_descendants_from_the_graph_state` (P4.T4).
4. **Names with unusual characters** (bidi override, trailing dot): band labels, tooltips and the accessible summary show the `display_name` placeholder; the menu still gets the raw `Tree::path`. Pinned: `graph_texts_spoofed_name_shows_a_placeholder` (P4.T2).
5. **A zero-size view folder or tree:** no bands, no panic, the note "No files with a size in this folder." Pinned: `layout_zero_size_view_is_empty` (P2.T3) and `graph_note_zero_size_view_is_no_sizes` (P3.T3).

Also pinned (named in the brief): rescan while expanded (P4.T4 `install_after_expand_starts_with_an_empty_expanded_set`), switch views mid-scan (P3.T2 `shown_view_table`), resize while expanded (P2.T3 `layout_invariants_hold_at_every_size`), dark theme (P2.T1 `age_colour_known_classes_have_3_to_1_contrast_in_each_theme`), unreadable times (P1.T2 `parse_record_without_standard_information_is_unknown_time`, P2.T1 `age_of_unknown_is_unknown`).

---

## Summary

The treemap answers "what is big"; the owner wants a second view of "where the data goes". This plan adds a Sankey-style folder graph as a second view behind a Treemap | Graph switch (Treemap stays the default). It needs one new fact per entry, the last-write time, read by both scanners and rolled up as "newest change inside" at no extra node memory. A new pure crate lays the graph out and draws it; the app shows it with the treemap's worker and staleness rules, and routes selection, menu and delete through the existing code paths. Five stacked PRs: scan times, `flow` layout, drawing and view switch, interaction, docs and verification.

## Architectural Decisions

1. **Time encoding: `u32` whole seconds since 2000-01-01 UTC, `0` = unknown** (`scan::time::UNKNOWN`). A known time before 2000-01-01 00:00:01 maps to `1`, after 2136 to `u32::MAX`. Reason: 4 bytes per node as the design asks, and with unknown as the minimum the roll-up is a plain `max` (unknown loses to any known time).
2. **`Node::modified: u32` costs no node memory.** `Node` is 49 bytes of fields padded to 56 (two `u64`, three `Option<NodeId>` of 8, `NameRef` 8, `bool`); a `u32` fills the padding (53 -> 56). `RawEntry` likewise stays 64. A test pins `size_of::<Node>() <= 64` (the 8-byte limit); the expected value is 56. The roll-up's temporary sums grow from 16 to 24 bytes per entry during the scan only.
3. **Folder value = max of its children's values; a directory record's own time is ignored** (as its own size is, `scan/src/rollup.rs:231`). An empty folder, or one whose files have no time, is unknown. `$Orphan` gets the max of the orphans. Reason: the design's "newest inside, not the folder's own time".
4. **`Tree::detach` leaves ancestors' `modified` as scanned** (documented on `detach`, tested). Recomputing needs a subtree walk per ancestor; the error is bounded by the deleted item's time and a rescan fixes it (design, "After a delete").
5. **MFT time source:** `$STANDARD_INFORMATION` (type `0x10`, resident) value offset `0x08` (last write). A missing, non-resident or short `$SI` gives unknown, never `Malformed`: sizes must stay exact when a time is unreadable. **Walker:** `FILE_ID_BOTH_DIR_INFO::LastWriteTime` (i64, verified in windows-sys 0.61.2). Both go through one function `scan::time::from_filetime(i64)`.
6. **`flow` is a new crate depending on `scan` and `treemap`** (for `Theme`, `Rect`, `filetype::category_of`, `filetype::colour`). Reason: the treemap's split (pure layout crate, tested without a window) is the design's own call; `Rect` reuse lets the app reuse `nav::neighbour_rect`, `tree_view::outline_node` and `selection_bounds` on folder bands.
7. **One global scale per layout** (`Graph::px_per_byte`): every band's height is `size * px_per_byte` exactly, so widths never mislead. A child under 3 % of its own parent (integer test `size * 100 < parent * 3`) **or** under `6 * scale` px merges into "other (N)"; both rules are monotonic in size, so the merged set is always the smallest-N suffix of the size order, which makes `Part::Other { parent, folders, files }` an exact identity (used as a cache key).
8. **Band order in a group:** folders by size descending, ties by `NodeId` ascending, then "files", then "other". Column 0 is the view folder alone, always open. Zero-size children are in no band and not in N.
9. **"other (N)" click** (`open_other` set): for that parent the 3 % rule is dropped, the pixel rule stays (a band is never stretched). Members still under the pixel line stay in a smaller "other". See open point 3.
10. **Columns:** pitch `w / min(columns, 4)` in whole columns; beyond 4 the view scrolls by whole columns (`first_column`), automatically to keep a newly opened column and the selection in view, and by the horizontal wheel or Shift+wheel. The picture is always the viewport size (a full-width picture of 15 columns at 4K would cost about 120 MB).
11. **Shared state:** `Loaded::view_root` and `Loaded::selected` are shared with the treemap; `Loaded::graph: GraphState` holds the expanded set, the opened "other" parents, `first_column`, the colour mode (session only, default Age), the shown `Graph` and the file-type cache. `show_folder` (the single path for every view change, `app/src/main.rs:1728`) calls `GraphState::reroot`, which clears the expanded set, `open_other` and `first_column`.
12. **One layout sequence for both views.** `begin_layout` (`main.rs:659`) bumps `Session::layout_seq` and returns a `LayoutJob` with a `view` field; `relayout` dispatches to `spawn_layout` or `graph_ui::spawn_graph`. A removal (`scan_apply::step_pending` bumps `layout_seq`, `scan_apply.rs:280`) therefore drops a graph result of the old tree too. Every worker that clones `Arc<Data>` drops it and calls `scan_apply::data_released`.
13. **File-type colours are lazy and cached** in `GraphState::types: HashMap<Part, Option<Category>>`, filled by a worker for the bands on screen (`flow::dominant`, one stack walk with `filetype::category_of`), dropped as `Late`/`Recount` with the `store_totals` rules (`main.rs:1490`), cleared on every removal. Until filled, a band gets the `Category::Other` colour.
14. **Guards for menu and Delete:** only a `Part::Folder` band gets a menu or a Delete. Right-click uses the band under the pointer; the Menu key, Shift+F10, Delete and Shift+Delete use the selection **only when it is drawn as its own folder band** (`graph_view::key_target`), else nothing happens. Both feed the existing `open_menu(ui, Source::Map, Some(node), at)` and `scan_apply::request_delete_of`; no menu or delete code is copied.
15. **Remembered view:** `Settings::view: View` with JSON key `"view"` (`"treemap"` | `"graph"`), parsed with the tolerant rules of `settings::parse`. Older builds ignore the key (unknown keys are fine, `settings.rs:129`) and drop it on their next save: the view falls back to Treemap, the conservative default.
16. **No runtime flag** (as PRs #7 to #9): Treemap is the default view, every graph path runs only when the graph is shown, and the scan-time recording is unconditional but gated by the P1 measurement. Needs the owner's confirmation (open point 4). Parity evidence: the existing suites pass with only the mechanical `modified` field additions, plus `begin_layout_treemap_view_is_the_treemap_job_as_before`.

## Files

| File | Change |
|---|---|
| `Cargo.toml` | add `"flow"` to `members` |
| `scan/src/time.rs` (new), `scan/src/lib.rs` | `pub mod time`; `Node::modified`; `detach` doc |
| `scan/src/rollup.rs` | `RawEntry::modified`; roll-up max; `$Orphan` |
| `scan/src/mft.rs` | `Record::modified`; `$SI` parse; extension merge; `entries`; `subtree` |
| `scan/src/walk.rs` | read `LastWriteTime` into `Item` and `RawEntry` |
| `scan/tests/common/{ntfs,reference}.rs` | `standard_information`, `file_at`, `dir_at`; oracle carries `modified` |
| `scan/tests/{rollup,detach,mft_records,walk,parity}.rs` + other `RawEntry {` literals | new tests; mechanical `modified` field (68 literals in 18 files, mostly in helpers) |
| `flow/Cargo.toml`, `flow/src/{lib,age,split,layout,render,types}.rs` (new) | the pure crate |
| `flow/tests/{split,layout,render,types,budget}.rs` (new) | crate tests; `budget.rs` is release-only `#[ignore]` |
| `app/Cargo.toml` | `flow = { path = "../flow" }` |
| `app/src/settings.rs` | `View`, `Settings::view`, parse and save |
| `app/src/graph_view.rs` (new) | pure: `GraphState`, actions, keys, texts, colours, `shown_view` |
| `app/src/graph_ui.rs` (new) | workers, wiring, headless tests |
| `app/src/main.rs` | `Loaded::graph`; `LayoutJob::view`; `relayout` dispatch; view-aware `sync_selection`, `show_folder`; register modules |
| `app/src/scan_apply.rs` | `remove_node` cleans `GraphState` |
| `app/ui/graph.slint` (new), `app/ui/main.slint`, `app/ui/treemap.slint` | `GraphView`; switch, colour toggle, legend; `TipBox` extracted from the treemap tooltip for reuse |
| `docs/USAGE.md`, `README.md`, `docs/BENCHMARK.md` | the graph; scan-time and graph-timing numbers |

CI (`.github/workflows/ci.yml`) needs no change: `--workspace` picks up `flow`.

## API Contracts (land as compiling stubs in each task's red commit)

```rust
// scan/src/time.rs
pub const UNKNOWN: u32 = 0;
pub const FILETIME_2000: i64 = 125_911_584_000_000_000; // 2000-01-01T00:00:00Z in 100 ns ticks
pub fn from_filetime(ticks: i64) -> u32;          // <= 0 -> UNKNOWN; < 2000 -> 1; > 2136 -> u32::MAX
pub fn from_system_time(t: std::time::SystemTime) -> u32; // same rules; for "now" and tests
// scan/src/lib.rs, rollup.rs, mft.rs: one new field each
pub struct Node { /* ... */ pub modified: u32 }     // file: own last write; folder: newest inside
pub struct RawEntry { /* ... */ pub modified: u32 } // a directory's own value is ignored by rollup
pub struct Record { /* ... */ pub modified: u32 }   // $STANDARD_INFORMATION last write or UNKNOWN

// flow/src/lib.rs
pub use treemap::Theme;
pub const MERGE_PERCENT: u64 = 3;   pub const MERGE_PX: f64 = 6.0;    // logical px
pub const VISIBLE_COLUMNS: u16 = 4; pub const BAND_W: f64 = 12.0;     // logical px
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part { Folder(NodeId), Files(NodeId), Other { parent: NodeId, folders: u32, files: bool } }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Expander { None, Collapsed, Expanded }
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band { pub part: Part, pub column: u16, pub x: f64, pub y: f64, pub w: f64, pub h: f64,
                  pub size: u64, pub newest: u32, pub expander: Expander }
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flow { pub from: usize, pub to: usize, pub from_y: (f64, f64), pub to_y: (f64, f64) }
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph { pub bands: Vec<Band>, pub flows: Vec<Flow>, pub columns: u16, pub first_column: u16, pub px_per_byte: f64 }
pub struct Input<'a> { pub tree: &'a Tree, pub view: NodeId, pub expanded: &'a HashSet<NodeId>,
    pub open_other: &'a HashSet<NodeId>, pub w: f64, pub h: f64, pub scale: f64, pub first_column: u16 }
pub fn layout(input: &Input) -> Graph;
pub struct Split { pub folders: Vec<NodeId>, pub files: Option<u64>, pub other: Option<(u32, bool, u64)> } // (folders, files merged, bytes)
pub fn split(tree: &Tree, parent: NodeId, px_per_byte: f64, min_px: f64, keep_small: bool) -> Split;
pub fn members(tree: &Tree, part: &Part) -> Vec<NodeId>;   // folder itself | direct files | merged folders (+ direct files)
pub fn hit(graph: &Graph, x: f64, y: f64) -> Option<usize>; // band index; flows and gaps: None
pub fn folder_rects(graph: &Graph) -> Vec<treemap::Rect>;  // Folder bands only, same geometry
pub fn label_spots(graph: &Graph, min_h: f64, cap: usize) -> Vec<usize>;
pub fn dominant(tree: &Tree, roots: &[NodeId]) -> Option<Category>;
pub fn render(graph: &Graph, colours: &[[u8; 3]], w: u32, h: u32, theme: Theme) -> Vec<u8>; // RGBA
// flow/src/age.rs
pub const RECENT_SECS: u32 = 183 * 86_400; pub const MIDDLE_SECS: u32 = 730 * 86_400;
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Age { Recent, Middle, Old, Unknown }
pub fn age_of(newest: u32, now: u32) -> Age;
pub fn age_colour(a: Age, theme: Theme) -> [u8; 3];

// app/src/settings.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)] pub enum View { #[default] Treemap, Graph }
pub struct Settings { /* ... */ pub view: View }  // JSON "view": "treemap" | "graph"

// app/src/graph_view.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)] pub enum ColourMode { #[default] Age, FileType }
#[derive(Default)] pub struct GraphState { pub expanded: HashSet<NodeId>, pub open_other: HashSet<NodeId>,
    pub first_column: u16, pub colour: ColourMode, pub shown: Option<Graph>, pub types: HashMap<Part, Option<Category>> }
impl GraphState {
    pub fn toggle(&mut self, n: NodeId);  pub fn show_other(&mut self, parent: NodeId);
    pub fn reroot(&mut self);             pub fn after_detach(&mut self, inside: impl Fn(NodeId) -> bool);
    pub fn scroll_to(&mut self, column: u16, columns: u16); pub fn scroll_by(&mut self, delta: i32, columns: u16);
}
pub enum Act { Select(NodeId), ShowOther(NodeId), Nothing }
pub enum KeyAct { Select(NodeId), Toggle(NodeId), Open(NodeId), Up, Delete { node: NodeId, shift: bool }, Nothing }
pub fn click(g: &Graph, hit: Option<usize>) -> Act;
pub fn menu_target(g: &Graph, hit: Option<usize>) -> Option<NodeId>;
pub fn open_target(g: &Graph, hit: Option<usize>) -> Option<NodeId>;
pub fn key_target(g: &Graph, selected: Option<NodeId>) -> Option<NodeId>;
pub fn key(g: &Graph, tree: &Tree, selected: Option<NodeId>, key: &str) -> KeyAct;
pub fn colours(g: &Graph, mode: ColourMode, now: u32, theme: Theme, types: &HashMap<Part, Option<Category>>) -> Vec<[u8; 3]>;
pub fn missing_types(g: &Graph, types: &HashMap<Part, Option<Category>>) -> Vec<Part>;
pub fn band_label(tree: &Tree, b: &Band, unit: Unit) -> (String, String);
pub fn tooltip(tree: &Tree, counts: &[u64], b: &Band, unit: Unit) -> (String, String);
pub fn summary(tree: &Tree, g: &Graph, unit: Unit) -> String;
pub fn format_day(t: u32) -> String;                 // "2026-10-26"
pub fn shown_view(state: &ScanState, loaded: bool, chosen: View) -> View;
pub const AGE_LEGEND: [(Age, &str); 4];
```

No HTTP surface. Persisted shape: `settings.json` gains one optional key (additive; see decision 15). No other contract changes; `Node`, `RawEntry` and `Record` gain a field (in-workspace only).

## Invocation paths of the menu and Delete guard (decision 14)

| Path | Entry | Decision | Existing sink |
|---|---|---|---|
| Right-click on the graph | `graph-menu(x, y, wx, wy)` | `menu_target(g, hit(x, y))` | `open_menu(Source::Map, Some(n), At::Pointer)` |
| Menu key, Shift+F10 in the graph | `graph-menu-key(...)` | `key_target(g, selected)`, box = the band | `open_menu(Source::Map, Some(n), At::Box)` |
| Delete, Shift+Delete in the graph | `graph-key("delete" / "shift-delete")` | `key(...)` -> `KeyAct::Delete` via `key_target` | `scan_apply::request_delete_of(Some(n), shift)` |
| Menu item Delete | (after row 1 or 2) | `Request::node` | `delete_ui::menu_delete` (unchanged) |

All four end in the existing gates (`menu::allowed`, `target_of`'s `in_subtree`, `menu::prepare` disk check, `open_in_for` policy). Equivalence test: `guard_paths_agree_on_every_band` (P4.T1).

## Failure behaviour

| Dependency | Not there | Failed | Direction, reason |
|---|---|---|---|
| Entry time (MFT `$SI`, listing) | no `$SI` | short or non-resident `$SI`; tick <= 0 | degrade: `UNKNOWN`, gray "Date not known"; sizes never lost |
| System clock | n/a | before 2000 | degrade: every known time is "future" -> Recent; no panic |
| `settings.json` `"view"` | missing -> Treemap | bad value -> Treemap, `Partial(["view"])` (existing `.bad` copy rules) | degrade to the default view |
| Graph layout worker | n/a | result late (newer layout, removal, new scan) | dropped (`is_current`), never shown |
| File-type worker | not finished | late or counted before a removal | degrade: `Other` colour; `Late` dropped, `Recount` re-runs |
| `Arc<Data>` held by a graph worker | n/a | removal waits | existing pending removal; every graph worker calls `data_released` |

## Test Plan

Exempt from test-first, with cover: the graph picture's look (curves, colours on screen) is covered by the `render.rs` smoke tests plus manual steps M2 to M5; the Slint markup layout (positions, styling) by the headless tests in P3.T4 and P4.T3 plus manual steps; the scan-time cost and the 100 ms budget on the real drive by the owner's measured steps (P1.T4 gate, M8). Everything else is red/green. TP ids match tasks.

**TP1.1 `scan/src/time.rs` (inline)**
- `from_filetime(0)`, `(-1)`, `(i64::MIN)` -> `UNKNOWN`
- `FILETIME_2000` -> 1 (never `UNKNOWN`); `+10_000_000` -> 1; `+20_000_000` -> 2; `+29_999_999` -> 2 (truncates)
- `FILETIME_2000 - 1` and `1` (year 1601) -> 1
- `FILETIME_2000 + u32::MAX * 10^7` and `i64::MAX` -> `u32::MAX`
- 2026-10-26T00:00:00Z: `from_system_time(UNIX_EPOCH + 1_792_972_800 s)` -> `846_288_000`, and `from_filetime` of the same instant -> the same
- `from_system_time(UNIX_EPOCH)` (1970) -> 1

**TP1.2 roll-up (`scan/tests/rollup.rs`, `detach.rs`, `common/reference.rs`)**
- a folder's `modified` is the max of its files' at three levels of nesting
- a directory entry's own `modified` is ignored (folder older than its own record -> the files' max)
- an empty folder -> `UNKNOWN`; all files unknown -> `UNKNOWN`; known and unknown mixed -> the known max
- `$Orphan` gets the orphans' max and the root includes it
- the seeded-forest suites (`rollup_seeded_*`) equal the oracle, which now carries `modified` (generator sets random times, some `UNKNOWN`)
- `node_size_stays_within_the_8_byte_budget`: `size_of::<Node>() <= 64` (expected 56)
- `detach_newest_file_leaves_ancestor_modified_as_scanned`

**TP1.3 MFT (`scan/tests/mft_records.rs`, `common/ntfs.rs`)**
- `parse_record_standard_information_gives_last_write_time`: `$SI` last write `FILETIME_2000 + 50 * 10^7` -> `modified == 50`
- `parse_record_without_standard_information_is_unknown_time`
- `parse_record_short_standard_information_is_unknown_time_not_malformed` (value of 8 bytes)
- `entries_base_without_time_takes_extension_time`; base time wins over an extension's
- `scan_device_files_keep_their_time_and_folders_roll_up` (image with `file_at`); `scan_device_sub_path_keeps_times` (`subtree` re-root)
- `parse_record_random_mutations_never_panic` also mutates a record with `$SI`
- `entries_seeded_record_sets_equal_the_reference` with `modified` in the oracle

**TP1.4 walker and parity (`scan/tests/walk.rs`, `parity.rs`)**
- `walk_file_time_is_its_last_write_time` (`File::set_modified(T)` -> `from_system_time(T)`)
- `walk_folder_time_is_its_newest_file`; `walk_empty_folder_time_is_unknown`
- `parity_temp_tree_walker_and_mft_image_listings_equal` compares `(path, size, is_dir, modified)`; `encode` writes `$SI` from the file's metadata
- `parity_live_volume_mft_matches_walker` (ignored, elevated) compares `modified` too

**TP2.1 `flow/src/age.rs` (inline)** (`now = 846_288_000`, `D = 86_400`)
- `age_of_unknown_is_unknown`; `now` -> Recent; `now + 10` (future) -> Recent
- `now - 183 D` -> Recent; `now - 183 D - 1` -> Middle; `now - 730 D` -> Middle; `now - 730 D - 1` -> Old; `1` -> Old
- `now = 100, newest = 1` -> Recent (no underflow)
- four classes have four different colours in each theme; Unknown is gray (r == g == b)
- `age_colour_known_classes_have_3_to_1_contrast_in_each_theme` (WCAG, against `#f3f3f3` light and `#202020` dark)

**TP2.2 `flow/tests/split.rs`** (fixture P = 1000: A 600, B 300, C 40, D 25, E 0, files 20 + 15)
- `split_order_is_folders_by_size_then_files_then_other`: k = 1.0 -> folders `[A, B, C]`, files 35, other `(1, false, 25)`
- zero-size E is in no band and not counted in N
- share rule alone (parent 10,000, k = 1.0): child 300 (3 %) shown, 299 merged
- pixel rule alone (parent 100, k = 1.0, min 6): child 6 shown, child 5 merged
- `keep_small` (opened other): child 299 of 10,000 shown; child of 5 px still merged
- files band under the rule -> `files: None`, `other.1 == true`; all children merged -> only other
- one child of 100 % -> one folder, nothing else; only files -> `folders` empty, `files` set, `other` None
- equal sizes order by `NodeId`; `members(Other)` is exactly the merged folders (+ direct files when `files`)
- `split_200k_tiny_siblings_is_one_other_band`: 200,000 folders of 1 byte + one of 10 MB -> folders `[big]`, other N = 200,000

**TP2.3 `flow/tests/layout.rs`** (800 x 600, scale 1 unless said)
- `layout_zero_size_view_is_empty`; `w == 0` or `h == 0` -> empty
- column 0 is the view band alone, `expander == None`, `h == size * px_per_byte`
- `layout_invariants_hold_at_every_size` at 1920x1080, 800x600, 400x300, 300x5 (fixture with two expanded folders): every band `|h - size * k| < 1e-6`; per group `Σ child size == parent size` and `Σ child h == parent h` (1e-6); flows: one per (parent, child), `from_y` spans tile the parent in child order, `to_y == (child.y, child.y + child.h)`; no overlap within a column; all bands in `[0, h]`; `x == (column - first_column) * pitch`; with <= 4 columns every `x + w <= w`
- only "other" bands are under `6 * scale` px; at scale 2 the line is 12 px
- expander: Collapsed iff a folder band has a sized subfolder and is not expanded; Expanded iff expanded; None for column 0, files, other, and folders without sized subfolders
- an expanded folder that is merged or under a collapsed ancestor shows no children; expanded ids outside the view are ignored
- `newest`: folder -> `node.modified`; files -> max of direct files; other -> max of members
- `layout_folder_with_only_files_is_one_files_band_without_expander`
- `first_column = 1` with 5 columns: no band of column 0; `columns == 5`
- `hit`: band centre -> its index; right and bottom edges -> not it; gap and flow area -> None
- `label_spots`: only `h >= min_h`; at most `cap`, the tallest kept, in band order; `folder_rects`: one `Rect` per Folder band, same geometry
- `layout` twice on the same input -> equal

**TP2.4 `flow/tests/types.rs`, `flow/tests/budget.rs`**
- `dominant`: video 100 + image 50 -> Video; tie -> the earlier in `Category::ALL`; zero bytes -> None
- Files roots (direct files) ignore files in subfolders; Other roots union their members
- `dominant_agrees_with_the_footer_totals`: seeded tree, `dominant(tree, [root])` == argmax of `type_totals` folded by `category_of_ext`
- `budget_5m_nodes_layout_and_render_under_100_ms` (`#[ignore]`, release): 5,000,000 nodes, view root, 3 expanded folders, one with 200,000 subfolders; `layout + render` under 100 ms at 1920x1080 scale 1 and 3840x2160 scale 2 (median of 5)

**TP3.1 `flow/tests/render.rs`**
- empty graph -> `w * h * 4` zero bytes; pixel at a band centre == its colour, alpha 255; pixel mid-flow alpha in `1..=254`; pixel in a gap alpha 0

**TP3.2 `app/src/settings.rs`, `graph_view.rs` (inline)**
- no `"view"` -> Treemap, `Ok`; `"graph"` -> Graph; `"treemap"` -> Treemap; `"Graph"`, `1`, `null` -> Treemap, `Partial(["view"])`; `to_json` round trip for both
- `shown_view_table`: 5 `ScanState`s x 2 choices with data loaded (10 cases) + Done without data x 2: Graph only for (Done, loaded, Graph)

**TP3.3 `app/src/graph_view.rs`, `app/src/main.rs` (inline)**
- `begin_layout_treemap_view_is_the_treemap_job_as_before` (parity); graph view -> `view == Graph`, same `seq + 1` rule
- `is_current`: a graph result after a newer layout, after an applied removal, or of another generation -> dropped
- `store_types`: kept for the shown data; `Late` for another scan; `Recount` after a removal
- `colours`: Age mode -> `age_colour(age_of(newest, now))`; FileType with a cached type -> `filetype::colour`; cache miss -> `Category::Other` colour and listed by `missing_types`
- `graph_note_zero_size_view_is_no_sizes` (reuses `tree_view::NO_SIZES`)

**TP3.4 `app/src/graph_ui.rs` (headless)**
- after a finished scan "Treemap" and "Graph" are enabled buttons; while scanning both are disabled (`accessible_enabled`)
- invoking "Graph" sets `Settings::view = Graph` and shows the element labelled "Folder graph"; "Treemap" back
- in the graph: "Age" and "File type" buttons; Age mode shows the four legend texts of `AGE_LEGEND`

**TP4.1 `app/src/graph_view.rs` (inline)**
- `click`/`menu_target`/`open_target` table: Folder(n) -> Select(n)/Some(n)/Some(n); Files(f) -> Select(f)/None/None; Other -> ShowOther(parent)/None/None; None -> Nothing/None/None (12 cases)
- `key_target`: drawn folder -> Some; a file, a merged folder, a folder in a scrolled-out column, no selection -> None
- `key`: up/down -> neighbour folder band; right on Collapsed -> Toggle, on Expanded -> first child folder band, without expander -> Nothing; left on Expanded -> Toggle, else the parent band, on column 0 -> Nothing; enter -> Open; backspace -> Up; delete/shift-delete -> Delete only via `key_target`; arrow with no selection -> first folder band
- `GraphState`: `toggle` adds then removes; `reroot` clears expanded, open_other and first_column; `scroll_to` keeps the column in `[first, first + 4)`; `scroll_by` clamps to `0..=columns - 4`
- `guard_paths_agree_on_every_band`: for each band, `click` then `key_target` equals `menu_target` for Folder bands; Files and Other give no menu

**TP4.2 `app/src/graph_view.rs` (inline)** (fixture `root` 1000: `a` 600 {`a1.bin` 400 at `T`, `s` {`x.mp4` 200 at 1}}, `b` 300 {`y.zip` 300 unknown}, `c.txt` 100 at `T - 200 D`; `T = 846_288_000`)
- `tooltip(a)` -> (`root\a`, `600 B. 60.0% of root. 2 files. Last change: 2026-10-26.`)
- `tooltip(b)` -> `... 1 file. Last change: not known.`; files band of root -> (`Files in root`, `100 B. 10.0% of root. 1 file. Last change: 2026-04-09.`)
- Other -> (`Other folders in <path>`, `<size>. <pct> of <parent>. <N> folders[ and files]. Last change: <day>.`)
- `band_label`: Folder -> `tree_view::frame_label`; Files -> `("files", size)`; Other -> `("other (N)", size)`
- `summary` -> `Folder graph of root: 3 bands, largest a, 600 B.`
- `format_day`: 1 -> `2000-01-01`; 5,097,600 -> `2000-02-29`; 3,160,857,600 -> `2100-03-01`, minus 86,400 -> `2100-02-28`
- `graph_texts_spoofed_name_shows_a_placeholder` (label, tooltip, summary)
- `note_graph_hover`: Folder(a) then Files(a) is a change

**TP4.3 `app/src/graph_ui.rs` (headless, fake delete deps as `delete_ui.rs` tests)**
- a click at a folder band's centre selects it: `Loaded::selected` and the tree's selected row follow
- a tree selection moves the graph outline to that folder's band (or its nearest drawn folder)
- the expander labelled "Expand a" opens `a`; then it is labelled "Collapse a"
- Delete with a drawn folder selected opens the confirm dialog for that folder; with a file selected (from the tree) the dialog stays closed
- `graph_and_treemap_menu_requests_are_equal` (`menu_request(Source::Map, Some(n), fake_disk)`); a right-click on a files band asks for no menu
- Enter on a folder band makes it the view folder for both views and empties the expanded set

**TP4.4 `app/src/scan_apply.rs`, `graph_ui.rs`**
- `remove_from_expanded_folder_drops_it_and_its_descendants_from_the_graph_state`; also `open_other`; `shown` is None and `types` empty after any applied removal
- an unrelated removal keeps the expanded set
- `install_after_expand_starts_with_an_empty_expanded_set` (headless)
- a graph or type worker result landing after `step_pending` applied is dropped

## Out of Scope

Picking, running totals, size targets, multi-item delete, "clean up for me"; animation of flows; any new way to change the tree (only `Tree::detach`); re-computing ancestor times after a delete; remembering the colour mode or the expanded set across runs; a scroll bar for the columns; per-file bands; the graph on the start screen or during a scan; changes to the installer.

## Definition of Done

- [ ] D1 `cargo test --workspace -j 1` passes; every red commit was checked out and failed on an assertion (all TP lines).
- [ ] D2 `cargo clippy --workspace --all-targets -j 1 -- -D warnings` clean.
- [ ] D3 Both scanners record last-write times; parity of `(path, size, is_dir, modified)` holds (TP1.3, TP1.4).
- [ ] D4 `size_of::<Node>() <= 64` (TP1.2) and the owner's A/B MFT bench shows the branch within 5 % of main (P1.T4 gate, numbers in `docs/BENCHMARK.md`).
- [ ] D5 Band heights are exactly proportional, conserved and never stretched (TP2.2, TP2.3).
- [ ] D6 Layout and render of a 5 M-node tree under 100 ms (TP2.4 budget, release) and opening, closing and switching under 100 ms on the benchmark scan (M8).
- [ ] D7 The view switch is remembered and is off (Treemap) by default (TP3.2, M1).
- [ ] D8 Menu and Delete act only on a folder band, through the existing code (TP4.1, TP4.3, M6, M7).
- [ ] D9 After a delete, rescan or re-root no stale id is in the graph state (TP4.4, TP4.1).
- [ ] D10 Light and dark themes are readable (TP2.1, M4); UI text is ASD-STE100 (review of the strings in TP3.4, TP4.2).
- [ ] D11 `README.md`, `docs/USAGE.md` describe the graph; the test count before and after is in the P5 PR.

## Acceptance Criteria

1. The results top bar offers Treemap | Graph; Treemap shows by default; the last choice is used at the next start. (D7)
2. The graph shows the view folder and its folders with widths exactly proportional to size; small folders merge into "other (N)"; files show as one "files" band. (D5)
3. A folder's expander opens and closes its subfolders in place; Enter or a double-click makes it the view folder for both views. (D9, M3)
4. A click on a band selects that folder in the tree and treemap and the other way round. (TP4.3)
5. Right-click, the Menu key and Shift+F10 on a folder band open the existing menu; Delete and Shift+Delete open the existing delete dialog; "files", "other" and empty space open nothing. (D8)
6. Age colours in four classes with a legend, and file-type colours that fill in later without blocking. (D10, D6)
7. No visible stall on the benchmark scan, and the MFT scan is not measurably slower. (D4, D6)

## Phases and task breakdown (one stacked PR per phase)

| Task | Behaviour | Files | Test Plan | Risk signal |
|---|---|---|---|---|
| P1.T1 | time encoding and roll-up | scan time, lib, rollup, tests | TP1.1, TP1.2 | 68 literal edits |
| P1.T2 | MFT reads `$SI` | mft, ntfs fixture | TP1.3 | Malformed vs unknown |
| P1.T3 | walker reads LastWriteTime, parity | walk, parity | TP1.4 | listing time can lag |
| P1.T4 | measure, stop gate | BENCHMARK.md | gate | > 5 % |
| P2.T1 | `flow` crate, age classes | flow age | TP2.1 | none |
| P2.T2 | one group's split | flow split | TP2.2 | 3 % edge |
| P2.T3 | columns, flows, hit, labels | flow layout | TP2.3 | overlap rules |
| P2.T4 | dominant type, budget | flow types, budget | TP2.4 | 100 ms |
| P3.T1 | picture | flow render | TP3.1 | exempt look |
| P3.T2 | remembered view | settings, graph_view | TP3.2 | old builds |
| P3.T3 | layout jobs, colours, types worker | main, graph_view, graph_ui | TP3.3 | staleness |
| P3.T4 | markup: switch, toggle, legend, picture | slint, graph_ui | TP3.4 | none |
| P4.T1 | actions, keys, state | graph_view | TP4.1 | guard paths |
| P4.T2 | texts, tooltip, a11y | graph_view | TP4.2 | ASD-STE100 |
| P4.T3 | wiring | graph_ui, main, slint | TP4.3 | menu reuse |
| P4.T4 | delete, rescan, scan lifecycle | scan_apply, graph_ui | TP4.4 | stale ids |
| P5.T1 | docs, manual steps, counts | docs | M1-M9 | none |

---

### P1.T1: Time encoding and "newest inside" roll-up

**Files:** Create `scan/src/time.rs`. Modify `scan/src/lib.rs:23-32` (`Node`), `:81-84` (detach doc), `scan/src/rollup.rs:31-39,229-268,292-296,355-383`, `scan/tests/common/reference.rs` (oracle and generator), every `RawEntry {` literal (helpers first: `detach.rs:4-10`, `rollup.rs:7-14`, `scan_apply.rs:567`, `main.rs:2413` ...), `scan/src/mft.rs:272,557` (`modified: time::UNKNOWN` for now). Test: `scan/tests/rollup.rs`, `detach.rs`.

**Interfaces:** Produces `scan::time::{UNKNOWN, FILETIME_2000, from_filetime, from_system_time}`, `Node::modified`, `RawEntry::modified`.

- [ ] **Step 1: Red.** Add `time.rs` with stub bodies returning `UNKNOWN`, the two fields (roll-up copies nothing yet: every `modified` is `UNKNOWN`), and the TP1.1 and TP1.2 tests. Key assertions:

```rust
assert_eq!(time::from_filetime(time::FILETIME_2000), 1);
assert_eq!(time::from_system_time(UNIX_EPOCH + Duration::from_secs(1_792_972_800)), 846_288_000);
assert_eq!(t.node(folder).modified, 30);                        // files at 10, 30, 20; dir record at 99
assert_eq!(t.node(empty).modified, time::UNKNOWN);
assert!(std::mem::size_of::<scan::Node>() <= 64, "node grew past the 8-byte budget");
```
Run `cargo test -p scan -j 1`; expect assertion failures in the new tests only. Commit `Add failing tests for entry times`.
- [ ] **Step 2: Implement.** `from_filetime`: `ticks <= 0` -> `UNKNOWN`; `(ticks - FILETIME_2000) / 10_000_000` clamped to `1..=u32::MAX` (negative -> 1). `from_system_time` via `duration_since(UNIX_EPOCH + 946_684_800 s)` with the same clamps. Roll-up: carry `max` next to the `(size, allocated)` sums in `roll_up_sizes` (a directory starts at `UNKNOWN`), `$Orphan` takes the orphans' max. Document on `Tree::detach` that ancestors keep their scanned `modified`. Oracle: the same max rule. Run; expect PASS. Commit `Implement entry times in the tree`.

### P1.T2: MFT reads the last-write time

**Files:** Modify `scan/src/mft.rs:44-55,185-222,245-257,260-273,545-567`; `scan/tests/common/ntfs.rs` (add `standard_information(last_write: i64) -> Vec<u8>`: resident `0x10`, 0x48-byte value, last write at `0x08`; `file_at(parent, name, size, last_write)`, `dir_at(parent, name, last_write)`); `scan/tests/common/reference.rs` (`entries_reference` carries `modified`). Test: `scan/tests/mft_records.rs`.

**Interfaces:** Consumes `time::from_filetime`. Produces `Record::modified`.

- [ ] **Step 1: Red.** Add `Record::modified` (always `UNKNOWN`), fixtures, and the TP1.3 tests (`modified == 50` for `FILETIME_2000 + 500_000_000`). Run `cargo test -p scan --test mft_records -j 1`; expect assertion failures. Commit `Add failing tests for MFT entry times`.
- [ ] **Step 2: Implement.** In `parse_record` add a `STANDARD_INFORMATION = 0x10` arm: resident value of at least 0x10 bytes -> `from_filetime(i64 at 0x08)`, else `UNKNOWN` (never `Malformed`). In `entries`, an extension's time fills a base whose time is `UNKNOWN`; `RawEntry::modified = r.modified`. `subtree` copies `nd.modified`. Run; expect PASS. Commit `Implement MFT entry times`.

### P1.T3: Walker reads the last-write time; parity

**Files:** Modify `scan/src/walk.rs:42-47,51-77,174-181`; `scan/tests/parity.rs:15-53` (`listing` adds `modified`; `encode` uses `file_at`/`dir_at` with the metadata's time as FILETIME). Test: `scan/tests/walk.rs`, `parity.rs`.

- [ ] **Step 1: Red.** Extend `listing` and `encode`; add the TP1.4 walker tests (`File::set_modified`, a fixed `SystemTime`). The walker still records `UNKNOWN`, so the walker tests and the image parity test fail on assertions. Commit `Add failing tests for walker entry times`.
- [ ] **Step 2: Implement.** `Item::modified` from `offset_of!(FILE_ID_BOTH_DIR_INFO, LastWriteTime)` through `from_filetime(i64)`; into `RawEntry` in `walk_dir`. The root entry stays `UNKNOWN` (its value is rolled up). Run `cargo test -p scan -j 1`; expect PASS. Commit `Implement walker entry times`.

### P1.T4: Measure the scan, stop gate

**Files:** Modify `docs/BENCHMARK.md` (a "Scan with entry times" table).

- [ ] **Step 1:** Ask the owner to run, in an elevated PowerShell, on the benchmark machine and drive, alternating builds of `main` and of this branch (`cargo build -p scan --release --example bench` in each checkout, unelevated): `bench.exe C:\ --method mft --runs 5 --csv ...`, 3 rounds each, warm. The baseline is `main` measured in the same session, not the 2026-10-22 table value (open point 7). Also: `cargo test -p scan --test parity -j 1 -- --ignored` (elevated).
- [ ] **Step 2:** Record medians and phase times (`rollup` especially) in `docs/BENCHMARK.md`, and `size_of::<Node>()` (56 expected). Commit `Measure the scan with entry times`.
- [ ] **Step 3: Gate.** If the branch median is more than 5 % over `main`, or the live parity fails, stop and report to the owner with the numbers before P2. Do not tune silently.

### P2.T1: `flow` crate and age classes

**Files:** Create `flow/Cargo.toml` (`scan`, `treemap` path deps), `flow/src/lib.rs` (all P2/P3 contract types and stubs from the API section), `flow/src/age.rs`; modify `Cargo.toml` members. Test: inline in `age.rs`.

**Interfaces:** Produces the `flow` contract surface (stubs: `layout` -> `Graph::default()`, `split` -> empty, `hit` -> None, `dominant` -> None, `render` -> zeros), `Age`, `age_of`, `age_colour`.

- [ ] **Step 1: Red.** Stubs (`age_of` -> `Unknown`, `age_colour` -> `[0; 3]`) and the TP2.1 tests. Run `cargo test -p flow -j 1`; expect assertion failures. Commit `Add failing tests for age classes`.
- [ ] **Step 2: Implement** the boundaries exactly as TP2.1 (`now.saturating_sub(newest)`; future -> Recent). Colours: the builder picks four per theme meeting the TP2.1 tests; Unknown gray near `Category::Other`'s. Commit `Implement age classes`.

### P2.T2: One group's split

**Files:** Create `flow/src/split.rs`. Test: `flow/tests/split.rs` (fixtures built with `scan::rollup::rollup`).

**Interfaces:** Produces `split`, `members` (contract). Consumed by P2.T3, P2.T4.

- [ ] **Step 1: Red.** Write TP2.2. Key assertions:

```rust
let s = split(&t, p, 1.0, 6.0, false);
assert_eq!(s.folders, [a, b, c]); assert_eq!(s.files, Some(35)); assert_eq!(s.other, Some((1, false, 25)));
assert_eq!(split(&t2, p2, 1.0, 6.0, false).other, Some((200_000, false, 200_000)));
```
Commit `Add failing tests for the band split`.
- [ ] **Step 2: Implement.** Sized children only; folder order (size desc, `NodeId` asc); merge when `size * 100 < parent * MERGE_PERCENT` (skipped with `keep_small`) or `size as f64 * px_per_byte < min_px`; the files band takes the same rule. `members` re-derives the merged suffix with the same order. Use `select_nth_unstable_by` or a sort; 200,000 siblings must not allocate per sibling beyond one `Vec`. Commit `Implement the band split`.

### P2.T3: Columns, flows, hit-test and labels

**Files:** Create `flow/src/layout.rs`. Test: `flow/tests/layout.rs`.

**Interfaces:** Consumes `split`. Produces `layout`, `hit`, `folder_rects`, `label_spots`.

- [ ] **Step 1: Red.** Write TP2.3 with a helper `check_invariants(&Graph, &Tree, w, h)` holding every invariant line, called at each size. Commit `Add failing tests for the graph layout`.
- [ ] **Step 2: Implement.** Rules the tests do not fix are the builder's (spacing, where a group sits near its parent, how overlaps push down, gaps), within the invariants: one `px_per_byte` for the whole layout chosen so every visible column fits in `h` with its gaps; column 0 = the view band; column `c + 1` = the split of every band of column `c` that is a Folder in `expanded` (or column 0); `open_other` sets `keep_small`; only columns `first_column..first_column + 4` are emitted; band thickness `BAND_W * scale`. `hit`: left/top inside, right/bottom outside. `label_spots`: `keep_largest` style O(n). Commit `Implement the graph layout`.

### P2.T4: Dominant file type and the time budget

**Files:** Create `flow/src/types.rs`. Test: `flow/tests/types.rs`, `flow/tests/budget.rs`.

**Interfaces:** Produces `dominant`. Consumes `treemap::filetype::{category_of, Category}`.

- [ ] **Step 1: Red.** TP2.4 (budget test `#[ignore = "release timing: cargo test -p flow --release --test budget -- --ignored"]`, building its tree through `rollup`). Commit `Add failing tests for the folder file type`.
- [ ] **Step 2: Implement.** One stack walk over the roots summing `size` into `[u64; 11]` by `Category::ALL` position; argmax, ties to the lower index; all zero -> None. Commit `Implement the folder file type`.
- [ ] **Step 3:** After P3.T1, run the budget test in release; record the medians in the P2 PR. Over 100 ms: stop and report (layout or render to optimise first, with the numbers).

### P3.T1: The picture

**Files:** Create `flow/src/render.rs`. Test: `flow/tests/render.rs`.

- [ ] **Step 1: Red.** TP3.1 against the stub (zeros fail the band-colour assertion). Commit `Add failing tests for the graph picture`.
- [ ] **Step 2: Implement.** Opaque bands; each flow a ribbon between `from_y` and `to_y` (smooth curve, the builder's shape) in the target band's colour at partial alpha; straight RGBA like `treemap::render_framed`'s output. No animation. Commit `Implement the graph picture`.

### P3.T2: Remembered view

**Files:** Modify `app/src/settings.rs` (`View`, `Settings::view`, `parse`, `to_json`); create `app/src/graph_view.rs` (stubs for every `graph_view` contract) and register `mod graph_view;` in `main.rs`.

- [ ] **Step 1: Red.** TP3.2 (`shown_view` stub -> Treemap fails the Done/Graph case; `parse` ignores the key). Commit `Add failing tests for the remembered view`.
- [ ] **Step 2: Implement** `"view"` like `"theme"` (`settings.rs:151-157`), and `shown_view`. Commit `Implement the remembered view`.

### P3.T3: Layout jobs, colours and the type worker

**Files:** Modify `app/src/main.rs:161-180` (`Loaded::graph`), `:649-710` (`LayoutJob::view`, `relayout` dispatch, extract `is_current(generation, seq) -> bool` used by both landings), `:1419-1442` (`install`: `GraphState::default()`), test helpers `loaded()`; create `app/src/graph_ui.rs` (`spawn_graph`, `count_graph_types`, `store_types`); `app/Cargo.toml` (`flow`).

**Interfaces:** Consumes `flow::{layout, render, label_spots, dominant, members}`, `graph_view::{colours, missing_types, band_label}`. Produces `graph_ui::spawn_graph(ui: &MainWindow, job: LayoutJob)`, `graph_ui::store_types(generation: u64, data: &Arc<Data>, edits: u64, found: Vec<(Part, Option<Category>)>) -> Stored`, and window data `GraphLabel { x, y, width, height, name, size }`, `GraphExpander { x, y, size, expanded, name, band: i32 }`.

- [ ] **Step 1: Red.** TP3.3 with stubs. Commit `Add failing tests for graph layout jobs`.
- [ ] **Step 2: Implement.** `spawn_graph` mirrors `spawn_layout`: on a thread, `flow::layout` with `now = time::from_system_time(SystemTime::now())`, `colours`, `render`, labels for `label_spots(graph, FILE_LABEL_MIN_HEIGHT * scale, LABEL_CAP)`; drop the data; land only if `is_current`; always `scan_apply::data_released`. With `--timing`, print `Graph <seq>: <ms> ms.` from job start to landing. In FileType mode, for `missing_types` start one type worker (`members` -> `dominant`), stored by `store_types` (rules of `store_totals`), then `relayout`. Commit `Implement graph layout jobs`.

### P3.T4: Markup: switch, colour toggle, legend, picture

**Files:** Create `app/ui/graph.slint` (`GraphView`: picture, labels, expanders, selection outline, hover box, `TipBox`, focus scope "Folder graph"); modify `app/ui/main.slint` (switch buttons after "New scan", enabled only on a finished scan; a graph bar under the crumbs with "Color:", "Age", "File type" and the legend; the map area shows `GraphView` when `graph-shown`), `app/ui/treemap.slint` (extract the tooltip block at `:499-531` into `export component TipBox`, used by both; no change in look); `graph_ui.rs` wiring of `view-chosen(int)`, `colour-chosen(int)`, `graph-resized`. Test: headless TP3.4 in `graph_ui.rs`.

- [ ] **Step 1: Red.** Callbacks and properties declared, wiring stubs that do nothing; TP3.4 tests. Run `SLINT_EMIT_DEBUG_INFO=1 cargo test -p app -j 1 graph_ui`; expect assertion failures. Commit `Add failing tests for the graph view switch`.
- [ ] **Step 2: Implement** the wiring: `view-chosen` sets `Settings::view`, `persist(ui)`, `drop_hover`, `relayout`; `colour-chosen` sets `GraphState::colour`, `relayout`. Texts: "Treemap", "Graph", "Color:", "Age", "File type", and `AGE_LEGEND`: "Changed in the last 6 months", "Changed 6 to 24 months ago", "Changed more than 24 months ago", "Date not known". In File type mode the footer chips are the legend. Commit `Implement the graph view switch`.

### P4.T1: Actions, keys and graph state

**Files:** Modify `app/src/graph_view.rs`. Test: inline (fixture graphs built by `flow::layout` on small trees).

**Interfaces:** Consumes `flow::{hit, folder_rects}`, `nav::{neighbour_rect, Dir}`. Produces `click`, `menu_target`, `open_target`, `key_target`, `key`, `GraphState` methods.

- [ ] **Step 1: Red.** TP4.1, the 12-case table as one `for` over `[(part, want_click, want_menu, want_open)]`. Commit `Add failing tests for graph actions`.
- [ ] **Step 2: Implement.** Up/Down: `neighbour_rect(&folder_rects(g), selected, dir)`. Right/Left as `nav::right_action`/`left_action` on bands. `key_target`: `selected` is the node of a visible `Part::Folder` band. Commit `Implement graph actions`.

### P4.T2: Texts, tooltip and accessibility

**Files:** Modify `app/src/graph_view.rs`; add `note_graph_hover(part: Option<Part>) -> bool` (and `Session::graph_hovered`) in `graph_ui.rs`.

- [ ] **Step 1: Red.** TP4.2 (exact strings above). Commit `Add failing tests for graph texts`.
- [ ] **Step 2: Implement** with `display_name`, `format_size`, `tree_view::{format_percent, frame_label}`, `group_digits`; `format_day` by the civil-from-days algorithm. Commit `Implement graph texts`.

### P4.T3: Wiring: selection, expand, open, menu, delete, hover, scroll

**Files:** Modify `app/src/graph_ui.rs`, `app/src/main.rs` (`sync_selection` uses `folder_rects` when the graph shows; `show_folder` calls `GraphState::reroot`), `app/ui/graph.slint`, `app/ui/main.slint`. Test: headless TP4.3.

**Interfaces:** Consumes `select`, `open_node`, `go_up`, `open_menu`, `scan_apply::request_delete_of`, `delete_ui::sync`, `tip_event`, `drop_hover`. Callbacks: `graph-click(x, y)`, `graph-open(x, y)`, `graph-menu(x, y, wx, wy)`, `graph-menu-key(x, y, w, h)`, `graph-key(string)`, `graph-expand(int)`, `graph-hover(x, y)`, `graph-leave()`, `graph-scroll(int)`.

- [ ] **Step 1: Red.** Callbacks wired to empty handlers; TP4.3 tests. Commit `Add failing tests for graph wiring`.
- [ ] **Step 2: Implement.** Click: `Act::Select` -> `select`; `ShowOther` -> `show_other` + `relayout`. Expander: `toggle`, `scroll_to(column + 1, columns)`, `relayout`. Keys: `KeyAct` -> `select` / toggle / `open_node` / `go_up` / `request_delete_of(Some(n), shift)` + `delete_ui::sync`; `root.delete-from-map = true` as the map does. Menu: the invocation-paths table. Hover: `note_graph_hover`, `Hover` from `tooltip`, `tip_event`. Wheel (horizontal, or Shift + vertical): `scroll_by`, `relayout`. Commit `Implement graph wiring`.

### P4.T4: Delete, rescan and scan lifecycle

**Files:** Modify `app/src/scan_apply.rs:502-553` (`remove_from` passes `&mut l.graph`; `remove_node` calls `graph.after_detach(inside)`, sets `shown = None`, clears `types`), test helper `loaded()`. Test: `scan_apply.rs` inline, `graph_ui.rs` headless.

- [ ] **Step 1: Red.** TP4.4. Commit `Add failing tests for the graph after a delete`.
- [ ] **Step 2: Implement.** Read every id before `Tree::detach` (as `remove_node` does today). `refresh_after_remove` already calls `relayout`, which now redraws the graph. Commit `Implement the graph after a delete`.

### P5.T1: Docs and verification

**Files:** Modify `README.md` (one line under "Use"), `docs/USAGE.md` (new "Graph" section after "Results screen"; "Keyboard" and "Settings" lines), `docs/BENCHMARK.md` (graph timing from M8).

- [ ] **Step 1:** Write the docs in ASD-STE100: the switch, expand and open, "files" and "other", the colours and legend, the keys, that the date is the newest change inside, that a delete leaves the parents' date as scanned until a rescan, and that times come from the directory listing without administrator rights. Commit `Document the flow graph`.
- [ ] **Step 2:** Record `cargo test --workspace -j 1` counts before (P1 base) and after in the PR (1,192 `#[test]` functions at plan time).
- [ ] **Step 3: Manual steps for the owner** (results pasted into the PR):
  1. M1 Start Volatus: Treemap shows. Choose Graph, close, start again: Graph shows after the scan. Scanning: the switch is unavailable.
  2. M2 Scan a folder with a few big and many small subfolders: bands, "other (N)", "files"; widths look proportional; nothing stretched.
  3. M3 Open and close a folder with its expander and with Right/Left; open 6 levels: the view scrolls by columns; Shift+wheel scrolls back.
  4. M4 Light and dark theme (Settings): bands, flows, labels and legend are readable.
  5. M5 Color: File type: colours fill in without a freeze; they match the footer chips.
  6. M6 Right-click a folder band: the real menu for that folder. Right-click "files", "other" and empty space: no menu. Menu key and Shift+F10 on a selected band: menu under the band.
  7. M7 Delete and Shift+Delete on a temp folder band: the existing dialog; after the delete the band leaves, parents shrink, the expanded state of other folders stays.
  8. M8 Elevated, `--timing`, whole `C:\`: open, close, switch views ten times; every `Graph <seq>` line under 100 ms.
  9. M9 Hover a band: the tooltip with path, size, share, files and date; it hides on click and scroll.

---

## Spec-to-task mapping

| Design item | Task |
|---|---|
| Goal 1 switch, default, shared scan/selection/view | P3.T2, P3.T4, P4.T3 |
| Goal 2 bands, 3 % "other (N)", "files" band | P2.T2, P2.T3 |
| Goal 3 expand in place, Enter/double-click, shared crumbs | P4.T1, P4.T3 |
| Goal 4 shared selection, Delete, Shift+Delete | P4.T1, P4.T3 |
| Goal 5 right-click, Menu key, Shift+F10, nothing on empty space | P4.T1, P4.T3 |
| Goal 6 age 3 steps + legend, file type + same legend, toggle, default age | P2.T1, P2.T4, P3.T3, P3.T4 |
| Goal 7 tooltip (name, size, share, files, age) | P4.T2, P4.T3 |
| Goal 8 5 M files without a stall, off-thread types | P2.T4, P3.T3, M8 |
| Non-goals (no picking, no animation, delete only via `Tree::detach`) | Out of Scope, P4.T4 |
| Anti-goals (exact widths, act on what is under the pointer, no freeze, no deleted folder shown, one selection) | P2.T3, P4.T1, P3.T3, P4.T4, P4.T3 |
| Constraints: timestamps both scanners, 4 bytes, roll-up newest inside | P1.T1 to P1.T3 |
| Constraints: 5 % and 8 bytes | P1.T1, P1.T4 |
| Calls: `flow` crate, worker picture, staleness, view shared, expanded set cleared | P2, P3.T3, P4.T4 |
| Calls: 4 columns then scroll, no expander without subfolders | P2.T3, P4.T3 |
| Calls: keyboard, remembered view, lazy types, availability, accessibility | P4.T1, P3.T2, P3.T3, P3.T4, P4.T2 |
| Cold read: view folder, sizes (`Node::size`), threshold per parent or 6 px | P2.T2, P2.T3, P4.T3 |
| Cold read: "files"/"other" rules and colours | P2.T2, P4.T1, P4.T2, P3.T3 |
| Cold read: timestamps, unknown class, after a delete | P1.T1 to P1.T3, P2.T1 |
| Cold read: settings, budget, focus and keys, summary | P3.T2, P2.T4, P4.T3, P4.T2 |

## Plan deviations and open points

1. **Walker times can differ from `$STANDARD_INFORMATION`.** The design says both sources "give the same meaning". The listing's `LastWriteTime` comes from the directory index's copy, which NTFS may update later than `$SI` (for example while a file is open for writing, or for another hard-link name). Not verified on this machine; the live parity test (P1.T4 Step 1) is the check. Plan: parity on closed files only; USAGE states the limit.
2. **Node memory: 0 bytes, not 4.** `Node` keeps its size (decision 2). If `size_of` shows 64 on the toolchain, it is still within the 8-byte limit.
3. **"A click on other (N) shows its members as normal bands"** cannot hold for members under 6 px without stretching (an anti-goal). The plan drops only the 3 % rule for that parent; the smallest members stay in a smaller "other". Owner to confirm.
4. **No runtime flag**, following PRs #7 to #9 (owner-approved there). Needs the owner's confirmation for this feature, including the unconditional scan-time recording.
5. **Delete and the Menu key act only on a selection drawn as its own folder band.** A file or a merged folder selected from the tree gives nothing in the graph (the design's "selected band" does not exist then). Stricter than a literal reading; chosen so the outline and the target never differ.
6. **File-type legend** is the existing footer chips (the treemap's legend). If the owner hides the strip and chips in Settings, the graph has no file-type legend.
7. **5 % baseline.** `docs/BENCHMARK.md` holds 6,716 ms in-process from 2026-10-22, before the P7 roll-up change (later `--timing` runs show 5.0 to 6.3 s). Comparing against that stale value would be meaningless; the plan measures `main` against the branch A/B in one session. The measurement needs elevation, so only the owner can run it.
8. **"6 months" and "24 months"** are 183 and 730 days.
9. **Columns scroll in whole columns**, with no scroll bar (keys, the horizontal wheel, Shift+wheel, and automatic scrolling to a newly opened column).

## Self-review

- Spec coverage: every Goal, Call and Cold-read item maps to a task (table above); no gap found.
- Steps: each red step names its tests (TP ids with exact values); each green step names the signature and the rules the tests do not fix. Left to the builder on purpose: band spacing and overlap pushing, curve shape, palettes (bounded by TP2.1), label placement inside `label_spots`.
- Names checked across tasks: `Part`, `Band`, `Graph`, `Input`, `split`, `members`, `layout`, `hit`, `folder_rects`, `label_spots`, `dominant`, `render`, `Age`, `age_of`, `age_colour`, `GraphState`, `ColourMode`, `View`, `shown_view`, `key_target`, `menu_target`, `store_types`, `is_current`, `spawn_graph`.
- Review Focus: each of the five has a named test in its owning task.
- Open on purpose: the owner's confirmations in open points 3, 4 and 5; the A/B scan numbers (P1.T4 gate) and the release budget numbers (P2.T4 Step 3) are measured, not assumed.

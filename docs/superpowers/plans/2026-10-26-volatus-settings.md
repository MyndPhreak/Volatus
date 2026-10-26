# Volatus settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A gear button opens a right-side flyout where the user picks the theme (Follow Windows, Light, Dark), the unit sizes show in (Dynamic, Byte, KB, MB, GB, TB), and which window parts show; the choices are saved and applied at start.

**Architecture:** A pure `app/src/settings.rs` holds the `Settings` values, the tolerant JSON load and the atomic save. `format_size` takes a `Unit`, and every call site passes the unit from the session settings. A new `app/ui/settings.slint` flyout binds to window properties; the host applies each change live, saves it, and re-syncs the parts that show sizes.

**Tech Stack:** Rust 2024, Slint 1.18, `serde_json` (moves from dev-dependency to dependency; no `serde` derive, fields are read by hand), `window-vibrancy` (already used).

**Spec:** `docs/superpowers/specs/2026-10-26-volatus-settings-design.md`. Read it with this plan: the "(my call)" decisions and the "Calls made after the builder check" are there.

## Global Constraints

- Defaults equal today's behaviour: theme Follow Windows, unit Dynamic, every part shown. A parity test proves it; that is why no feature flag is added (spec, Constraints).
- Unit base is **1024**; labels `KB`, `MB`, `GB`, `TB` as today. No 1000 choice in v1.
- A fixed unit shows one decimal; a non-empty size below 0.05 of the unit shows `< 0.1 KB` (with the unit's label), never `0.0`. Byte shows a whole number with thousands separators.
- The gear is never hideable. Settings file: `%APPDATA%\Volatus\settings.json`. A missing, empty or damaged file gives defaults; a bad field falls back alone; an unknown field is ignored; a file that was not read cleanly is kept as `settings.json.bad` before any save.
- Save is a synchronous atomic write (temp file in the same folder, then rename). A failed save shows "Settings could not be saved." in the flyout; the setting stays applied for the session.
- "Labels on tiles" means the name text on tiles and folder frames. Hover and tooltips stay on.
- "Header" hides the scan title and summary text only. The buttons (Cancel, New scan, Restart as administrator) and the gear stay, and the top bar stays 52 px high so the map keeps its size (existing rule in `main.slint`). (my call: the spec leaves where the gear sits to the builder; hiding the buttons would make a running scan impossible to cancel.)
- UI text follows ASD-STE100. Commits: `Add failing tests for <area>` (with stubs, failing on an assertion), then `Implement <area>`; both end with the Co-Authored-By line from the session.
- No test reads or writes the real `%APPDATA%`. File tests use a temp directory the test makes.

## Review Focus

1. A settings file saved by Notepad (UTF-8 with a BOM) loads; it is not treated as damaged.
2. A fixed unit and a 0-byte or 1-byte file: `0.0 MB` for 0 bytes, `< 0.1 MB` for 1 byte; `u64::MAX` in Byte does not overflow or panic.
3. The settings folder cannot be made or the file is read-only: Volatus starts, the setting works for the session, the flyout says it was not saved.
4. Every part hidden, on the results screen and on the start screen: the gear still opens the flyout and Esc closes it.
5. The folder tree is hidden while a tree row has the keyboard focus: focus moves to the map, and the arrow keys still move the selection.

## Files

| File | Change |
|---|---|
| `app/src/settings.rs` | new: `Settings`, `ThemeChoice`, `Part`, load, save, path |
| `app/src/model.rs` | `Unit`; `format_size(bytes, unit)`; scan summary takes a unit |
| `app/src/footer.rs`, `app/src/drives.rs`, `app/src/tree_view.rs` | pass the unit; `Chip` keeps `bytes` and not a formatted `size` |
| `app/src/main.rs` | load at start, apply, save, re-sync; theme and Mica |
| `app/ui/settings.slint` | new: the flyout and the gear button |
| `app/ui/main.slint`, `app/ui/start.slint` | gear, flyout, hide switches, properties and callbacks |
| `app/Cargo.toml` | `serde_json` to `[dependencies]` |
| `docs/USAGE.md`, `README.md` | settings section, one line in README |

## API Contracts (land as compiling stubs in each task's red commit)

```rust
// app/src/model.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Unit { #[default] Dynamic, Byte, Kb, Mb, Gb, Tb }
impl Unit { pub const ALL: [Unit; 6]; pub fn from_index(i: i32) -> Unit; pub fn index(self) -> i32; }
pub fn format_size(bytes: u64, unit: Unit) -> String;

// app/src/settings.rs
pub enum ThemeChoice { System, Light, Dark }            // from_index/index like Unit
impl ThemeChoice { pub fn forced_dark(self) -> Option<bool>; }   // System None, Light Some(false), Dark Some(true)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part { Header, Tree, Strip, Chips, Labels }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings { pub theme: ThemeChoice, pub unit: Unit, pub show_header: bool, pub show_tree: bool,
                      pub show_strip: bool, pub show_chips: bool, pub show_labels: bool }
impl Settings { pub fn shown(&self, p: Part) -> bool; pub fn set_shown(&mut self, p: Part, on: bool); }
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded { Fresh, Ok, Partial(Vec<&'static str>), Damaged }
pub fn parse(text: &str) -> (Settings, Loaded);          // pure; strips a leading BOM
pub fn to_json(s: &Settings) -> String;
pub fn load(path: &Path) -> (Settings, Loaded);          // reads; copies the file to `.bad` on Partial/Damaged
pub fn save(path: &Path, s: &Settings) -> std::io::Result<()>;
pub fn settings_path() -> Option<PathBuf>;               // %APPDATA%\Volatus\settings.json
```

## Task 0: Theme spike (throwaway, no tests)

**Goal:** learn how to force and release the colour scheme from Rust, and what Mica needs.

- [ ] **Step 1:** On a scratch branch, export `Palette` from `main.slint`, and from `main()` set `ui.global::<Palette>().set_color_scheme(ColorScheme::Dark)` while Windows is Light. Check the tiles, the chips (`ui.get_dark()`), and that `on_theme_changed` fires.
- [ ] **Step 2:** Find how to return to "Follow Windows": try `ColorScheme::Unknown`. If that does not follow the system again, the fallback is to read `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme` and re-read it on a `WM_SETTINGCHANGE`; Negotium does this (`~/DEV/negotium/crates/app/src/platform.rs`, `apply_theme_with`). Read it, do not import from it.
- [ ] **Step 3:** Check that `window_vibrancy::apply_mica(&handle, Some(dark))` re-applied after a change gives a dark backdrop under a forced dark theme (today `None` is passed, `app/src/main.rs:1437`).
- [ ] **Step 4:** Write the findings (what works, what Task 4 must do) in a new "Spike result" section at the end of this plan, commit only that section, and throw the scratch code away.

## Task 1: Settings values and the settings file

**Files:** Create `app/src/settings.rs`; modify `app/Cargo.toml` (`serde_json` to `[dependencies]`; keep the version already in `[dev-dependencies]`), `app/src/main.rs` (`mod settings;`).
**Produces:** the `settings.rs` contracts above.

- [ ] **Step 1: Red commit.** Stubs for the contracts, and tests in `settings.rs`:
  - `default_settings_are_todays_behaviour`: `Settings::default()` is System, Dynamic, all five parts shown.
  - `parse_roundtrips_to_json`; `parse_empty_text_is_damaged_and_default`; `parse_not_json_is_damaged`; `parse_json_that_is_not_an_object_is_damaged`.
  - `parse_wrong_type_in_one_field_keeps_the_others`: `{"theme":"dark","unit":7,"show_tree":false}` gives Dark, Dynamic, tree hidden, `Partial(vec!["unit"])`.
  - `parse_unknown_value_names_the_field`: `{"theme":"purple"}` is `Partial(vec!["theme"])`, default theme.
  - `parse_unknown_field_is_ignored` and `parse_older_file_without_a_field_uses_its_default` (both `Loaded::Ok`).
  - `parse_utf8_bom_is_stripped` (Review Focus 1).
  - `load_missing_file_is_fresh_and_default`; `load_damaged_file_is_kept_as_bad` and `load_partial_file_is_kept_as_bad` (the `.bad` holds the original text).
  - `save_then_load_returns_the_same`; `save_creates_the_folder`; `save_replaces_an_existing_file`; `save_failure_leaves_the_old_file` (make the target a directory, or the folder a file, in the temp dir; assert `Err` and that the old file is intact; Review Focus 3).
  - `forced_dark_maps_each_choice`; `set_shown_and_shown_agree_for_every_part`.
  Run `cargo test -p app settings` and check that each test fails on its assertion, not on a missing symbol.
- [ ] **Step 2: Green commit.** Implement. `parse` reads the text as `serde_json::Value`, then each field by hand (no derive). JSON keys: `theme` (`"system"|"light"|"dark"`), `unit` (`"dynamic"|"byte"|"kb"|"mb"|"gb"|"tb"`), `show_header`, `show_tree`, `show_strip`, `show_chips`, `show_labels` (booleans). `save` writes `settings.json.tmp` next to the file, then `std::fs::rename`. Run `cargo test -p app settings` and `cargo clippy --workspace --all-targets -- -D warnings`.

## Task 2: `format_size` with a unit

**Files:** Modify `app/src/model.rs` (`Unit`, `format_size`, `group_digits` stays).
**Consumes:** nothing. **Produces:** `Unit`, `format_size(bytes, unit)` (the call sites change in Task 3).

- [ ] **Step 1: Red commit.** Stub `Unit` and the new signature; the existing `format_size` tests move to `Unit::Dynamic` and must still pass (parity). New failing tests:
  - `format_size_dynamic_matches_the_old_values` (every pair in the existing known-values test).
  - `format_size_fixed_units_known_values`: 1536 bytes is `1.5 KB` in Kb; `1.0 MB` for 1 048 576 in Mb; `2,048.0 GB` for 2 TiB in Gb; `1,610.6 MB` for 1.5 GiB in Mb.
  - `format_size_byte_shows_a_whole_number_with_separators`: 1 234 567 in Byte is `1,234,567 B`; `u64::MAX` in Byte does not panic (Review Focus 2).
  - `format_size_tiny_non_empty_file_in_a_big_unit`: 1 byte in Mb is `< 0.1 MB`; 51 bytes in Kb is `< 0.1 KB`; 52 bytes in Kb is `0.1 KB`; 0 bytes in Mb is `0.0 MB`.
  - `unit_index_roundtrip_and_unknown_index_is_dynamic`.
- [ ] **Step 2: Green commit.** Implement `Unit::from_index/index/ALL` and the fixed formats; Dynamic is the old body unchanged.

## Task 3: Every size obeys the unit

**Files:** Modify `app/src/model.rs` (summary banner text, `banner_text`), `app/src/footer.rs` (`Chip.size: String` becomes `bytes: u64`), `app/src/drives.rs` (detail line), `app/src/tree_view.rs` (`frame_label`, `file_label`, `tooltip_lines`, `map_value`, `row_spoken` input), `app/src/main.rs` (row data at `main.rs:99`, `type_chip`).
**Consumes:** `Unit`, `format_size(bytes, unit)`. **Produces:** each of those functions takes a `unit: Unit`; the window passes `SESSION.settings.unit` (Task 4 adds the field; until then pass `Unit::Dynamic`).

- [ ] **Step 1: Red commit.** Change the signatures with `Unit::Dynamic` behaviour and add tests where a fixed unit must show through: `frame_label_uses_the_unit`, `file_label_uses_the_unit`, `tooltip_lines_use_the_unit`, `map_value_uses_the_unit`, `chips_bytes_are_kept_and_formatted_by_the_window` (`Chip.bytes`), `drive_detail_uses_the_unit_for_free_and_total` (a 2 TB drive in Byte shows full numbers; the card elides, see Task 5 step 3), `banner_text_done_uses_the_unit`. Tests for the Dynamic path are the existing ones, edited only to pass `Unit::Dynamic`. Each new test fails because the stubs ignore the unit.
- [ ] **Step 2: Green commit.** Thread `unit` to each function; `type_chip` formats `Chip.bytes`. Run the whole workspace tests: nothing else changes.

## Task 4: Gear, flyout, theme and units, live and saved

**Files:** Create `app/ui/settings.slint`; modify `app/ui/main.slint`, `app/ui/start.slint` if the gear needs a slot there, `app/src/main.rs`.
**Consumes:** Tasks 0 to 3. **Produces:** window properties `settings-open`, `theme-choice: int`, `unit-choice: int`, `settings-error: string`, callbacks `theme-chosen(int)`, `unit-chosen(int)`, `part-toggled(int, bool)`; host functions `apply_settings(&MainWindow)`, `persist(&MainWindow)`.

- [ ] **Step 1: Red commit.** Pure pieces first, in `main.rs`: `resync_after_unit_change` is the list of syncs (rows, types, selection, banner, drives); test `unit_change_resyncs_every_size_site` against a function `fn sites_after_unit_change() -> &'static [Site]` that names them, so a missing site fails a test. Add `Session.settings: Settings` and `Session.settings_path: Option<PathBuf>`; test `session_starts_with_the_loaded_settings` and `a_failed_save_keeps_the_setting_and_reports_it` against `persist_with(&mut Session, save_fn)` where `save_fn` is a closure (the seam; a real `save` is tested in Task 1). Stubs fail on assertion.
- [ ] **Step 2: Green commit, host.** At start: `settings::load(settings_path())`, put it in the session, `apply_settings`. `theme-chosen` sets `settings.theme`, applies it as the spike found (force with `Palette.color-scheme`, or release it for Follow Windows), re-applies Mica with `Some(dark)`, and `persist`. `unit-chosen` sets the unit, `persist`, then runs the syncs. `persist` calls `save`; on `Err` set `settings-error` to "Settings could not be saved.", on `Ok` clear it. The existing `on_theme_changed` stays: it redraws the map and chips when `dark` changes.
- [ ] **Step 3: Green commit, UI.** `settings.slint`: a `GlassPanel` flyout, 300 px wide, over the right edge below the top bar, with a "Theme" group (Follow Windows, Light, Dark), a "Units" group (Dynamic, Byte, KB, MB, GB, TB) and the "Show" group of Task 5. The gear is a 32 px button at the window's top-right, above both the start screen and the results screen; the top bar keeps an empty 40 px at its right end so the summary text never sits under the gear. Esc closes the flyout; the gear toggles it. Labels in plain ASD-STE100 words ("Settings", "Theme", "Units", "Show"). A drive card that is too narrow elides its text with `overflow: elide` (Task 3's note on a 2 TB drive in Byte).
- [ ] **Step 4: Verify by running the app.** Run `cargo run -p app`, then check each item and write down what you saw in the commit body: gear opens and closes the flyout on the start screen and on a scanned folder; Light, Dark and Follow Windows change the UI, the tiles and the chips and survive a restart; each unit changes the tree, tile hover, footer chips, summary and drive cards; a file made unreadable (damaged JSON) starts with defaults and leaves `settings.json.bad`.

## Task 5: Show or hide the parts

**Files:** Modify `app/ui/main.slint`, `app/ui/settings.slint`, `app/src/main.rs`.
**Consumes:** Task 1 `Part`, Task 4 callbacks. **Produces:** properties `show-header`, `show-tree`, `show-strip`, `show-chips`, `show-labels` on `MainWindow`.

- [ ] **Step 1: Red commit.** Pure tests in `main.rs` against `fn footer_height(strip: bool, chips: bool) -> f32` (0 when both hidden, 48 otherwise) and `fn focus_after_hide(part: Part, focus: Focus) -> Focus` (hiding the tree while the tree has focus gives Map; Review Focus 5); `show_flags_follow_settings`: `fn show_flags(&Settings) -> [bool; 5]` (the values `apply_settings` writes to the five `show-*` properties) is right for all 32 combinations set through `set_shown`. Stubs fail on assertion.
- [ ] **Step 2: Green commit.** In `main.slint`: the scan title and summary text get `visible: root.show-header`; the sidebar `GlassPanel` gets `visible: root.show-tree` and its `min-width`, `max-width` and the gap it leaves are 0 when hidden (do not use `if`: the tree must keep its scroll position, see the comment above it); the footer strip row and the chip row each get their own `if`, and the footer panel `height` follows `footer_height`; the treemap takes `labels: root.show-labels ? root.labels : []` and the same for `file-labels`. In `main.rs`, `part-toggled` calls `set_shown`, `persist`, and `focus_after_hide`; the layout change reaches the map through the existing `treemap-resized` path.
- [ ] **Step 3: Verify by running the app.** Hide each part alone and then all five, on a scanned folder and on the start screen. The map fills the room freed by the tree and the footer; the gear, Esc, Cancel during a scan, and the arrow keys on the map still work; after a restart the same parts are hidden. Write down what you saw in the commit body.

## Task 6: Docs

**Files:** Modify `docs/USAGE.md` (a "Settings" section: where the gear is, the three groups, the file path and that a damaged file is kept as `.bad`), `README.md` (one line).

- [ ] **Step 1:** Write the section in ASD-STE100 style, from what Task 4 and 5 showed on screen. Commit as `Document the settings`.

## Test Plan

Pure logic (Tasks 1, 2, 3, and the seams in 4 and 5) is covered by the named tests. The Slint layout and the live theme change have no automated test in this repo; Tasks 4 step 4 and 5 step 3 are the stated exemption, and each is run by hand and recorded in a commit body. `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` pass at every green commit.

## Out of Scope

Icon style, resizable sidebar and treemap, the update toggle, drive-info toggle, the 1000/1024 base, language, "Run as administrator", the Explorer-menu option, colour pages, Slint style switching. The delete feature (`docs/superpowers/plans/2026-10-25-volatus-delete.md`) also edits `main.slint` and `main.rs`: merge order is a human call, and whichever lands second resolves the overlap.

## Definition of Done

- The gear opens the flyout on the start and results screens; theme, units and the five parts all work live and are the same after a restart.
- Every size in the window obeys the unit; Dynamic output is byte-for-byte what it was.
- A damaged or unreadable settings file starts Volatus with defaults and leaves `settings.json.bad`; a failed save is shown in the flyout.
- `cargo test --workspace` and clippy are clean, and Tasks 4 and 5 each have the hand-run result in a commit body.

## Acceptance Criteria

1. With Windows in Light, choosing Dark makes the UI, the treemap tiles, the footer chips and the backdrop dark; choosing Follow Windows returns to Light.
2. With unit GB, a 700 MB file shows `0.7 GB` in the tree, the tooltip and the chips; with Byte it shows its full byte count with separators.
3. With the folder tree hidden, the treemap is wider, and the arrow keys on the map still change the selection.
4. With all five parts hidden, Cancel works during a scan and the gear still opens the flyout.
5. A `settings.json` holding `{"theme": 5}` starts Volatus with the default theme and the other fields, and leaves the old file as `settings.json.bad`.

## Spike result

Task 0 was run on 2026-10-26 with a scratch build (Slint 1.18.1, window-vibrancy 0.8.1, winit 0.30.13, Windows 11 build 26300). The scratch code is discarded. Windows was in **Dark** during the run (`AppsUseLightTheme` = 0); Windows Light was not tried, because the spike must not change the owner's Windows settings. Everything below was seen in log output unless marked "not verified".

**What works**

- Rust can force both schemes while Windows is Dark. `export { Palette }` in `main.slint`, then `ui.global::<Palette>().set_color_scheme(slint::language::ColorScheme::Light)` (and `::Dark`) compiles and takes effect at once: `Palette.color-scheme`, `ui.get_dark()` and `on_theme_changed` all follow. `on_theme_changed` fires once for each real flip of `dark` (and not when the forced value equals the current one). A public function on `MainWindow` that assigns `Palette.color-scheme = ColorScheme.dark` works the same way; the export is the shorter route.
- Before any forcing, and once the event loop runs, `Palette.color-scheme` read `Dark` with Windows Dark. Before the event loop runs (right after `show()`), it reads `Unknown` and `get_dark()` is false: Slint receives the system scheme during the first loop turn, and `on_theme_changed` fires once then. Windows Light was not observed; the mapping is symmetric in `winitwindowadapter.rs` (winit `Theme::Light` gives `ColorScheme::Light`).
- `window_vibrancy::apply_mica(&handle, Some(dark))` returned `Ok(())` at runtime, repeatedly, from a timer callback after the event loop started. `DwmGetWindowAttribute` read back `DWMWA_USE_IMMERSIVE_DARK_MODE` as 1, then 0 after `Some(false)`, then 1 after `Some(true)`; the backdrop type (`DWMWA_SYSTEMBACKDROP_TYPE`) stayed 2 (Mica). `None` leaves the dark attribute as it was. The Mica call is therefore also the title-bar dark-mode call: nothing else is needed for the title bar. Only the owner's eyes can confirm how the backdrop and title bar look.

**What does not work**

- `ColorScheme::Unknown` does **not** release the override. After it, `Palette.color-scheme` read `Unknown` and `get_dark()` was false while Windows was Dark. Assigning the property replaces its system binding, and Slint has no way to restore it.

**How to follow Windows after forcing, and how a live change is seen**

- Do not release. Always write `Palette.color-scheme` from Rust, with `dark = match choice { Light => false, Dark => true, Follow => system_dark }`.
- `system_dark` comes from winit, which keeps tracking Windows while `Palette` is forced: with `slint = { features = ["raw-window-handle-06", "unstable-winit-030"] }`, `use slint::winit_030::WinitWindowAccessor;` then `ui.window().with_winit_window(|w| w.theme())` returned `Some(Some(Theme::Dark))` at every step, even while `Palette` was forced Light. Before the event loop starts it is `None`. No registry read is needed.
- A live Windows change: `ui.window().on_winit_window_event(|_, e| ...)` (same trait) receives `winit::event::WindowEvent::ThemeChanged(theme)`; Slint's filter runs for every window event (`winitwindowadapter.rs:1258`), and winit sends `ThemeChanged` from its `WM_SETTINGCHANGE` handler only when the theme really changed. This is the smallest reliable signal. **Not verified live** (it needs a real Windows theme flip, which the spike must not do); the owner flips Windows once in Task 4 step 4. Fallback if it fails: a 1 s `Timer` that reads `with_winit_window(|w| w.theme())` and compares with the last value, or the registry value `HKCU\...\Themes\Personalize\AppsUseLightTheme`.
- Winit's `WM_SETTINGCHANGE` handler resets the title bar to the system theme before it sends `ThemeChanged` (`dark_mode.rs`, `try_theme`). So the handler must always re-apply the effective theme and Mica, even in Light or Dark mode.

**Code recipe for Task 4**

1. `app/Cargo.toml`: add `"unstable-winit-030"` to the `slint` features. `main.slint`: `export { Palette }` above `MainWindow`.
2. One host function, `fn apply_theme(ui: &MainWindow, choice: ThemeChoice)`: `let system_dark = ui.window().with_winit_window(|w| w.theme()).flatten() == Some(slint::winit_030::winit::window::Theme::Dark);` (an unknown system theme: keep the current value), `let dark = match choice {..};`, then `ui.global::<Palette>().set_color_scheme(if dark { ColorScheme::Dark } else { ColorScheme::Light });`, then `window_vibrancy::apply_mica(&ui.window().window_handle(), Some(dark))` with the existing `set_mica(false)` on `Err`.
3. Call `apply_theme` in the existing `invoke_from_event_loop` block that applies Mica today (`main.rs` ~:1445), once the window exists, with the loaded setting; and from `theme-chosen`.
4. Register `on_winit_window_event` there too: on `ThemeChanged`, queue `Timer::single_shot(Duration::ZERO, ...)` that calls `apply_theme(ui, current_choice)`. The existing `changed dark` and `on_theme_changed` then redraw the map and chips.

**Changes to the plan's Task 4**

- Step 2 says to "force with `Palette.color-scheme`, or release it for Follow Windows". Release is impossible; replace with "always write `Palette.color-scheme` from `apply_theme`, with Follow Windows reading the system theme through `with_winit_window(|w| w.theme())`".
- Step 2 passes `Some(dark)` to `apply_mica`: confirmed, keep it, and call it on every live system change too.
- Add the `unstable-winit-030` feature to the "Files" list (`app/Cargo.toml`); run `cargo tree -p app -d` and check `Cargo.lock` for new crates when adding it.
- Add `fn effective_dark(choice, system_dark) -> bool` as a pure function with a test (Light and Dark ignore the system; Follow returns it), as Negotium's `settings::effective_dark` does.
- Step 4 (run by hand) must add: with the app in Dark or Light mode, flip Windows once and check that the UI follows only in Follow Windows, and that the title bar and backdrop keep the chosen mode. This is the one item the spike could not prove.

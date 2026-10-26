# Volatus settings: design

Date: 2026-10-26. Status: draft for approval. Written after a conversation with the owner. Lines marked **(my call)** are the writer's defaults or guesses, not the owner's words.

## Intent

Volatus has no settings and saves no preference. The theme follows Windows, sizes always show in the unit that fits, and every panel is always on. The owner wants a Settings section to show or hide parts of the window, to choose the theme, and to choose how sizes show. The owner likes WizTree's menu (Display Dynamic / Byte / KB / MB / GB / TB values; Show Header, File Types, Treemap, Folder and File Names on Treemap).

This is project 1 of 5 from the owner's wishlist. The order the owner agreed: **1 Settings** (this document), 2 Icons, 3 Resizable sidebar and treemap, 4 Update checking, 5 Drive info next to the scan section. Later projects add their own settings to the page this project builds. They are not part of this one.

## What the owner decided

- Settings opens as a **flyout panel on the right** from a gear button. The map stays visible, so a change shows at once. (The owner took the writer's recommendation over a dialog and a full page.)
- **Theme:** the owner wants a choice of theme.
- **Unit display:** like WizTree: Dynamic, or one fixed unit.
- **Show or hide:** header, folder tree, file-type strip, footer chips, labels on treemap tiles. (The owner said "yeah those you listed" to the writer's list of the parts in the window.)
- Slint widget styles (fluent, material, cupertino) are **not** a setting. The owner asked whether they can be switched. They are fixed at build time, and Volatus uses only Button, ProgressIndicator, ListView, ScrollView and Palette from the widget set; the rest is the custom Glass look in `app/ui/theme.slint`. The owner accepted the writer's advice to skip it.

## Goals

1. A gear button opens and closes a right-side flyout. Esc closes it. The flyout does not block the map.
2. **Theme:** Follow Windows (default), Light, Dark. It applies live, to the Slint UI and to the treemap tiles and footer chips that are drawn with `treemap::Theme` (`app/src/main.rs` reads `ui.get_dark()` in each place).
3. **Units:** Dynamic (default; today's behaviour of `format_size` in `app/src/model.rs`), or fixed Byte, KB, MB, GB, TB. A fixed unit applies to every size the window shows: tree rows, tile tooltips and hover, footer chips, the drive screen, the scan summary. **(my call)** A fixed unit that makes a small size round to `0.0` shows `< 0.1` of that unit, never a plain zero for a non-empty file.
4. **Show or hide** each of: header, folder tree, file-type strip, footer chips, labels on treemap tiles. The treemap itself stays; with the folder tree hidden the map takes the room. **(my call)** Everything is shown by default.
5. **Saved** to a JSON file, read at start, written when a setting changes. A missing, empty or damaged file gives the defaults. A field that is wrong falls back to its default and the other fields still load. The program never overwrites a file it could not read without keeping the old one as `settings.json.bad`. **(my call)** The file is `%APPDATA%\Volatus\settings.json`.
6. The settings model (`Settings`, defaults, load, save, the unit rule) is plain Rust with unit tests. The Slint side only binds to it.

## Non-goals and anti-goals

- Not in this project: icon style (project 2), sidebar and treemap size (project 3), the update toggle (project 4), drive-info toggle (project 5), language, "run as administrator", "Windows Explorer context menu" (the installer owns that), colors and tweaks pages.
- No Slint style switch.
- No per-drive or per-folder settings.
- Anti-goals: a setting that changes the screen but not what is saved, or saves but is not applied at start; a fixed unit applied in some places and not others; a hidden panel the user cannot get back (the gear button always stays); a damaged settings file that stops Volatus starting, or that is silently replaced; a hide option that stops the scan or the keyboard moves from working.

## Constraints

- Rust workspace (`app`, `scan`, `treemap`), Slint 1.18, Windows 11. UI is in `app/ui/main.slint`, `start.slint`, `treemap.slint`, `theme.slint`. Sizes format in `app/src/model.rs` (`format_size`, binary 1024 steps with `KB`, `MB` labels) and `app/src/drives.rs` (`412.0 GB free of ...`).
- `Palette.color-scheme` from `std-widgets.slint` is how `Glass` reads dark or light (`app/ui/theme.slint`). The builder checks that the window can set it from Rust to force Light or Dark, and records the result. If it cannot, the builder tells the owner before choosing another way.
- No new dependency without a reason. `serde` and `serde_json` are acceptable if not already used. **(my call)** Negotium (`~/DEV/negotium/crates/app/src/settings.rs`) is a worked example of the same file rules (defaults, partial load, atomic save); the builder may read and mirror it but must not import from it.
- Engineering rules: plan first, tests first for the pure logic, UI text in ASD-STE100 plain style, no silent loss of anything. A new setting that changes behaviour is on by default only because every default equals today's behaviour (a parity test proves it); this is recorded as the reason no flag is added.

## The unit rule

- **Base: 1024, labels KB, MB, GB, TB**, as today and as Explorer shows. **(my call)** The writer proposed a 1000 / 1024 choice. It is **not** in v1: today's behaviour is 1024, WizTree has no such choice, and the owner did not ask for it. It is a one-line follow-up if wanted.
- Dynamic: unchanged from today (parity test over the existing known values).
- Fixed: bytes divided by the unit, one decimal, except Byte, which shows a whole number with thousands separators. **(my call)** Format of the number follows the existing `format_size` style.

## Calls made after the builder check

All are **(my call)**; the owner can change any of them when reading this.

- **Drive screen:** drive capacity, used and free space follow the unit setting like every other size. Percentages do not change. Byte on a large drive shows the full number with separators; a number that does not fit its card is shortened with an ellipsis, never wrapped or clipped without a mark. Dynamic stays the default, so this only shows if the owner picks a fixed unit.
- **"Labels on treemap tiles"** means the **name text drawn on tiles and folder frames** (WizTree: "Folder and File Names on Treemap"). Hover, tooltips and the selection stay on. Size text on a tile, if any exists today, follows the same switch only if it is drawn by the same label code; the builder records which.
- **The gear is never hideable.** It sits outside every part the settings can hide. If the header holds it today, hiding the header leaves the gear in a minimal strip or floating at the window edge; the builder picks which. Keyboard Esc and the gear both reopen and close the flyout.
- **Theme spike first.** Plan a short first task that proves whether Rust can set `Palette.color-scheme`. If it cannot, the fallback is a Volatus-owned dark flag in the `Glass` global that all Volatus UI reads instead of `Palette.color-scheme`. The owner does not need to be asked; the builder records the result.
- **Saving:** a synchronous atomic write (write a temp file, then rename), on the change. The file is small. A failed save shows one plain message in the flyout ("Settings could not be saved.") and the setting stays applied for the session.
- **Dependencies:** whether `serde` is already in `app/Cargo.toml` is for the builder to check; adding it needs one line of reason in the plan.

## What is left to the builder

- The flyout's exact look and animation, in the existing Glass style. The Slint component split. How the gear reaches the user by keyboard (**my call**: a shortcut such as Ctrl+, is welcome, not required).
- How Follow Windows detects a change in the Windows theme while Volatus runs (today's behaviour is the reference; it must not get worse).
- Where the settings code lives (**my call**: a new `app/src/settings.rs`).
- The names of the fields in the JSON file, as long as they are plain and an unknown field is ignored (a newer file read by an older Volatus).

## Test ideas (the builder writes the plan)

- Defaults equal today's behaviour: Dynamic units, Follow Windows, everything shown.
- Load: missing file, empty file, not JSON, wrong type in one field (the others keep their values), unknown field, older file without a newer field.
- Save then load returns the same settings; a failed save is reported and the old file is kept.
- Every fixed unit over known sizes (0, 1, 1023, 1024, a size just below a unit step, a very large value), including the `< 0.1` rule.
- Hidden parts: the layout still works with each part hidden and with all hidden.

## Definition of done

- The gear opens the flyout; each setting works live and is still set after a restart.
- All five parts hide and show; with the folder tree hidden the map fills the room.
- Light, Dark and Follow Windows all work, on the UI, tiles and chips.
- Every size in the window obeys the unit setting.
- A damaged settings file starts Volatus with defaults and keeps the old file as `.bad`.
- Tests pass, and the owner has seen the app run with the flyout.

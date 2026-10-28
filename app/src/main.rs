//! Volatus window: Mica backdrop, scan controls and the result banner.
//!
//! Mica recipe (proved by a spike, see `.claude/work/storagetool-wiztree-clone/logs/P3-mica-spike.md`):
//! the root `Window` background starts transparent; `slint::Window::window_handle()` only yields
//! a handle after the winit window exists, so `apply_mica` runs from `invoke_from_event_loop`
//! queued before the event loop starts. Calling it right after `show()` returns `NotSupported`.
//! If Mica is not available (Windows 10), the window falls back to the opaque Fluent background.

// Release builds are GUI-subsystem so no console window opens; debug builds keep theirs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browse;
mod cli;
mod delete;
mod delete_flow;
mod delete_ui;
mod drives;
mod elevate;
mod footer;
mod graph_ui;
mod graph_view;
mod menu;
mod model;
mod motion;
mod nav;
mod purge;
mod recycle;
mod scan_apply;
mod shell;
mod settings;
mod tip;
mod tree_view;

use browse::BrowseState;
use model::{
    Event, ScanControl, ScanState, Summary, TimingRun, Unit, append_timing, banner_text, scan_error_message, with_options, format_size,
    group_digits, offer_for, shows_start, timing_for_state,
};
use nav::{Dir, TreeMove};
use scan::{NodeId, Progress, ScanError, Tree};
use slint::{
    ComponentHandle, Image, Model, ModelNotify, ModelRc, ModelTracker, Rgba8Pixel, SetRenderingNotifierError,
    SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel, Weak,
};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use settings::{Part, Settings, ThemeChoice, View};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tree_view::{Row, SortKey, ViewState};
use treemap::filetype::TypeTotal;
use treemap::{Chrome, Frame, Rect, Theme};

slint::include_modules!();

/// Minimum gap between progress updates sent to the UI.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Quiet time after the last treemap size change before it is laid out again.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(80);
/// Narrowest frame label, in logical pixels: room for a few characters and "…".
const LABEL_MIN_WIDTH: f64 = 24.0;
/// A frame label hides while the stretched picture makes its box lower than this (logical px).
const FRAME_LABEL_MIN_STRETCHED_HEIGHT: f64 = 8.0;
/// Most labels on the map, frames and file tiles together: the frames first, then the largest
/// tiles. Measured at 3840x2160 and 200%, release, 3 runs of 10 frames, each with a new image
/// (P8.P6b fix 1): 650 labels (554 frames and 96 files) show in a median of 36.6 to 38.1 ms and a
/// worst of 43.6 ms, 13% under the 50 ms target. 750 gave a worst of 49.8 ms (0.4% margin), 1000
/// a median of 50.7 to 55.7 ms. The C: drive has 554 frames and 76 file tiles with a label at that
/// size (630), so the cap rarely applies.
const LABEL_CAP: usize = 650;
/// File tiles at least this high, in logical pixels, show the size on a second line.
const FILE_LABEL_TWO_LINES: f64 = 32.0;
/// Time the pointer stays on one treemap node before its tooltip shows.
const TIP_DELAY: Duration = Duration::from_millis(400);
/// Most file-type chips in the footer before "Other".
const FOOTER_CHIPS: usize = 8;
/// Time after which the folder argument's scan starts even if the window has drawn no frame.
const LAUNCH_FALLBACK: Duration = Duration::from_secs(2);

/// A finished scan plus its derived per-node data. Shared with treemap layout threads.
struct Data {
    tree: Tree,
    /// Files under each node, from `tree_view::file_counts`.
    counts: Vec<u64>,
}

/// The file tree as a Slint model. Rows are cheap ids; text is built only for rows the list
/// asks for, so a tree with millions of nodes costs nothing for the rows nobody sees.
struct RowModel {
    /// The shown data. Weak, and let go while a delete changes the data in place
    /// (`scan_apply::remove_from`, which gives it back): `Arc::get_mut` needs the only reference.
    data: RefCell<std::sync::Weak<Data>>,
    rows: RefCell<Vec<Row>>,
    notify: ModelNotify,
    /// The unit of the size column. Kept here, not read from the session: the list can ask for a
    /// row while the session is borrowed.
    unit: Cell<Unit>,
}

impl RowModel {
    fn new(data: Arc<Data>) -> Self {
        RowModel {
            data: RefCell::new(Arc::downgrade(&data)),
            rows: RefCell::new(Vec::new()),
            notify: ModelNotify::default(),
            unit: Cell::new(Unit::Dynamic),
        }
    }

    /// Lets go of the model's weak link to the data, so `Arc::get_mut` can change the data in
    /// place (a weak link makes it fail too). The rows stay; `row_data` gives nothing until `attach`.
    fn release(&self) {
        *self.data.borrow_mut() = std::sync::Weak::new();
    }

    /// Gives the model its data again and makes the list read every row again.
    fn attach(&self, data: &Arc<Data>) {
        *self.data.borrow_mut() = Arc::downgrade(data);
        self.notify.reset();
    }

    fn set_rows(&self, rows: Vec<Row>) {
        *self.rows.borrow_mut() = rows;
        self.notify.reset();
    }
}

impl Model for RowModel {
    type Data = TreeRow;

    fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    fn row_data(&self, i: usize) -> Option<TreeRow> {
        let row = *self.rows.borrow().get(i)?;
        let data = self.data.borrow().upgrade()?;
        let tree = &data.tree;
        let node = tree.node(row.node);
        let parent_size = node.parent.map_or(node.size, |p| tree.node(p).size);
        let (name, size) = (model::display_name(tree.name(row.node)), format_size(node.size, self.unit.get()));
        let (percent, files) = (tree_view::format_percent(node.size, parent_size), data.counts[row.node.0 as usize]);
        Some(TreeRow {
            spoken: tree_view::row_spoken(&name, &size, &percent, files).into(),
            name: name.as_ref().into(),
            size: size.into(),
            percent: percent.into(),
            files: group_digits(files).into(),
            depth: row.depth as i32,
            expanded: row.expanded,
            has_children: row.has_children,
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

/// What the tree and treemap show for the current scan.
struct Loaded {
    data: Arc<Data>,
    /// A number no other shown scan in this run has (`next_serial`). Tells two scans apart where
    /// the scan generation does not (`ScanControl::reset` keeps it) and a pointer could repeat.
    serial: u64,
    model: Rc<RowModel>,
    view: ViewState,
    selected: Option<NodeId>,
    /// Folder the treemap shows, or will show when its layout arrives.
    view_root: NodeId,
    /// Treemap rects in physical pixels, from the last layout that arrived.
    rects: Vec<Rect>,
    /// The folder of the shown picture and its `rects`; `None` before the first one.
    shown_root: Option<NodeId>,
    /// File-type totals of the shown tree: the count less every removal since. `None` while the
    /// file types are counted.
    totals: Option<Vec<TypeTotal>>,
    /// Removals from the tree since it was installed. A count started before a removal is late.
    edits: u64,
    /// The folder graph's state: expanded folders, colour mode, the shown graph, the type cache.
    graph: graph_view::GraphState,
}

/// UI-thread state. Events from worker threads are applied here via `invoke_from_event_loop`.
struct Session {
    /// State, id and stop flag of the current scan. Events from an older scan are dropped.
    scan: ScanControl,
    loaded: Option<Loaded>,
    /// Treemap area in logical pixels, set by the window on every resize.
    map_size: (f32, f32),
    /// Id of the latest treemap layout request. Results of older requests are dropped.
    layout_seq: u64,
    /// Root of the latest scan, as typed. Decides whether "Restart as administrator" is offered.
    scan_root: PathBuf,
    /// `scan_root` as the worker resolved it, once per scan: canonical, or as typed when it cannot
    /// be resolved (same rule as `scan::scan_with`). `None` until the scan finished.
    resolved_root: Option<PathBuf>,
    /// Browse dialog state: at most one dialog at a time.
    browse: BrowseState,
    /// Command line options (`--timing`, `--method`, `--threads`). Also passed on by "Restart as administrator".
    flags: cli::Flags,
    /// Notices about command line options that were ignored. Shown on the banner.
    notices: Vec<String>,
    /// Timings of the latest scan. `None` unless `timing` is set and a scan started.
    run: Option<TimingRun>,
    /// Id of the latest drive list request. Results of older requests are dropped.
    drives_seq: u64,
    /// The treemap node under the pointer, from the shown rects.
    hovered: Option<NodeId>,
    /// Times the hovered node changed (`note_hover`). With `--timing` it is printed at exit.
    hover_changes: u64,
    /// Latest pointer position over the treemap, in logical pixels.
    pointer: (f32, f32),
    /// Where the tooltip is in its life (see the tip module).
    tip: tip::TipState,
    /// Longest time one pointer move took on the UI thread. With `--timing` it is printed at exit.
    hover_slowest: Duration,
    /// The picture that moves over the treemap, until it is hidden.
    moving: Option<Move>,
    /// A layout that landed while a zoom in ran (`motion::holds_layout`). It shows when the zoom ends.
    held: Option<Layout>,
    /// The folder that the last "up" left. When the layout of the folder above lands, the old
    /// picture shrinks into it.
    out_from: Option<NodeId>,
    /// The next layout to land is the first picture of a scan: it fades in.
    fade_next: bool,
    /// Folder given on the command line. Its scan starts once, after the first frame or after
    /// `LAUNCH_FALLBACK`, whichever comes first (`take_launch_path`).
    launch_path: Option<String>,
    /// The shown scan was found changed on disk after an action (`menu::Follow`): the top bar
    /// offers Rescan. Cleared with the scan (`forget_scan`).
    outdated: bool,
    /// The checks after a menu action that are still to come. At most one; a new menu or scan drops it.
    follow: Option<menu::Follow>,
    /// The user's settings, as applied to the window.
    settings: Settings,
    /// Where the settings are saved. `None`: they are never saved (no `APPDATA`, or the file
    /// could not be kept before a save would replace it, see `adopt_settings`).
    settings_path: Option<PathBuf>,
    /// The delete dialog and the delete it runs.
    delete: scan_apply::DeleteSession,
}

/// The text the flyout shows when the settings are not saved.
const SAVE_ERROR: &str = "Settings could not be saved.";

/// A part of the window that shows a size, so it is synced again when the unit changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Site {
    /// The tree rows.
    Rows,
    /// The footer chips.
    Types,
    /// The treemap value of the selected node for a screen reader.
    Selection,
    /// The scan summary on the top bar or the start screen.
    Banner,
    /// The labels on the map and the hover tooltip (a new layout makes both again).
    Map,
    /// The drive cards of the start screen.
    Drives,
    /// The width of the Size column of the tree.
    Column,
}

/// Every part that is synced again after a unit change (`resync_after_unit_change`).
fn sites_after_unit_change() -> &'static [Site] {
    &[Site::Rows, Site::Types, Site::Selection, Site::Banner, Site::Map, Site::Drives, Site::Column]
}

/// Puts the settings loaded from `path` into the session. A file that was not kept
/// (`Loaded::NotKept`) is never saved over, and no path means nowhere to save: both give the
/// text for `settings-error` at once. Else the text is "".
fn adopt_settings(s: &mut Session, path: Option<PathBuf>, (settings, loaded): (Settings, settings::Loaded)) -> &'static str {
    s.settings = settings;
    s.settings_path = path.filter(|_| loaded != settings::Loaded::NotKept);
    if s.settings_path.is_some() { "" } else { SAVE_ERROR }
}

/// Saves the session settings with `save`. Returns the text for `settings-error`: "" when saved,
/// `SAVE_ERROR` when the save failed or the settings must not be saved. The settings stay as they
/// are either way.
fn persist_with(s: &mut Session, save: impl FnOnce(&Path, &Settings) -> std::io::Result<()>) -> &'static str {
    let Some(path) = s.settings_path.as_deref() else {
        return SAVE_ERROR;
    };
    match save(path, &s.settings) {
        Ok(()) => "",
        Err(e) => {
            eprintln!("Settings could not be saved to {}: {e}", path.display());
            SAVE_ERROR
        }
    }
}

/// The window is dark for `choice` when Windows is dark or not (`system_dark`).
fn effective_dark(choice: ThemeChoice, system_dark: bool) -> bool {
    choice.forced_dark().unwrap_or(system_dark)
}

/// The theme the winit window is set to for `choice`: `None` follows Windows.
fn winit_theme_for(choice: ThemeChoice) -> Option<slint::winit_030::winit::window::Theme> {
    use slint::winit_030::winit::window::Theme;
    choice.forced_dark().map(|dark| if dark { Theme::Dark } else { Theme::Light })
}

/// The biggest folder the Size column is made for: 4 TiB.
const SIZE_COLUMN_SAMPLE: u64 = 4 << 40;
/// The Size column width for Dynamic, GB and TB, where every size fits (logical pixels).
const SIZE_COLUMN_MIN: f32 = 76.0;

/// The width of the Size column of the tree for `unit`, in logical pixels: room for the size of a
/// 4 TiB folder in that unit (`estimated_text_width`), and never less than `SIZE_COLUMN_MIN`.
fn size_column_width(unit: Unit) -> f32 {
    // `NumberCell` keeps 6 px at its right; 2 px more is room for the estimate to be wrong.
    let text = estimated_text_width(&format_size(SIZE_COLUMN_SAMPLE, unit)) + 8.0;
    text.ceil().max(SIZE_COLUMN_MIN)
}

/// A high estimate of the width of a size text in the tree font, in logical pixels. Measured at
/// 100%: a digit is about 6 px, and a space and "B" together about 11 px.
fn estimated_text_width(text: &str) -> f32 {
    text.chars()
        .map(|c| match c {
            '0'..='9' => 7.0,
            ',' | '.' | ' ' => 3.5,
            'M' => 9.0,
            'G' => 8.0,
            _ => 7.5,
        })
        .sum()
}

/// Where the keyboard focus is, as far as hiding a part cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    /// A row of the folder tree.
    Tree,
    /// The treemap.
    Map,
    /// Anything else: the flyout, the gear, a button.
    Other,
}

/// Height of the footer panel, in logical pixels: 0 when the strip and the chips are both hidden.
fn footer_height(strip: bool, chips: bool) -> f32 {
    if strip || chips { 48.0 } else { 0.0 }
}

/// Where the focus goes when `part` is hidden while the focus is at `focus`. A hidden tree must not
/// keep the focus: it goes to the map, so the arrow keys still move the selection.
fn focus_after_hide(part: Part, focus: Focus) -> Focus {
    match (part, focus) {
        (Part::Tree, Focus::Tree) => Focus::Map,
        _ => focus,
    }
}

/// The values of the window's `show-header`, `show-tree`, `show-strip`, `show-chips` and
/// `show-labels`, in `Part::ALL` order.
fn show_flags(s: &Settings) -> [bool; 5] {
    Part::ALL.map(|p| s.shown(p))
}

/// Shows or hides `part` in the session settings, then saves them with `save`. Returns the text
/// for `settings-error`, as `persist_with` does.
fn toggle_part_with(s: &mut Session, part: Part, on: bool, save: impl FnOnce(&Path, &Settings) -> std::io::Result<()>) -> &'static str {
    s.settings.set_shown(part, on);
    persist_with(s, save)
}

/// Writes the five `show-*` properties and the footer height from the session settings.
fn apply_shown(ui: &MainWindow) {
    let [header, tree, strip, chips, labels] = SESSION.with_borrow(|s| show_flags(&s.settings));
    ui.set_show_header(header);
    ui.set_show_tree(tree);
    ui.set_show_strip(strip);
    ui.set_show_chips(chips);
    ui.set_show_labels(labels);
    ui.set_footer_height(footer_height(strip, chips));
}

/// Saves the session settings and shows in the flyout whether that worked.
fn persist(ui: &MainWindow) {
    let error = SESSION.with_borrow_mut(|s| persist_with(s, settings::save));
    ui.set_settings_error(error.into());
}

/// Pushes the session settings into the flyout. The theme itself is applied by `apply_theme`,
/// which needs the window to exist.
fn apply_settings(ui: &MainWindow) {
    let s = SESSION.with_borrow(|s| s.settings.clone());
    ui.set_theme_choice(s.theme.index());
    ui.set_unit_choice(s.unit.index());
    ui.set_always_admin(s.always_admin);
    ui.set_size_column(size_column_width(s.unit));
    apply_shown(ui);
    // A forced scheme is written before the window shows, so the first frame has it. Follow
    // Windows waits for `apply_theme`: the system theme is known once the window exists.
    if let Some(dark) = s.theme.forced_dark() {
        let scheme = if dark { slint::language::ColorScheme::Dark } else { slint::language::ColorScheme::Light };
        ui.global::<Palette>().set_color_scheme(scheme);
    }
}

/// Applies the theme setting: always writes `Palette.color-scheme` (a write cannot be undone, so
/// Follow Windows writes the system theme too), sets the winit window theme (`winit_theme_for`),
/// and Mica with the same dark mode, which also sets the title bar. An unknown system theme keeps
/// the scheme the window has. The `dark` property then follows and `theme-changed` redraws the map
/// and the chips.
fn apply_theme(ui: &MainWindow) {
    use slint::winit_030::{WinitWindowAccessor, winit::window::Theme as WinitTheme};
    let choice = SESSION.with_borrow(|s| s.settings.theme);
    // winit 0.30 keeps `theme()` as the system theme whatever `set_theme` is given: its
    // `set_theme` only calls `try_theme` and does not change `current_theme`.
    let system = ui
        .window()
        .with_winit_window(|w| {
            w.set_theme(winit_theme_for(choice));
            w.theme()
        })
        .flatten()
        .map(|t| t == WinitTheme::Dark);
    let dark = effective_dark(choice, system.unwrap_or_else(|| ui.get_dark()));
    let scheme = if dark { slint::language::ColorScheme::Dark } else { slint::language::ColorScheme::Light };
    ui.global::<Palette>().set_color_scheme(scheme);
    if ui.get_mica()
        && let Err(e) = window_vibrancy::apply_mica(ui.window().window_handle(), Some(dark))
    {
        eprintln!("Mica is not available: {e}");
        ui.set_mica(false);
    }
}

/// Syncs every part that shows a size (`sites_after_unit_change`) after the unit changed.
fn resync_after_unit_change(ui: &MainWindow) {
    for site in sites_after_unit_change() {
        match site {
            Site::Rows => sync_rows(ui),
            Site::Types => sync_types(ui),
            Site::Selection => sync_selection(ui),
            Site::Banner => show_state(ui),
            Site::Map => relayout(ui),
            // The start screen lists the drives again each time it shows.
            Site::Drives if ui.get_start_screen() => refresh_drives(ui),
            Site::Drives => {}
            Site::Column => ui.set_size_column(size_column_width(current_unit())),
        }
    }
}

/// Wires the flyout: each choice is applied at once and saved.
fn wire_settings(ui: &MainWindow) {
    ui.on_theme_chosen({
        let weak = ui.as_weak();
        move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let choice = ThemeChoice::from_index(i);
            SESSION.with_borrow_mut(|s| s.settings.theme = choice);
            ui.set_theme_choice(choice.index());
            apply_theme(&ui);
            persist(&ui);
        }
    });
    ui.on_unit_chosen({
        let weak = ui.as_weak();
        move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let unit = Unit::from_index(i);
            SESSION.with_borrow_mut(|s| s.settings.unit = unit);
            ui.set_unit_choice(unit.index());
            persist(&ui);
            resync_after_unit_change(&ui);
        }
    });
    ui.on_always_admin_toggled({
        let weak = ui.as_weak();
        move |on| {
            let Some(ui) = weak.upgrade() else { return };
            SESSION.with_borrow_mut(|s| s.settings.always_admin = on);
            ui.set_always_admin(on);
            persist(&ui);
        }
    });
    // A part shown or hidden: the layout change reaches the map through `treemap-resized`.
    ui.on_part_toggled({
        let weak = ui.as_weak();
        move |i, on| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(part) = usize::try_from(i).ok().and_then(|i| Part::ALL.get(i).copied()) else { return };
            let error = SESSION.with_borrow_mut(|s| toggle_part_with(s, part, on, settings::save));
            ui.set_settings_error(error.into());
            apply_shown(&ui);
            let focus = if ui.get_tree_has_focus() { Focus::Tree } else { Focus::Other };
            let next = if on { focus } else { focus_after_hide(part, focus) };
            if next != focus && next == Focus::Map {
                ui.invoke_focus_map();
            }
        }
    });
}

/// Says in the log how the settings file was read, when it was not read cleanly.
fn report_load(loaded: &settings::Loaded) {
    match loaded {
        settings::Loaded::Fresh | settings::Loaded::Ok => {}
        settings::Loaded::Partial(fields) => {
            eprintln!("Settings: these fields were wrong and use their defaults: {}. The file is kept as settings.json.bad.", fields.join(", "));
        }
        settings::Loaded::Damaged => eprintln!("Settings: the file could not be read. Defaults are used. The file is kept as settings.json.bad."),
        settings::Loaded::NotKept => eprintln!("Settings: the file could not be read or kept. Defaults are used and no setting is saved."),
    }
}

/// A finished layout of folder `root`: its rects in physical pixels, its picture and its labels.
struct Layout {
    root: NodeId,
    rects: Vec<Rect>,
    image: Option<SharedPixelBuffer<Rgba8Pixel>>,
    labels: (Vec<FrameLabel>, Vec<FileLabel>),
    /// The treemap size it was made for, in logical pixels.
    size: (f32, f32),
}

/// A move of a picture over the treemap, from frame `from` to frame `to` in physical pixels.
#[derive(Debug, Clone, Copy)]
struct Move {
    kind: motion::Kind,
    from: Frame,
    to: Frame,
    /// Its time is not up. A zoom in whose time is up stays at `to` until its layout lands.
    running: bool,
    started: Instant,
    /// `FRAMES` when it started.
    frames: u64,
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session {
        scan: ScanControl::default(),
        loaded: None,
        map_size: (0.0, 0.0),
        layout_seq: 0,
        scan_root: PathBuf::new(),
        resolved_root: None,
        browse: BrowseState::default(),
        flags: cli::Flags::default(),
        notices: Vec::new(),
        run: None,
        drives_seq: 0,
        hovered: None,
        hover_changes: 0,
        pointer: (0.0, 0.0),
        tip: tip::TipState::Idle,
        hover_slowest: Duration::ZERO,
        moving: None,
        held: None,
        out_from: None,
        fade_next: false,
        launch_path: None,
        outdated: false,
        follow: None,
        settings: Settings::default(),
        settings_path: None,
        delete: scan_apply::DeleteSession::default(),
    });
    /// Starts one relayout after the window stops changing the treemap size.
    static RESIZE_TIMER: Timer = Timer::default();
    /// Shows the tooltip of the hovered node after `TIP_DELAY`.
    static TIP_TIMER: Timer = Timer::default();
    /// Ends the time of the move of the treemap picture.
    static MOVE_TIMER: Timer = Timer::default();
    /// Runs the next check after a menu action (`run_follow`).
    static FOLLOW_TIMER: Timer = Timer::default();
    /// Frames the window has drawn. With `--timing` each move prints how many it took.
    static FRAMES: Cell<u64> = const { Cell::new(0) };
}

/// Time of a zoom in or out.
const ZOOM_TIME: Duration = Duration::from_millis(180);
/// Time of the fade in of the first picture of a scan.
const FADE_TIME: Duration = Duration::from_millis(250);

/// Applies `event` from scan `generation` and shows the new state. Dropped if a newer scan
/// started or the scan was cancelled.
fn apply(ui: &MainWindow, generation: u64, event: Event) {
    if SESSION.with_borrow_mut(|s| s.scan.apply(generation, event)) {
        show_state(ui);
    }
}

/// Pushes the scan state into the window.
fn show_state(ui: &MainWindow) {
    let state = SESSION.with_borrow(|s| s.scan.state().clone());
    let start = shows_start(&state);
    if start && !ui.get_start_screen() {
        refresh_drives(ui);
    }
    ui.set_start_screen(start);
    ui.set_show_new_scan(matches!(state, ScanState::Done(_)));
    ui.set_scanning(matches!(state, ScanState::Scanning { .. }));
    ui.set_banner_is_error(matches!(state, ScanState::Error(_)));
    let offer = SESSION.with_borrow(|s| offer_for(&state, s.resolved_root.as_deref(), elevate::is_admin()));
    let line = SESSION.with_borrow(|s| timing_for_state(&state, s.run.as_ref().and_then(TimingRun::line)));
    let (threads, notices) = SESSION.with_borrow(|s| {
        (s.flags.threads, s.notices.iter().chain(&s.delete.notice).cloned().collect::<Vec<_>>())
    });
    let banner = with_options(banner_text(&state, offer, current_unit()), &state, threads, &notices);
    ui.set_banner(append_timing(banner, line.as_deref()).into());
    ui.set_show_elevate(offer);
    ui.set_outdated(SESSION.with_borrow(|s| rescan_shown(&state, s.outdated)));
    ui.set_scan_title(SESSION.with_borrow(|s| model::scan_title(&s.scan_root)).into());
    graph_ui::sync_view(ui);
    delete_ui::sync(ui);
}

fn with_loaded<R>(f: impl FnOnce(&mut Loaded) -> R) -> Option<R> {
    SESSION.with_borrow_mut(|s| s.loaded.as_mut().map(f))
}

/// Rebuilds the visible tree rows and the sort and selection markers.
fn sync_rows(ui: &MainWindow) {
    let unit = current_unit();
    let Some((selected, key, descending)) = with_loaded(|l| {
        let rows = tree_view::flatten(&l.data.tree, &l.data.counts, &l.view);
        let selected = l.selected.and_then(|n| tree_view::row_index(&rows, n));
        l.model.unit.set(unit);
        l.model.set_rows(rows);
        (selected, l.view.key, l.view.descending)
    }) else {
        return;
    };
    ui.set_selected_row(selected.map_or(-1, |i| i as i32));
    ui.set_sort_key(key as i32);
    ui.set_sort_descending(descending);
}

/// Moves the selection outline to the selected node's area in the shown treemap, or to its
/// nearest drawn folder when the node itself is too small to draw (`tree_view::outline_node`).
/// The treemap value for a screen reader names the selected node.
fn sync_selection(ui: &MainWindow) {
    let scale = ui.window().scale_factor() as f64;
    let unit = current_unit();
    let value = with_loaded(|l| l.selected.map(|n| tree_view::map_value(&l.data.tree, n, unit))).flatten();
    ui.set_map_value(value.unwrap_or_default().into());
    let bounds = with_loaded(|l| {
        let shown = tree_view::outline_node(&l.data.tree, &l.rects, l.selected?)?;
        tree_view::selection_bounds(&l.rects, shown)
    })
    .flatten();
    ui.set_has_selection(bounds.is_some());
    if let Some((x, y, w, h)) = bounds {
        ui.set_selection_x((x / scale) as f32);
        ui.set_selection_y((y / scale) as f32);
        ui.set_selection_width((w / scale) as f32);
        ui.set_selection_height((h / scale) as f32);
    }
}

/// A layout to run (`begin_layout`): the data, the folder, the scan and layout ids it is for, the
/// map size in logical pixels, and the view (treemap or graph) it is for.
struct LayoutJob {
    data: Arc<Data>,
    root: NodeId,
    generation: u64,
    seq: u64,
    size: (f32, f32),
    view: View,
}

/// The session part of a new layout: a new `layout_seq` (any layout on its way is late now) and
/// no layout kept for a zoom in. `None` with no scan shown.
fn begin_layout(s: &mut Session) -> Option<LayoutJob> {
    let l = s.loaded.as_ref()?;
    let (data, root) = (l.data.clone(), l.view_root);
    s.layout_seq += 1;
    // A layout kept for a zoom in is older than this one now.
    s.held = None;
    // With `--timing`, each layout (and new picture) shows, so a hover that starts one is seen.
    if s.flags.timing {
        eprintln!("Layout {}.", s.layout_seq);
    }
    let view = graph_view::shown_view(s.scan.state(), true, s.settings.view);
    Some(LayoutJob { data, root, generation: s.scan.generation(), seq: s.layout_seq, size: s.map_size, view })
}

/// True when a layout of scan `generation` and layout `seq` may still land: no newer layout, no
/// applied removal (`scan_apply::step_pending` takes a new `layout_seq`) and no other scan since.
fn is_current(s: &Session, generation: u64, seq: u64) -> bool {
    (s.scan.generation(), s.layout_seq) == (generation, seq)
}

/// Lays out and draws the shown view (treemap or graph) for the current folder and window size on
/// a worker thread. The window keeps the old image until the new one arrives. A result is dropped
/// if a newer layout or a new scan started meanwhile.
fn relayout(ui: &MainWindow) {
    match SESSION.with_borrow_mut(begin_layout) {
        Some(job) if job.view == View::Graph => graph_ui::spawn_graph(ui, job),
        Some(job) => spawn_layout(ui, job),
        None => {}
    }
}

/// Runs `job` (`begin_layout`) on a worker thread and lands it when it is still the latest.
/// ponytail: one thread per request and no early stop; fine while a layout takes well under
/// a second and resizes are debounced. Upgrade path: a cancel flag per request.
fn spawn_layout(ui: &MainWindow, LayoutJob { data, root, generation, seq, size: (lw, lh), .. }: LayoutJob) {
    let scale = ui.window().scale_factor();
    let theme = if ui.get_dark() { Theme::Dark } else { Theme::Light };
    let (w, h) = ((lw * scale).round() as u32, (lh * scale).round() as u32);
    let weak = ui.as_weak();
    let unit = current_unit();
    std::thread::spawn(move || {
        let chrome = Chrome::scaled(f64::from(scale));
        let rects = treemap::layout_framed(&data.tree, root, w, h, chrome);
        let image = (w > 0 && h > 0).then(|| {
            let px = treemap::render_framed(&data.tree, &rects, w, h, chrome, theme);
            SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&px, w, h)
        });
        let labels = map_labels(&data.tree, &rects, chrome, f64::from(scale), theme, unit);
        // Let go of the data before the UI thread runs, so a delete waiting for it can apply.
        drop(data);
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            if SESSION.with_borrow(|s| is_current(s, generation, seq)) {
                land(&ui, Layout { root, rects, image, labels, size: (w as f32 / scale, h as f32 / scale) });
            }
            scan_apply::data_released(&ui);
        });
    });
}

/// The labels of the frames in `rects` (physical pixels) for the window, in logical pixels.
fn frame_labels(tree: &Tree, rects: &[Rect], chrome: Chrome, scale: f64, unit: Unit) -> Vec<FrameLabel> {
    let px = |v: f64| (v / scale) as f32;
    treemap::label_spots(tree, rects, chrome, LABEL_MIN_WIDTH * scale, LABEL_CAP)
        .iter()
        .map(|r| {
            let (name, size) = tree_view::frame_label(tree, r.node, unit);
            FrameLabel { x: px(r.x), y: px(r.y), width: px(r.w), height: px(r.h), name: name.into(), size: size.into() }
        })
        .collect()
}

/// The labels of the file tiles in `rects` (physical pixels) for the window, in logical pixels:
/// at most `cap`, the largest tiles first, with the text colour of each tile in `theme`.
fn file_labels(tree: &Tree, rects: &[Rect], scale: f64, theme: Theme, cap: usize, unit: Unit) -> Vec<FileLabel> {
    let px = |v: f64| (v / scale) as f32;
    let (min_w, min_h) = treemap::file_label_min(scale);
    treemap::file_label_spots(tree, rects, min_w, min_h, cap)
        .iter()
        .map(|r| {
            let (name, size) = tree_view::file_label(tree, r.node, unit);
            let [red, green, blue] = treemap::filetype::ink(treemap::filetype::colour(treemap::filetype::category_of(&name), theme));
            FileLabel {
                x: px(r.x),
                y: px(r.y),
                width: px(r.w),
                height: px(r.h),
                name: name.into(),
                size: size.into(),
                ink: slint::Color::from_rgb_u8(red, green, blue),
                two_lines: r.h >= FILE_LABEL_TWO_LINES * scale,
            }
        })
        .collect()
}

/// All labels of the map under the one `LABEL_CAP`: the frames first, then the file tiles in the
/// room the frames leave.
fn map_labels(tree: &Tree, rects: &[Rect], chrome: Chrome, scale: f64, theme: Theme, unit: Unit) -> (Vec<FrameLabel>, Vec<FileLabel>) {
    let frames = frame_labels(tree, rects, chrome, scale, unit);
    let files = file_labels(tree, rects, scale, theme, LABEL_CAP.saturating_sub(frames.len()), unit);
    (frames, files)
}

/// Keeps `node` as the hovered node. `true` when it is not the node hovered before: only then does
/// the window get new hover properties.
fn note_hover(node: Option<NodeId>) -> bool {
    SESSION.with_borrow_mut(|s| {
        let changed = std::mem::replace(&mut s.hovered, node) != node;
        s.hover_changes += u64::from(changed);
        changed
    })
}

/// What the window shows for hovered `node` in the shown `rects` (physical pixels), in logical
/// pixels: a file tile lifts in its file-type colour in `theme` and keeps its label when the tile
/// is big enough for one; a folder frame gets a highlight. Not visible when `node` is not drawn.
fn hover_of(data: &Data, rects: &[Rect], node: NodeId, scale: f64, theme: Theme, unit: Unit) -> Hover {
    let Some((x, y, w, h)) = tree_view::selection_bounds(rects, node) else {
        return Hover::default();
    };
    let tree = &data.tree;
    let px = |v: f64| (v / scale) as f32;
    let [red, green, blue] = treemap::filetype::colour(treemap::filetype::category_of(tree.name(node)), theme);
    // The same label rules as on the map, for the tile alone at its own origin (none for a folder).
    let label = file_labels(tree, &[Rect { node, x: 0.0, y: 0.0, w, h }], scale, theme, 1, unit).pop();
    let (title, detail) = tree_view::tooltip_lines(tree, &data.counts, node, unit);
    Hover {
        visible: true,
        x: px(x),
        y: px(y),
        width: px(w),
        height: px(h),
        lift: !tree.node(node).is_dir,
        color: slint::Color::from_rgb_u8(red, green, blue),
        labelled: label.is_some(),
        label: label.unwrap_or_default(),
        title: title.into(),
        detail: detail.into(),
    }
}

/// The hover glow is drawn as a drop shadow, which the software renderer does not draw. A renderer
/// that takes a rendering notifier (FemtoVG on OpenGL) draws it; the software renderer returns
/// `Unsupported`, and then the hovered tile gets a brighter border instead.
fn glow_drawn(notifier: Result<(), SetRenderingNotifierError>) -> bool {
    // AlreadySet: some renderer took a notifier, so it is not the software one. Any other error is
    // read as "no shadows": the border shows on every renderer.
    matches!(notifier, Ok(()) | Err(SetRenderingNotifierError::AlreadySet))
}

/// The node of the shown treemap under the window point (x, y) in logical pixels. While a zoom in
/// or out shows, the point is on the zooming picture, so it is found in that picture's own rects.
fn node_at(ui: &MainWindow, x: f32, y: f32) -> Option<NodeId> {
    let (x, y) = to_physical(ui, x, y);
    let moving = SESSION.with_borrow(|s| s.moving);
    let (t, view) = (f64::from(ui.get_zoom_t()), map_px(ui));
    with_loaded(|l| hit_at(&l.rects, moving, t, view, x, y)).flatten()
}

/// The point of the shown picture, in its own rects' physical pixels, under the view point (x, y)
/// of a `view` (w, h) treemap, at time `t` of move `moving`. While a zoom shows, the picture is
/// drawn at the zoom's frame (`zoom_frame_at`). `None` when that frame has no area.
fn picture_point(moving: Option<Move>, t: f64, (w, h): (f64, f64), x: f64, y: f64) -> Option<(f64, f64)> {
    match zoom_frame_at(moving, t) {
        Some(frame) => treemap::unzoom(frame, w, h, x, y),
        None => Some((x, y)),
    }
}

/// The pointer moved over the treemap to (x, y) in logical pixels. The window gets new hover
/// properties only when the node under the pointer changes: no layout, no new picture. Only a
/// real pointer move arms the tooltip again after a key, a click, a scroll or a new layout.
fn hover(ui: &MainWindow, x: f32, y: f32) {
    let started = Instant::now();
    SESSION.with_borrow_mut(|s| s.pointer = (x, y));
    let node = node_at(ui, x, y);
    let (over, changed) = gate_hover(node, |n| show_hover(ui, n));
    tip_event(ui, tip::TipEvent::Moved { over, changed });
    let took = started.elapsed();
    SESSION.with_borrow_mut(|s| s.hover_slowest = s.hover_slowest.max(took));
}

/// The hover gate: keeps `node` as the hovered node and calls `show` with it only when it is not
/// the node hovered before (`note_hover`). Returns (a node is under the pointer and shown, it
/// changed); `show` returns whether the hover is visible.
fn gate_hover(node: Option<NodeId>, show: impl FnOnce(Option<NodeId>) -> bool) -> (bool, bool) {
    let changed = note_hover(node);
    let over = if changed { show(node) } else { node.is_some() };
    (over, changed)
}

/// The pointer left the map: nothing is hovered (`show` gets `None` when a node was) and the
/// tooltip gets `Leave`. Returns what the tooltip does next.
fn left_map(show: impl FnOnce(Option<NodeId>) -> bool) -> tip::TipAction {
    gate_hover(None, show);
    step_tip(tip::TipEvent::Leave)
}

/// Drops the shown scan from the session: its data, the picture kept or moving for it and its
/// hover (`left_map`). Returns what the tooltip does next.
fn forget_scan(show: impl FnOnce(Option<NodeId>) -> bool) -> tip::TipAction {
    SESSION.with_borrow_mut(|s| {
        s.loaded = None;
        s.held = None;
        s.out_from = None;
        s.fade_next = false;
        s.outdated = false;
        s.follow = None;
        // A dialog, a job result or a removal of the dropped scan never touches another one.
        scan_apply::scan_changed(s);
    });
    left_map(show)
}

/// The window opens on the start screen unless a folder to scan was given on the command line:
/// that scan starts after the first frame, so the first frame shows the results screen. A blank
/// folder fails at once and shows the start screen with the error, so it opens there.
fn shows_start_at_launch(launch_path: Option<&str>) -> bool {
    launch_path.is_none_or(|p| p.trim().is_empty())
}

/// The pointer left the treemap: nothing is hovered.
fn leave(ui: &MainWindow) {
    let action = left_map(|n| show_hover(ui, n));
    do_tip(ui, action);
}

/// The shown rects changed or the shown folder changes: the old hover and tooltip go. The node
/// under a pointer that has not moved is not looked up; the next pointer move finds it.
fn drop_hover(ui: &MainWindow) {
    gate_hover(None, |n| show_hover(ui, n));
    tip_event(ui, tip::TipEvent::Layout);
}

/// Pushes the hover properties of `node` (`None`: nothing hovered) into the window. True when
/// the hover is visible.
fn show_hover(ui: &MainWindow, node: Option<NodeId>) -> bool {
    let scale = f64::from(ui.window().scale_factor());
    let theme = if ui.get_dark() { Theme::Dark } else { Theme::Light };
    let unit = current_unit();
    let hover = node.and_then(|n| with_loaded(|l| hover_of(&l.data, &l.rects, n, scale, theme, unit))).unwrap_or_default();
    let shown = hover.visible;
    ui.set_hover(hover);
    shown
}

/// Feeds `event` to the tooltip state machine and does what it says (`do_tip`).
fn tip_event(ui: &MainWindow, event: tip::TipEvent) {
    let action = step_tip(event);
    do_tip(ui, action);
}

/// Feeds `event` to the tooltip state machine of the session. Returns what the tooltip does next.
fn step_tip(event: tip::TipEvent) -> tip::TipAction {
    SESSION.with_borrow_mut(|s| {
        let (next, action) = tip::step(s.tip, event);
        s.tip = next;
        action
    })
}

/// Does what the tooltip state machine said: the tooltip of the old node hides; the one of the new
/// node shows after `TIP_DELAY`, at the pointer.
fn do_tip(ui: &MainWindow, action: tip::TipAction) {
    match action {
        tip::TipAction::None => {}
        tip::TipAction::Hide => hide_tip(ui),
        tip::TipAction::Show => {
            let (x, y) = SESSION.with_borrow(|s| s.pointer);
            ui.set_tip_x(x);
            ui.set_tip_y(y);
            ui.set_tip_visible(true);
        }
        tip::TipAction::Start => {
            hide_tip(ui);
            let weak = ui.as_weak();
            TIP_TIMER.with(|t| {
                t.start(TimerMode::SingleShot, TIP_DELAY, move || {
                    if let Some(ui) = weak.upgrade() {
                        tip_event(&ui, tip::TipEvent::Timer);
                    }
                })
            });
        }
    }
}

/// Hides the tooltip, also one still waiting for `TIP_DELAY`.
fn hide_tip(ui: &MainWindow) {
    TIP_TIMER.with(Timer::stop);
    ui.set_tip_visible(false);
}

/// Shows a finished layout of `root`: the image, its rects and labels, the crumbs and the selection.
fn show_layout(ui: &MainWindow, Layout { root, rects, image, labels: (labels, file_labels), size }: Layout) {
    let Some((crumbs, note)) = with_loaded(|l| {
        let tree = &l.data.tree;
        let note = tree_view::map_note(tree, root, &rects, image.is_some());
        l.rects = rects;
        l.shown_root = Some(root);
        (tree_view::crumbs(tree, root).into_iter().map(crumb).collect::<Vec<_>>(), note)
    }) else {
        return;
    };
    if let Some(px) = image {
        ui.set_treemap(Image::from_rgba8(px));
    }
    ui.set_layout_width(size.0);
    ui.set_layout_height(size.1);
    ui.set_labels(ModelRc::new(VecModel::from(labels)));
    ui.set_file_labels(ModelRc::new(VecModel::from(file_labels)));
    ui.set_map_note(note.into());
    // A new model rebuilds the crumbs and drops the keyboard focus of a crumb, so a relayout of the
    // same folder (resize, theme) keeps the model.
    if !ui.get_crumbs().iter().eq(crumbs.iter().cloned()) {
        ui.set_crumbs(ModelRc::new(VecModel::from(crumbs)));
    }
    sync_selection(ui);
    drop_hover(ui);
    note_treemap_shown(ui);
}

/// A box (x, y, w, h) in logical pixels.
type LogicalBox = (f32, f32, f32, f32);

/// Box `b` of a layout made for a `from` (w, h) map, on a `to` (w, h) map that shows the picture of
/// that layout stretched over it: each axis scales by its own factor, so the box stays on its frame.
/// An axis with no `from` size (no layout yet) does not scale.
fn stretch(b: LogicalBox, from: (f32, f32), to: (f32, f32)) -> LogicalBox {
    // `from > 0` is false for NaN too.
    let factor = |from: f32, to: f32| if from > 0.0 { to / from } else { 1.0 };
    let (fx, fy) = (factor(from.0, to.0), factor(from.1, to.1));
    (b.0 * fx, b.1 * fy, b.2 * fx, b.3 * fy)
}

/// Label or outline `p` of the layout made for a `from` map, on the `to` map (`stretch`). A label
/// shows only while its box is still as big as a label of `kind` needs; the text keeps its size. A
/// file label keeps the size on a second line only while the box is high enough for two lines.
fn place_label(p: Placed, kind: LabelKind, from: (f32, f32), to: (f32, f32)) -> Placed {
    let (x, y, width, height) = stretch((p.x, p.y, p.width, p.height), from, to);
    // A frame label needs its least width and, while the picture is stretched, only half the label
    // box in height: the text keeps its size and the final layout re-fits it.
    let (min_w, min_h) = match kind {
        LabelKind::Frame => (LABEL_MIN_WIDTH, FRAME_LABEL_MIN_STRETCHED_HEIGHT),
        LabelKind::File => (treemap::FILE_LABEL_MIN_WIDTH, treemap::FILE_LABEL_MIN_HEIGHT),
        LabelKind::Outline => (0.0, 0.0),
    };
    let (w, h) = (f64::from(width), f64::from(height));
    Placed { x, y, width, height, shown: w >= min_w && h >= min_h, two_lines: h >= FILE_LABEL_TWO_LINES }
}

/// The frame of move `m` at time `t` (0 to 1) of the move, eased out: `treemap::zoom_at` for the
/// place and `treemap::eased` for the opacity, in logical pixels.
fn frame_at(m: &ZoomMove, t: f32) -> ZoomFrame {
    let f = |z: &ZoomFrame| (f64::from(z.x), f64::from(z.y), f64::from(z.width), f64::from(z.height));
    let t = f64::from(t);
    let (x, y, w, h) = treemap::zoom_at(f(&m.from), f(&m.to), t);
    let opacity = treemap::eased(f64::from(m.from.opacity), f64::from(m.to.opacity), t);
    ZoomFrame { x: x as f32, y: y as f32, width: w as f32, height: h as f32, opacity: opacity as f32 }
}

/// A layout of the current request landed. It waits while a zoom in runs (`motion::holds_layout`);
/// else it shows at once, in the same frame as the moving picture hides. Then it fades in (the
/// first picture of a scan) or zooms out from the folder that going up left (`motion::landing`).
fn land(ui: &MainWindow, layout: Layout) {
    let hold = SESSION.with_borrow(|s| motion::holds_layout(s.moving.map(|m| m.kind), s.moving.is_some_and(|m| m.running)));
    if hold {
        SESSION.with_borrow_mut(|s| s.held = Some(layout));
        return;
    }
    let old_root = with_loaded(|l| l.shown_root).flatten();
    let (out_from, first) = SESSION.with_borrow_mut(|s| (s.out_from.take(), std::mem::take(&mut s.fade_next)));
    show_layout(ui, layout);
    stop_move(ui);
    let out_rect = out_from.and_then(|left| {
        with_loaded(|l| {
            let shown = tree_view::outline_node(&l.data.tree, &l.rects, left)?;
            tree_view::selection_bounds(&l.rects, shown)
        })
        .flatten()
    });
    let (w, h) = map_px(ui);
    let full = (0.0, 0.0, w, h);
    match (motion::landing(motion_on(), first, out_from, old_root, out_rect), out_from) {
        (motion::Landing::Fade, _) => start_move(ui, motion::Kind::Fade, ui.get_treemap(), full, full, (0.0, 1.0)),
        (motion::Landing::Out((x, y, rw, rh)), Some(left)) => {
            if let Some(from) = treemap::zoom_frame(&Rect { node: left, x, y, w: rw, h: rh }, w, h) {
                start_move(ui, motion::Kind::Out, ui.get_treemap(), from, full, (1.0, 1.0));
            }
        }
        _ => {}
    }
}

/// Motion is on now (`motion::allowed`). Read for each move, so a change of the setting counts at once.
fn motion_on() -> bool {
    motion::allowed(motion::animation_setting())
}

/// The treemap view in physical pixels.
fn map_px(ui: &MainWindow) -> (f64, f64) {
    let scale = f64::from(ui.window().scale_factor());
    let (w, h) = SESSION.with_borrow(|s| s.map_size);
    (f64::from(w) * scale, f64::from(h) * scale)
}

/// Starts move `kind` of `image` over the treemap, from frame `from` to frame `to` (physical
/// pixels), with the opacity going from `opacity.0` to `opacity.1`. It replaces a move that runs.
/// The treemap picture does not show while a picture moves: they are glass and would mix.
/// Nothing waits for the move: the window takes input all the time.
fn start_move(ui: &MainWindow, kind: motion::Kind, image: Image, from: Frame, to: Frame, opacity: (f32, f32)) {
    let scale = f64::from(ui.window().scale_factor());
    let logical = |(x, y, w, h): Frame, opacity: f32| ZoomFrame {
        x: (x / scale) as f32,
        y: (y / scale) as f32,
        width: (w / scale) as f32,
        height: (h / scale) as f32,
        opacity,
    };
    ui.set_zoom_image(image);
    ui.set_zoom(ZoomMove { from: logical(from, opacity.0), to: logical(to, opacity.1) });
    let time = if kind == motion::Kind::Fade { FADE_TIME } else { ZOOM_TIME };
    ui.set_zoom_duration(time.as_millis() as i64);
    ui.set_zoom_running(true);
    ui.set_zoom_shown(true);
    ui.invoke_start_zoom();
    let frames = FRAMES.with(Cell::get);
    SESSION.with_borrow_mut(|s| s.moving = Some(Move { kind, from, to, running: true, started: Instant::now(), frames }));
    drop_hover(ui);
    let weak = ui.as_weak();
    MOVE_TIMER.with(|t| {
        t.start(TimerMode::SingleShot, time, move || {
            if let Some(ui) = weak.upgrade() {
                move_time_up(&ui);
            }
        })
    });
}

/// The time of the move is up. A zoom in stays at its end frame until its layout lands; then
/// that layout shows and the moving picture hides, in the same frame. Other moves hide.
fn move_time_up(ui: &MainWindow) {
    ui.set_zoom_running(false);
    let Some(m) = SESSION.with_borrow_mut(|s| {
        s.moving.as_mut().map(|m| {
            m.running = false;
            *m
        })
    }) else {
        return;
    };
    if SESSION.with_borrow(|s| s.flags.timing) {
        let now = FRAMES.with(Cell::get);
        eprintln!("Move {:?}: {} frames in {} ms. Frames drawn: {now}.", m.kind, now - m.frames, m.started.elapsed().as_millis());
    }
    if m.kind != motion::Kind::In {
        stop_move(ui);
    } else if let Some(layout) = SESSION.with_borrow_mut(|s| s.held.take()) {
        show_layout(ui, layout);
        stop_move(ui);
    }
}

/// Hides the moving picture at once.
fn stop_move(ui: &MainWindow) {
    MOVE_TIMER.with(Timer::stop);
    if SESSION.with_borrow_mut(|s| s.moving.take()).is_some() {
        ui.set_zoom_running(false);
        ui.set_zoom_shown(false);
        ui.set_zoom_image(Image::default());
    }
}

/// Where the picture of a zoom in or out is on the treemap now (physical pixels), or `None` when no
/// zoom shows.
fn zoom_now(ui: &MainWindow) -> Option<Frame> {
    zoom_frame_at(SESSION.with_borrow(|s| s.moving), f64::from(ui.get_zoom_t()))
}

/// Where the picture of zoom `moving` (in or out) is on the treemap at time `t` of it (physical
/// pixels), or `None` when no zoom shows (no move, or a fade).
fn zoom_frame_at(moving: Option<Move>, t: f64) -> Option<Frame> {
    let m = moving.filter(|m| matches!(m.kind, motion::Kind::In | motion::Kind::Out))?;
    Some(treemap::zoom_at(m.from, m.to, t))
}

/// The window's form of a crumb from `tree_view::crumbs`: its index is the node it opens, -1 for none.
fn crumb((node, text): (Option<NodeId>, String)) -> Crumb {
    Crumb { text: text.into(), index: node.map_or(-1, |n| i32::try_from(n.0).unwrap_or(-1)) }
}

/// With `--timing`: when the first treemap of a finished scan is shown, completes the timing line,
/// prints it to stderr and adds it to the banner. Later layouts of the same scan change nothing.
fn note_treemap_shown(ui: &MainWindow) {
    let line = SESSION.with_borrow_mut(|s| {
        s.run.as_mut()?.complete(Instant::now())
    });
    if let Some(line) = line {
        eprintln!("{line}");
        show_state(ui);
    }
}

/// Selects `node` in the tree and the treemap and scrolls the tree to it. If the treemap
/// folder does not hold `node`, the treemap shows the folder of `node` (`view_for_selection`).
/// Every select path comes here.
fn select(ui: &MainWindow, node: NodeId) {
    let view = with_loaded(|l| {
        l.selected = Some(node);
        l.view.reveal(&l.data.tree, node);
        tree_view::view_for_selection(&l.data.tree, l.view_root, node)
    });
    if let Some(folder) = view {
        show_folder(ui, folder);
    }
    sync_rows(ui);
    sync_selection(ui);
    let row = ui.get_selected_row();
    // Next turn of the event loop: the list needs one layout pass to know its new height.
    let weak = ui.as_weak();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade().filter(|_| row >= 0) {
            ui.invoke_show_row(row);
        }
    });
}

/// Where a menu was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// A right-click on the treemap; the tile under the pointer is passed separately (`hit_at`).
    Map,
    /// A right-click on tree row `i`.
    Row(i32),
    /// A right-click on the crumb that opens node `i` (-1: the gap crumb, which opens nothing).
    Crumb(i32),
    /// The Menu key or Shift+F10: the selected node.
    Key,
}

/// The node a menu is for and whether to select it first. `hit` is the treemap tile under the
/// pointer (`Source::Map`). `None` for no item or an item that left the tree. A right-click on a
/// tile or a row selects it, as a left-click does; on a crumb it does not (selecting an ancestor
/// would move the map), and a key acts on the selection that already exists.
fn target_of(l: &Loaded, src: Source, hit: Option<NodeId>) -> Option<(NodeId, bool)> {
    let (node, select) = match src {
        Source::Map => (hit?, true),
        Source::Row(i) => (l.model.rows.borrow().get(usize::try_from(i).ok()?)?.node, true),
        Source::Crumb(i) => {
            let i = u32::try_from(i).ok().filter(|&i| (i as usize) < l.data.counts.len())?;
            (NodeId(i), false)
        }
        Source::Key => (l.selected?, false),
    };
    // A tile or a crumb can outlive a removal: a node that left the tree gets no menu.
    let tree = &l.data.tree;
    scan_apply::in_subtree(tree, node, tree.root()).then_some((node, select))
}

/// What a menu is for, after the gate and the disk check.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    node: NodeId,
    /// Select the node before the menu shows (`target_of`).
    select: bool,
    is_dir: bool,
    /// `Loaded::edits` at the menu.
    edits: u64,
    prepared: menu::Prepared,
}

/// The menu asked for at `src` (`hit`: the tile under the pointer), or `None` when no menu opens:
/// no finished scan (`menu::allowed`) or no item (`target_of`). The disk check (`menu::prepare`
/// with `disk`) runs here only, for every way a menu opens. A menu drops the checks still to come
/// of the menu before it.
fn menu_request(src: Source, hit: Option<NodeId>, disk: impl Fn(&std::path::Path) -> menu::Disk) -> Option<Request> {
    SESSION.with_borrow_mut(|s| {
        let l = s.loaded.as_ref().filter(|_| menu::allowed(s.scan.state(), true))?;
        let (node, select) = target_of(l, src, hit)?;
        let tree = &l.data.tree;
        let request = Request { node, select, is_dir: tree.node(node).is_dir, edits: l.edits, prepared: menu::prepare(tree, node, disk) };
        s.follow = None;
        Some(request)
    })
}

/// Opens the context menu asked for at `src` (`hit`: the tile under the pointer) at `at` in the
/// window, when `menu_request` gives one. A tile or a row is selected first;
/// the hover and the tooltip hide. No session borrow is held while the menu shows (it runs its
/// own message loop).
fn open_menu(ui: &MainWindow, src: Source, hit: Option<NodeId>, at: menu::At) {
    let Some(window) = shell::window_hwnd(ui.window()) else { return };
    let Some(request) = menu_request(src, hit, menu::read_disk) else { return };
    if request.select {
        select(ui, request.node);
    }
    drop_hover(ui);
    let owner = shell::menu_owner(window);
    let path = match &request.prepared {
        menu::Prepared::Ready { path, .. } => Some(path.as_path()),
        menu::Prepared::Changed => None,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_APPS, VK_SHIFT};
    let flags = menu::query_flags(menu::extended(at, shell::key_down(VK_SHIFT), shell::key_down(VK_APPS)));
    let popup = match shell::Popup::new(owner, path, flags, |shell_ok| menu::items(&request.prepared, request.is_dir, shell_ok)) {
        Ok(popup) => popup,
        Err(e) => return eprintln!("The menu cannot open: {e}"),
    };
    let anchor = menu::anchor(at, ui.window().scale_factor(), shell::client_origin(window));
    let id = popup.track(owner, window, anchor);
    match (menu::picked(id), request.prepared) {
        (menu::Picked::Volatus(menu::Cmd::Rescan), _) => rescan(ui),
        (menu::Picked::Volatus(menu::Cmd::ShowFolder), _) => open_node(ui, request.node),
        // Shift is read now, as the item is chosen (`delete_ui::menu_delete`).
        (menu::Picked::Volatus(menu::Cmd::Delete), _) => delete_ui::menu_delete(ui, request.node),
        (menu::Picked::Volatus(cmd), menu::Prepared::Ready { path, .. }) => {
            let act = |cmd, p: &std::path::Path| match cmd {
                // On a worker (another file manager can block it); its note comes through the event loop.
                menu::Cmd::ExploreFolder => {
                    let weak = ui.as_weak();
                    shell::explore_on_worker(p.to_path_buf(), request.is_dir, move |result| {
                        if let Some(note) = action_note(cmd, result.inspect_err(|e| eprintln!("{e}")).is_err()) {
                            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_elevate_note(note.into()));
                        }
                    });
                    Ok(())
                }
                _ => shell::act(cmd, p, request.is_dir).inspect_err(|e| eprintln!("{e}")),
            };
            if let Some(note) = run_action(cmd, &path, request.is_dir, act) {
                ui.set_elevate_note(note.into());
            }
        }
        (menu::Picked::Shell(offset), menu::Prepared::Ready { path, before }) => {
            let follow = menu::Follow::new(request.edits, path, before);
            use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_SHIFT};
            // The keys held now, as Explorer passes them to the item.
            let mask = menu::invoke_mask(shell::key_down(VK_SHIFT), shell::key_down(VK_CONTROL));
            let point = match anchor {
                menu::Anchor::Point(x, y) | menu::Anchor::Below { left: x, bottom: y, .. } => (x, y),
            };
            if let Some(follow) = menu::after_pick(follow, menu::read_disk, || popup.invoke(offset, window, point, mask)) {
                SESSION.with_borrow_mut(|s| s.follow = Some(follow));
                run_follow(ui);
            }
        }
        _ => {}
    }
}

/// The tile of the treemap (`rects`, physical pixels) under the view point (x, y) while `moving`
/// is at time `t`: what a left-click hits (`picture_point`).
fn hit_at(rects: &[Rect], moving: Option<Move>, t: f64, view: (f64, f64), x: f64, y: f64) -> Option<NodeId> {
    let (x, y) = picture_point(moving, t, view, x, y)?;
    tree_view::hit_test(rects, x, y)
}

/// Runs a Volatus menu item through `act` and gives the note to show when it fails. `act` gets the
/// command and the path to use (a command prompt gets `menu::terminal_folder`). Show this folder,
/// Rescan and Delete are not actions of the shell: no call, no note. An `act` that runs on a worker (Explore
/// Folder) gives `Ok` at once and shows its note itself (`action_note`).
fn run_action(
    cmd: menu::Cmd,
    path: &std::path::Path,
    is_dir: bool,
    act: impl FnOnce(menu::Cmd, &std::path::Path) -> Result<(), String>,
) -> Option<String> {
    let used = match cmd {
        menu::Cmd::ExploreFolder | menu::Cmd::CopyPath => Cow::Borrowed(path),
        menu::Cmd::CommandPrompt => Cow::Owned(menu::terminal_folder(path, is_dir)),
        menu::Cmd::ShowFolder | menu::Cmd::Rescan | menu::Cmd::Delete => return None,
    };
    action_note(cmd, act(cmd, &used).is_err()).map(str::to_string)
}

/// The note to show after the Volatus menu item `cmd` ran: its own note when it `failed`, else
/// none. Show this folder, Rescan and Delete have none.
fn action_note(cmd: menu::Cmd, failed: bool) -> Option<&'static str> {
    let note = match cmd {
        menu::Cmd::ExploreFolder => "Volatus cannot open the folder.",
        menu::Cmd::CommandPrompt => "Volatus cannot open a command prompt.",
        menu::Cmd::CopyPath => "Volatus cannot copy the path.",
        menu::Cmd::ShowFolder | menu::Cmd::Rescan | menu::Cmd::Delete => return None,
    };
    failed.then_some(note)
}

/// True when the top bar offers Rescan: a finished scan found changed on disk.
fn rescan_shown(state: &ScanState, outdated: bool) -> bool {
    outdated && matches!(state, ScanState::Done(_))
}

/// The folder a Rescan scans, as typed (`relaunch_path`): the shown scan's root. `None` unless a
/// scan is finished. The options stay in the session, so the scan runs with the same ones.
fn rescan_target() -> Option<String> {
    SESSION.with_borrow(|s| matches!(s.scan.state(), ScanState::Done(_)).then(|| relaunch_path(false, &s.scan_root)))
}

/// What `check_follow` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FollowStep {
    /// The scan is outdated now (`Session::outdated`): the top bar offers Rescan.
    Outdated,
    /// Check again after this time.
    Again(Duration),
    /// No check is left.
    Done,
}

/// One check of the pending checks after a menu action (`Session::follow`), with the disk read by
/// `disk`. A decision, or the last check, ends them; so does a scan that was replaced or changed
/// since the menu (its node id would be stale).
fn check_follow(disk: impl Fn(&std::path::Path) -> menu::Disk) -> FollowStep {
    SESSION.with_borrow_mut(|s| {
        let Some(mut follow) = s.follow.take() else { return FollowStep::Done };
        if s.loaded.as_ref().map(|l| l.edits) != Some(follow.edits) {
            return FollowStep::Done;
        }
        match follow.step(&disk(&follow.path)) {
            (menu::Outcome::Outdated, _) => {
                s.outdated = true;
                FollowStep::Outdated
            }
            (menu::Outcome::Wait, Some(delay)) => {
                s.follow = Some(follow);
                FollowStep::Again(delay)
            }
            _ => FollowStep::Done,
        }
    })
}

/// Runs the checks after a menu action (`check_follow`) until one decides: the top bar says the
/// scan is outdated. The next check runs on `FOLLOW_TIMER`.
fn run_follow(ui: &MainWindow) {
    match check_follow(menu::read_disk) {
        FollowStep::Outdated => show_state(ui),
        FollowStep::Again(delay) => {
            let weak = ui.as_weak();
            FOLLOW_TIMER.with(|t| {
                t.start(TimerMode::SingleShot, delay, move || {
                    if let Some(ui) = weak.upgrade() {
                        run_follow(&ui);
                    }
                })
            });
        }
        FollowStep::Done => {}
    }
}

/// Rescan: scans the shown scan's root again, with the same options (`rescan_target`). The new
/// scan drops the outdated mark and the pending checks (`forget_scan`).
fn rescan(ui: &MainWindow) {
    if let Some(path) = rescan_target() {
        start_scan(ui, &path);
    }
}

/// A new number for a shown scan, never given before in this run.
fn next_serial() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Replaces the shown scan with a new one.
fn install(ui: &MainWindow, data: Arc<Data>) {
    let root = data.tree.root();
    let model = Rc::new(RowModel::new(data.clone()));
    model.unit.set(current_unit());
    ui.set_rows(ModelRc::from(model.clone()));
    SESSION.with_borrow_mut(|s| {
        s.loaded = Some(Loaded {
            data: data.clone(),
            serial: next_serial(),
            model,
            view: ViewState::new(root),
            selected: Some(root),
            view_root: root,
            rects: Vec::new(),
            shown_root: None,
            totals: None,
            edits: 0,
            graph: graph_view::GraphState::default(),
        });
        s.fade_next = true;
    });
    graph_ui::clear_picture(ui);
    sync_rows(ui);
    relayout(ui);
    count_types(ui, data);
}

/// Counts the file types of the scan for the footer on its own worker thread, started after the
/// layout worker of the first treemap, which does not wait for it. Until it ends the footer
/// says "Counting file types.".
/// The result is dropped if a newer scan started or the scan was cancelled meanwhile, and counted
/// again for the changed tree if a removal happened meanwhile (`store_totals`).
fn count_types(ui: &MainWindow, data: Arc<Data>) {
    let (generation, edits) = SESSION.with_borrow(|s| (s.scan.generation(), s.loaded.as_ref().map_or(0, |l| l.edits)));
    sync_types(ui);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let totals = treemap::filetype::type_totals(&data.tree, data.tree.root());
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            let stored = store_totals(generation, &data, edits, totals);
            // Let go of the data, so a delete waiting for it can apply.
            drop(data);
            match stored {
                Stored::Kept => sync_types(&ui),
                Stored::Recount => {
                    if let Some(now) = with_loaded(|l| l.data.clone()) {
                        count_types(&ui, now);
                    }
                }
                Stored::Late => {}
            }
            scan_apply::data_released(&ui);
        });
    });
}

/// What `store_totals` did with file-type totals that landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stored {
    /// Kept: the footer shows them.
    Kept,
    /// Dropped: of an older scan, or of data the window no longer shows.
    Late,
    /// Dropped: counted before a removal from the shown tree. The count must start again.
    Recount,
}

/// Keeps the file-type `totals` counted for `data` of scan `generation` after `edits` removals.
/// A result of an older scan or for data the window no longer shows is `Late` (`ScanControl::reset`
/// keeps the generation, so the generation alone does not tell); one counted before a later
/// removal is `Recount`. Only `Kept` keeps them.
fn store_totals(generation: u64, data: &Arc<Data>, edits: u64, totals: Vec<TypeTotal>) -> Stored {
    SESSION.with_borrow_mut(|s| {
        let Some(l) = s.scan.is_current(generation).then_some(s.loaded.as_mut()).flatten() else {
            return Stored::Late;
        };
        if l.edits != edits {
            Stored::Recount
        } else if !Arc::ptr_eq(&l.data, data) {
            Stored::Late
        } else {
            l.totals = Some(totals);
            Stored::Kept
        }
    })
}

/// The file types `gone` (those of a removed subtree) left the shown tree: they come off the
/// totals. While the totals are counted, nothing comes off: that count is late (`store_totals`).
pub(crate) fn drop_types(l: &mut Loaded, gone: &[TypeTotal]) {
    l.edits += 1;
    if let Some(totals) = l.totals.as_mut() {
        *totals = footer::subtract(totals, gone);
    }
}

/// The footer chips of the shown tree, from its totals; `None` while they are counted.
fn chips_of(l: &Loaded) -> Option<Vec<footer::Chip>> {
    let tree = &l.data.tree;
    l.totals.as_ref().map(|t| footer::chips(t, tree.node(tree.root()).size, FOOTER_CHIPS))
}

/// Pushes the footer chips into the window, in the tile colours of the current theme.
fn sync_types(ui: &MainWindow) {
    let theme = if ui.get_dark() { Theme::Dark } else { Theme::Light };
    let unit = current_unit();
    let (counting, chips) = with_loaded(|l| match chips_of(l) {
        None => (true, Vec::new()),
        Some(chips) => (false, chips.iter().map(|c| type_chip(c, theme, unit)).collect()),
    })
    .unwrap_or_default();
    ui.set_types_counting(counting);
    ui.set_type_chips(ModelRc::new(VecModel::from(chips)));
}

/// The unit every size in the window shows in: the session setting. Not to be called while the
/// session is borrowed (the tree rows keep their own copy, `RowModel::unit`).
fn current_unit() -> Unit {
    SESSION.with_borrow(|s| s.settings.unit)
}

/// The window's form of chip `c`, in its tile colour in `theme`.
fn type_chip(c: &footer::Chip, theme: Theme, unit: Unit) -> TypeChip {
    let [r, g, b] = treemap::filetype::colour(c.category, theme);
    TypeChip {
        label: c.label.as_str().into(),
        percent: c.percent.as_str().into(),
        share: c.share,
        color: slint::Color::from_rgb_u8(r, g, b),
        spoken: footer::spoken(c, unit).into(),
    }
}

/// Drops the shown scan.
fn clear(ui: &MainWindow) {
    stop_move(ui);
    let tip = forget_scan(|n| show_hover(ui, n));
    ui.set_rows(ModelRc::default());
    ui.set_treemap(Image::default());
    graph_ui::clear_picture(ui);
    ui.set_crumbs(ModelRc::default());
    ui.set_labels(ModelRc::default());
    ui.set_file_labels(ModelRc::default());
    ui.set_map_note(SharedString::new());
    ui.set_map_value(SharedString::new());
    sync_types(ui);
    do_tip(ui, tip);
    ui.set_has_selection(false);
    ui.set_selected_row(-1);
    delete_ui::sync(ui);
}

/// Queues `event` for the UI thread. Dropped if a newer scan has started or the scan was cancelled.
fn post(ui: &Weak<MainWindow>, generation: u64, event: Event) {
    let ui = ui.clone();
    // Err means the event loop has stopped: nothing left to update.
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            apply(&ui, generation, event);
        }
    });
}

/// Like `post` for `Finished`, and installs the scanned tree in the window. Both are dropped if
/// a newer scan has started or the scan was cancelled (`ScanControl::finish` decides).
/// `root` is the scan root as the worker resolved it (`Session::resolved_root`).
/// The post-scan time of `--timing` starts at `finished`, when the worker's scan returned, so the
/// wait for the UI thread is counted.
fn post_done(ui: &Weak<MainWindow>, generation: u64, data: Data, summary: Summary, root: PathBuf, finished: Instant) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = ui.upgrade() else { return };
        let shown = summary.clone();
        if let Some(data) = SESSION.with_borrow_mut(|s| s.scan.finish(generation, summary, data)) {
            SESSION.with_borrow_mut(|s| {
                s.resolved_root = Some(root);
                if let Some(run) = s.run.as_mut() {
                    run.scan_finished(finished, &shown);
                }
            });
            install(&ui, Arc::new(data));
            show_state(&ui);
        }
    });
}

/// Stops the running scan. The window shows "Scan stopped" at once; the worker ends in the
/// background and its late events are dropped.
fn cancel_scan(ui: &MainWindow) {
    if SESSION.with_borrow_mut(|s| s.scan.cancel()) {
        show_state(ui);
    }
}

fn start_scan(ui: &MainWindow, path: &str) {
    let path = path.trim();
    let Some((generation, cancel)) = SESSION.with_borrow_mut(|s| s.scan.start()) else {
        return;
    };
    SESSION.with_borrow_mut(|s| {
        s.scan_root = PathBuf::from(path);
        s.resolved_root = None;
        s.run = s.flags.timing.then(|| TimingRun::new(Instant::now()));
    });
    show_state(ui);
    clear(ui);
    ui.set_elevate_note(SharedString::new());
    if path.is_empty() {
        apply(ui, generation, Event::Failed("Choose a folder or drive first.".into()));
        return;
    }
    let root = PathBuf::from(path);
    let opts = SESSION.with_borrow(|s| s.flags.scan_options());
    let method = opts.method;
    let weak = ui.as_weak();
    let (tx, rx) = mpsc::channel::<Progress>();

    // Forwarder: latest progress only, at most one UI update per interval.
    let fwd_weak = weak.clone();
    std::thread::spawn(move || {
        while let Ok(mut p) = rx.recv() {
            while let Ok(newer) = rx.try_recv() {
                p = newer;
            }
            post(&fwd_weak, generation, Event::Progress { files: p.files, dirs: p.dirs });
            std::thread::sleep(PROGRESS_INTERVAL);
        }
    });

    std::thread::spawn(move || match scan::scan_with(&root, tx, &cancel, &opts) {
        Ok((tree, stats)) => {
            let finished = Instant::now();
            let summary = Summary::new(&stats, tree.node(tree.root()).size);
            // Counted here, not on the UI thread: it visits every node.
            let counts = tree_view::file_counts(&tree);
            // Resolved here once per scan, not on the UI thread at every event: it asks the file system.
            let resolved = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            post_done(&weak, generation, Data { tree, counts }, summary, resolved, finished);
        }
        Err(ScanError::Cancelled) => post(&weak, generation, Event::Cancelled),
        Err(e) => post(&weak, generation, Event::Failed(scan_error_message(&e, method))),
    });
}

/// Looks for the drives on a worker thread (a sleeping disk can take seconds) and shows them as
/// buttons on the start screen. The old buttons stay until the new list arrives. A result is
/// dropped if a newer request started meanwhile.
fn refresh_drives(ui: &MainWindow) {
    let seq = SESSION.with_borrow_mut(|s| {
        s.drives_seq += 1;
        s.drives_seq
    });
    ui.set_drives_loading(true);
    let unit = current_unit();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let list = drives::list();
        let note = drives::admin_note(&list, elevate::is_admin());
        let buttons = drives::drive_buttons(&list, unit);
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            if SESSION.with_borrow(|s| s.drives_seq) != seq {
                return;
            }
            ui.set_drive_buttons(ModelRc::new(VecModel::from(buttons.into_iter().map(drive_button).collect::<Vec<_>>())));
            ui.set_admin_note(note.into());
            ui.set_drives_loading(false);
        });
    });
}

/// The window's form of drive button `b`.
fn drive_button(b: drives::DriveButton) -> DriveButton {
    DriveButton {
        root: b.root.into(),
        title: b.title.into(),
        detail: b.detail.into(),
        used: b.used,
        size_known: b.size_known,
    }
}

/// "New scan": leaves the finished scan, drops its data and shows the start screen.
fn new_scan(ui: &MainWindow) {
    if SESSION.with_borrow_mut(|s| s.scan.reset()) {
        clear(ui);
        show_state(ui);
    }
}

/// Window position in logical pixels to the treemap's physical pixels.
fn to_physical(ui: &MainWindow, x: f32, y: f32) -> (f64, f64) {
    let scale = f64::from(ui.window().scale_factor());
    (f64::from(x) * scale, f64::from(y) * scale)
}

fn row_node(i: i32) -> Option<NodeId> {
    with_loaded(|l| l.model.rows.borrow().get(usize::try_from(i).ok()?).map(|r| r.node)).flatten()
}

/// Collapses or expands the folder in row `i`.
fn toggle_row(ui: &MainWindow, i: i32) {
    if let Some(n) = row_node(i) {
        with_loaded(|l| l.view.toggle(&l.data.tree, n));
        sync_rows(ui);
    }
}

/// Shows `folder` in the treemap. Lays it out again only if it is not the shown folder.
fn show_folder(ui: &MainWindow, folder: NodeId) {
    if with_loaded(|l| std::mem::replace(&mut l.view_root, folder) != folder) == Some(true) {
        drop_hover(ui);
        move_to_folder(ui, folder);
        relayout(ui);
    }
}

/// The picture starts to move for the change of the shown folder to `folder`
/// (`tree_view::zoom_way`): it zooms in at once, from where a zoom in or out is now; or the move out waits
/// for the new layout; or the picture stays still and a move that shows hides.
fn move_to_folder(ui: &MainWindow, folder: NodeId) {
    let way = with_loaded(|l| tree_view::zoom_way(&l.data.tree, l.shown_root, &l.rects, folder));
    SESSION.with_borrow_mut(|s| s.out_from = None);
    // The treemap is hidden behind the graph: its rects are stale, and no treemap layout lands to
    // end a zoom in or to play a zoom out.
    if SESSION.with_borrow(|s| graph_view::shown_view(s.scan.state(), s.loaded.is_some(), s.settings.view)) == View::Graph {
        return stop_move(ui);
    }
    let (w, h) = map_px(ui);
    match way {
        Some(tree_view::ZoomWay::In((x, y, rw, rh))) if motion_on() => {
            if let Some(to) = treemap::zoom_frame(&Rect { node: folder, x, y, w: rw, h: rh }, w, h) {
                let from = zoom_now(ui).unwrap_or((0.0, 0.0, w, h));
                return start_move(ui, motion::Kind::In, ui.get_treemap(), from, to, (1.0, 1.0));
            }
        }
        Some(tree_view::ZoomWay::Out(left)) => SESSION.with_borrow_mut(|s| s.out_from = Some(left)),
        _ => {}
    }
    stop_move(ui);
}

/// Opens `node` in the treemap (`tree_view::open_target`): a folder shows itself, a file
/// shows its folder with the file selected. Every open path comes here.
fn open_node(ui: &MainWindow, node: NodeId) {
    if let Some((folder, selected)) = with_loaded(|l| tree_view::open_target(&l.data.tree, node)).flatten() {
        show_folder(ui, folder);
        select(ui, selected);
    }
}

/// Shows the parent of the treemap folder and selects the folder it left.
fn go_up(ui: &MainWindow) {
    if let Some((parent, left)) = with_loaded(|l| l.data.tree.node(l.view_root).parent.map(|p| (p, l.view_root))).flatten() {
        show_folder(ui, parent);
        select(ui, left);
    }
}

/// Opens the selected node in the treemap.
fn open_selected(ui: &MainWindow) {
    if let Some(node) = with_loaded(|l| l.selected).flatten() {
        open_node(ui, node);
    }
}

/// A key press in the file tree.
fn tree_key(ui: &MainWindow, key: &str) {
    let current = usize::try_from(ui.get_selected_row()).ok();
    let move_to = |i: usize| {
        if let Some(n) = row_node(i as i32) {
            select(ui, n);
        }
    };
    match (key, current) {
        ("up" | "down", _) => {
            let len = with_loaded(|l| l.model.rows.borrow().len()).unwrap_or(0);
            if let Some(i) = nav::step_row(len, current, if key == "up" { -1 } else { 1 }) {
                move_to(i);
            }
        }
        ("left" | "right", Some(i)) => {
            let action = with_loaded(|l| {
                let rows = l.model.rows.borrow();
                if key == "left" { nav::left_action(&rows, i) } else { nav::right_action(&rows, i) }
            });
            match action {
                Some(TreeMove::Toggle) => toggle_row(ui, i as i32),
                Some(TreeMove::Select(j)) => move_to(j),
                Some(TreeMove::Nothing) | None => {}
            }
        }
        ("space", Some(i)) => toggle_row(ui, i as i32),
        ("enter", Some(_)) => open_selected(ui),
        ("delete" | "shift-delete", _) => delete_ui::request(ui, key == "shift-delete"),
        _ => {}
    }
}

/// A key press in the treemap.
fn map_key(ui: &MainWindow, key: &str) {
    let dir = match key {
        "up" => Dir::Up,
        "down" => Dir::Down,
        "left" => Dir::Left,
        "right" => Dir::Right,
        "enter" => return open_selected(ui),
        "backspace" => return go_up(ui),
        "delete" | "shift-delete" => return delete_ui::request(ui, key == "shift-delete"),
        _ => return,
    };
    if let Some(n) = with_loaded(|l| nav::neighbour_rect(&l.rects, l.selected, dir)).flatten() {
        select(ui, n);
    }
}

fn wire_tree_and_treemap(ui: &MainWindow) {
    ui.on_tree_key({
        let weak = ui.as_weak();
        move |key| {
            if let Some(ui) = weak.upgrade() {
                tip_event(&ui, tip::TipEvent::Key);
                tree_key(&ui, &key);
            }
        }
    });
    ui.on_map_key({
        let weak = ui.as_weak();
        move |key| {
            if let Some(ui) = weak.upgrade() {
                tip_event(&ui, tip::TipEvent::Key);
                map_key(&ui, &key);
            }
        }
    });
    ui.on_tree_select({
        let weak = ui.as_weak();
        move |i| {
            if let (Some(ui), Some(n)) = (weak.upgrade(), row_node(i)) {
                select(&ui, n);
            }
        }
    });
    ui.on_tree_toggle({
        let weak = ui.as_weak();
        move |i| {
            if let Some(ui) = weak.upgrade() {
                toggle_row(&ui, i);
            }
        }
    });
    ui.on_tree_sort({
        let weak = ui.as_weak();
        move |k| {
            let Some(ui) = weak.upgrade() else { return };
            let key = match k {
                0 => SortKey::Name,
                1 => SortKey::Size,
                2 => SortKey::Percent,
                _ => SortKey::Files,
            };
            with_loaded(|l| l.view.sort_by(key));
            sync_rows(&ui);
        }
    });
    ui.on_treemap_click({
        let weak = ui.as_weak();
        move |x, y| {
            let Some(ui) = weak.upgrade() else { return };
            if let Some(n) = node_at(&ui, x, y) {
                select(&ui, n);
            }
        }
    });
    ui.on_treemap_menu({
        let weak = ui.as_weak();
        move |x, y, wx, wy| {
            let Some(ui) = weak.upgrade() else { return };
            // The tile a left-click there hits, also during a zoom.
            let hit = node_at(&ui, x, y);
            open_menu(&ui, Source::Map, hit, menu::At::Pointer(wx, wy));
        }
    });
    ui.on_tree_menu({
        let weak = ui.as_weak();
        move |i, x, y| {
            if let Some(ui) = weak.upgrade() {
                open_menu(&ui, Source::Row(i), None, menu::At::Pointer(x, y));
            }
        }
    });
    ui.on_crumb_menu({
        let weak = ui.as_weak();
        move |i, x, y| {
            if let Some(ui) = weak.upgrade() {
                open_menu(&ui, Source::Crumb(i), None, menu::At::Pointer(x, y));
            }
        }
    });
    ui.on_crumb_menu_key({
        let weak = ui.as_weak();
        move |i, x, y, w, h| {
            if let Some(ui) = weak.upgrade() {
                open_menu(&ui, Source::Crumb(i), None, menu::At::Box(x, y, w, h));
            }
        }
    });
    // The Menu key or Shift+F10 in the tree or the map: the selected item, under its box.
    ui.on_menu_key({
        let weak = ui.as_weak();
        move |x, y, w, h| {
            if let Some(ui) = weak.upgrade() {
                open_menu(&ui, Source::Key, None, menu::At::Box(x, y, w, h));
            }
        }
    });
    ui.on_treemap_open({
        let weak = ui.as_weak();
        move |x, y| {
            let Some(ui) = weak.upgrade() else { return };
            // The rects of the shown image, even while a newer layout is on its way.
            if let Some(n) = node_at(&ui, x, y) {
                open_node(&ui, n);
            }
        }
    });
    ui.on_crumb_open({
        let weak = ui.as_weak();
        move |i| {
            let Some(ui) = weak.upgrade() else { return };
            // The crumbs belong to the shown scan (`clear` drops them with it), so the index is
            // a node of the shown tree; the bound check keeps a bad index from panicking.
            let node = u32::try_from(i).ok().map(NodeId);
            if let Some(n) = node.filter(|n| with_loaded(|l| (n.0 as usize) < l.data.counts.len()) == Some(true)) {
                open_node(&ui, n);
            }
        }
    });
    ui.on_treemap_hover({
        let weak = ui.as_weak();
        move |x, y| {
            if let Some(ui) = weak.upgrade() {
                hover(&ui, x, y);
            }
        }
    });
    ui.on_treemap_leave({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                leave(&ui);
            }
        }
    });
    ui.on_treemap_hide_tip({
        let weak = ui.as_weak();
        move |scrolled| {
            if let Some(ui) = weak.upgrade() {
                tip_event(&ui, if scrolled { tip::TipEvent::Scroll } else { tip::TipEvent::Click });
            }
        }
    });
    ui.on_tip_position(tree_view::tip_position);
    ui.on_tip_break_points(|path| tip::break_points(&path).into());
    ui.on_zoom_frame(|m, t| frame_at(&m, t));
    ui.on_place_label(|p, kind, from_w, from_h, to_w, to_h| place_label(p, kind, (from_w, from_h), (to_w, to_h)));
    ui.on_theme_changed({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                relayout(&ui);
                sync_types(&ui);
            }
        }
    });
    ui.on_treemap_resized({
        let weak = ui.as_weak();
        move |w, h| {
            SESSION.with_borrow_mut(|s| s.map_size = (w, h));
            // Width and height change one after the other: one layout for both.
            let weak = weak.clone();
            RESIZE_TIMER.with(|t| {
                t.start(TimerMode::SingleShot, RESIZE_DEBOUNCE, move || {
                    if let Some(ui) = weak.upgrade() {
                        relayout(&ui);
                    }
                })
            });
        }
    });
    ui.on_restart_admin({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let root = SESSION.with_borrow(|s| relaunch_path(ui.get_start_screen(), &s.scan_root));
            match restart_admin(&root, elevate::restart_as_admin) {
                Ok(()) => {
                    let _ = slint::quit_event_loop();
                }
                Err(e) => {
                    eprintln!("Restart as administrator failed: {e}");
                    ui.set_elevate_note(
                        "Restart was cancelled. The app is still running without administrator rights.".into(),
                    );
                }
            }
        }
    });
}

/// Opens the folder dialog without blocking the UI thread. The dialog belongs to the window.
/// A press while a dialog is open does nothing.
fn start_browse(ui: &MainWindow) {
    if !SESSION.with_borrow_mut(|s| s.browse.try_open()) {
        return;
    }
    sync_browsing(ui);
    let dialog = rfd::AsyncFileDialog::new().set_parent(&ui.window().window_handle()).pick_folder();
    let weak = ui.as_weak();
    let task = slint::spawn_local(async move {
        let picked = dialog.await.map(|f| f.path().to_path_buf());
        if let Some(ui) = weak.upgrade() {
            finish_browse(&ui, picked);
        }
    });
    if let Err(e) = task {
        eprintln!("Could not open the folder dialog: {e}");
        finish_browse(ui, None);
    }
}

/// The dialog closed. A chosen folder is scanned; a cancelled dialog changes nothing.
fn finish_browse(ui: &MainWindow, picked: Option<PathBuf>) {
    let dir = SESSION.with_borrow_mut(|s| s.browse.finish(picked));
    sync_browsing(ui);
    if let Some(dir) = dir {
        start_scan(ui, &dir.to_string_lossy());
    }
}

/// Disables the "Choose folder" button while a dialog is open.
fn sync_browsing(ui: &MainWindow) {
    ui.set_browsing(SESSION.with_borrow(|s| s.browse.is_open()));
}

/// Keeps the options and notices of the command line for the whole run.
fn apply_args(args: &cli::Args) {
    SESSION.with_borrow_mut(|s| {
        s.flags = args.flags;
        s.notices = args.notices.clone();
    });
}

/// A GUI-subsystem exe has no console. Started from a terminal, join the terminal's so `--timing`
/// and the renderer note still print there. Fails (and is ignored) when started from Explorer.
#[cfg(not(debug_assertions))]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: AttachConsole takes a process id constant and has no pointer arguments.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Whether the process should attempt an elevated restart on startup.
fn should_auto_elevate(always_admin: bool, is_admin: bool, no_elevate: bool) -> bool {
    always_admin && !is_admin && !no_elevate
}

/// Attempts to restart as administrator if the setting is enabled and the process is not elevated.
/// Returns true if an elevated copy was started (the current process should exit).
/// If elevation fails or is declined, appends a notice to `args.notices` and returns false.
fn auto_elevate_on_start(
    always_admin: bool,
    is_admin: bool,
    args: &mut cli::Args,
    launch: impl FnOnce(&str, cli::Flags) -> Result<(), String>,
) -> bool {
    if !should_auto_elevate(always_admin, is_admin, args.flags.no_elevate) {
        return false;
    }
    let scan_path = args.path.as_deref().unwrap_or("");
    let mut flags = args.flags;
    flags.no_elevate = true;
    match launch(scan_path, flags) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("Auto-restart as administrator was declined or failed: {e}");
            args.notices.push("Administrator rights were declined. Running as standard user.".into());
            false
        }
    }
}

fn main() {
    #[cfg(not(debug_assertions))]
    attach_parent_console();
    let mut args = cli::parse_args(std::env::args().skip(1));

    // The settings are read before the first size is formatted (the banner, the drive cards).
    let path = settings::settings_path();
    let loaded = path.as_deref().map_or((Settings::default(), settings::Loaded::Fresh), settings::load);
    report_load(&loaded.1);
    if auto_elevate_on_start(loaded.0.always_admin, elevate::is_admin(), &mut args, elevate::restart_as_admin) {
        return;
    }

    apply_args(&args);
    let settings_error = SESSION.with_borrow_mut(|s| adopt_settings(s, path, loaded));
    let ui = MainWindow::new().expect("create window");
    ui.set_settings_error(settings_error.into());
    apply_settings(&ui);
    wire_settings(&ui);
    // The renderer is chosen when the window is made (winit falls back to software when OpenGL
    // cannot start), so the answer is final here. The notifier counts the frames drawn and starts
    // the folder argument's scan after the first one.
    let first_frame = ui.as_weak();
    let notifier = ui.window().set_rendering_notifier(move |state, _| {
        if matches!(state, slint::RenderingState::AfterRendering) {
            FRAMES.with(|f| f.set(f.get() + 1));
            if let Some(path) = take_launch_path() {
                let weak = first_frame.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    if let Some(ui) = weak.upgrade() {
                        start_scan(&ui, &path);
                    }
                });
            }
        }
    });
    if !glow_drawn(notifier) {
        eprintln!("This renderer draws no shadows. The hovered tile gets a border.");
        ui.set_glow(false);
    }
    // A folder to scan opens on the results screen: the start screen would show for one frame.
    if shows_start_at_launch(args.path.as_deref()) {
        ui.set_banner(with_options(banner_text(&ScanState::Idle, false, current_unit()), &ScanState::Idle, None, &args.notices).into());
    } else {
        ui.set_start_screen(false);
    }
    ui.on_scan_drive({
        let weak = ui.as_weak();
        move |root| {
            if let Some(ui) = weak.upgrade() {
                start_scan(&ui, &root);
            }
        }
    });
    ui.on_rescan({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                rescan(&ui);
            }
        }
    });
    ui.on_new_scan({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                new_scan(&ui);
            }
        }
    });
    ui.on_cancel({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                cancel_scan(&ui);
            }
        }
    });
    ui.on_choose_folder({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                start_browse(&ui);
            }
        }
    });
    wire_tree_and_treemap(&ui);
    graph_ui::wire(&ui);
    delete_ui::wire(&ui, delete_ui::Deps::real());

    ui.show().expect("show window");
    let weak = ui.as_weak();
    slint::invoke_from_event_loop(move || {
        let Some(ui) = weak.upgrade() else { return };
        apply_theme(&ui);
        // A change of the Windows theme: winit has just set the title bar to the system theme, so
        // the setting is applied again, after Slint has seen the event too.
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
        let weak = ui.as_weak();
        ui.window().on_winit_window_event(move |_, event| {
            if matches!(event, WindowEvent::ThemeChanged(_)) {
                let weak = weak.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    if let Some(ui) = weak.upgrade() {
                        apply_theme(&ui);
                    }
                });
            }
            EventResult::Propagate
        });
    })
    .expect("queue theme and Mica setup");
    // Started with a folder (also by "Restart as administrator"): scan it after the first frame,
    // so the window draws before the walker takes the CPUs. A window that never draws
    // (minimised, hidden) starts it after `LAUNCH_FALLBACK`.
    if args.path.is_some() {
        SESSION.with_borrow_mut(|s| s.launch_path = args.path);
        let weak = ui.as_weak();
        Timer::single_shot(LAUNCH_FALLBACK, move || {
            if let (Some(ui), Some(path)) = (weak.upgrade(), take_launch_path()) {
                start_scan(&ui, &path);
            }
        });
    } else {
        refresh_drives(&ui);
    }
    slint::run_event_loop().expect("run event loop");
    SESSION.with_borrow(|s| {
        if s.flags.timing {
            let slowest = s.hover_slowest.as_secs_f64() * 1000.0;
            eprintln!("Hover changes: {}. Slowest hover: {slowest:.2} ms.", s.hover_changes);
            eprintln!("Frames drawn: {}.", FRAMES.with(Cell::get));
        }
    });
}

/// The command-line folder to scan, the first time it is asked for: on the first frame or when
/// `LAUNCH_FALLBACK` is up. Every later call gets `None`.
fn take_launch_path() -> Option<String> {
    SESSION.with_borrow_mut(|s| s.launch_path.take())
}

/// The folder the elevated copy scans: none from the start screen, so the copy opens on the start
/// screen; else the root of the shown scan, as typed.
fn relaunch_path(start_screen: bool, scan_root: &std::path::Path) -> String {
    if start_screen { String::new() } else { scan_root.to_string_lossy().into_owned() }
}

/// Starts the elevated copy through `launch(path, flags)`. The options are carried over from the session.
fn restart_admin(path: &str, launch: impl FnOnce(&str, cli::Flags) -> Result<(), String>) -> Result<(), String> {
    launch(path, SESSION.with_borrow(|s| s.flags))
}

#[cfg(test)]
mod tests {
    use super::{Unit,
        Data, FILE_LABEL_TWO_LINES, FileLabel, FrameLabel, Hover, LABEL_CAP, ZoomFrame, ZoomMove, frame_at,
        Loaded, RowModel, SESSION, file_labels, frame_labels, glow_drawn, hover_of, map_labels, note_hover, restart_admin,
        Move, picture_point, zoom_frame_at, take_launch_path, forget_scan, gate_hover, left_map,
        shows_start_at_launch, relaunch_path, LabelKind, LogicalBox, Placed, place_label, stretch,
        apply_args, Stored, drop_types, store_totals, begin_layout,
        Source, target_of, hit_at, run_action, action_note, rescan_shown, rescan_target, Request, menu_request, FollowStep, check_follow,
    };
    use crate::menu::{self, Cmd, Disk};
    use treemap::filetype::{TypeTotal, type_totals};
    use crate::tip;
    use crate::motion::Kind;
    use std::time::Instant;
    use slint::SetRenderingNotifierError;
    use scan::{NodeId, Tree};
    use treemap::{FILE_LABEL_MIN_HEIGHT, FILE_LABEL_MIN_WIDTH, Theme};
    use crate::model::{Event, Summary};
    use crate::tree_view::ViewState;
    use scan::Method;
    use slint::ModelNotify;
    use std::rc::Rc;
    use std::sync::Arc;
    use treemap::filetype::Category;
    use scan::rollup::{RawEntry, rollup};
    use std::cell::RefCell;
    use treemap::{Chrome, Rect};

    /// The labels of one folder "f" drawn as `w` x `h` physical pixels at `scale`:
    /// (x, y, width, height) in logical pixels.
    fn labels_of_one_frame(w: f64, h: f64, scale: f64) -> Vec<(f32, f32, f32, f32)> {
        let entries = vec![
            RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true, modified: scan::time::UNKNOWN },
            RawEntry { id: 1, parent: 0, name: "f".into(), size: 0, allocated: 0, is_dir: true, modified: scan::time::UNKNOWN },
            RawEntry { id: 2, parent: 1, name: "g".into(), size: 10, allocated: 10, is_dir: false, modified: scan::time::UNKNOWN },
        ];
        let tree = rollup(0, entries).unwrap().tree;
        let f = tree.children(tree.root()).next().unwrap();
        let rects = [Rect { node: f, x: 0.0, y: 0.0, w, h }];
        frame_labels(&tree, &rects, Chrome::scaled(scale), scale, Unit::Dynamic).iter().map(|l| (l.x, l.y, l.width, l.height)).collect()
    }

    /// 28 x 24 is the smallest frame with a band; its band is 24 px, the narrowest label.
    #[test]
    fn frame_labels_narrowest_banded_frame_gets_a_24_px_label() {
        assert_eq!(labels_of_one_frame(28.0, 24.0, 1.0), vec![(2.0, 0.0, 24.0, 16.0)]);
    }

    #[test]
    fn frame_labels_at_scale_two_narrowest_banded_frame_gets_a_24_px_label() {
        assert_eq!(labels_of_one_frame(56.0, 48.0, 2.0), vec![(2.0, 0.0, 24.0, 16.0)]);
    }

    // ---- file labels ----
    /// Under the root: `folders` empty folders "d0", "d1", …, then files "clip0.mp4", "clip1.mp4", …
    /// of the sizes in `files`. Returns the tree, the folders and the files, in that order.
    fn flat(folders: usize, files: &[u64]) -> (Tree, Vec<NodeId>, Vec<NodeId>) {
        let mut e = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true, modified: scan::time::UNKNOWN }];
        e.extend((0..folders).map(|i| RawEntry { id: i as u64 + 1, parent: 0, name: format!("d{i}"), size: 0, allocated: 0, is_dir: true, modified: scan::time::UNKNOWN }));
        e.extend(files.iter().enumerate().map(|(i, &s)| RawEntry {
            id: (folders + i) as u64 + 1,
            parent: 0,
            name: format!("clip{i}.mp4"),
            size: s,
            allocated: s,
            is_dir: false,
            modified: scan::time::UNKNOWN,
        }));
        let tree = rollup(0, e).unwrap().tree;
        let by_name = |n: String| tree.children(tree.root()).find(|&c| tree.name(c) == n).unwrap();
        let dirs = (0..folders).map(|i| by_name(format!("d{i}"))).collect();
        let fs = (0..files.len()).map(|i| by_name(format!("clip{i}.mp4"))).collect();
        (tree, dirs, fs)
    }

    /// The file labels of one file "clip0.mp4" of 10 bytes drawn as the physical rect (x, y, w, h).
    fn file_labels_of_one_tile(x: f64, y: f64, w: f64, h: f64, scale: f64, theme: Theme) -> Vec<FileLabel> {
        let (tree, _, files) = flat(0, &[10]);
        file_labels(&tree, &[Rect { node: files[0], x, y, w, h }], scale, theme, usize::MAX, Unit::Dynamic)
    }

    fn geometry(labels: &[FileLabel]) -> Vec<(f32, f32, f32, f32, bool)> {
        labels.iter().map(|l| (l.x, l.y, l.width, l.height, l.two_lines)).collect()
    }

    #[test]
    fn file_labels_at_scale_two_are_in_logical_pixels() {
        let labels = file_labels_of_one_tile(10.0, 20.0, 80.0, 28.0, 2.0, Theme::Dark);
        assert_eq!(geometry(&labels), vec![(5.0, 10.0, 40.0, 14.0, false)]);
    }

    #[test]
    fn file_labels_text_is_the_file_name_and_size() {
        let labels = file_labels_of_one_tile(0.0, 0.0, 200.0, 100.0, 1.0, Theme::Dark);
        let text: Vec<(String, String)> = labels.iter().map(|l| (l.name.to_string(), l.size.to_string())).collect();
        assert_eq!(text, vec![("clip0.mp4".to_string(), "10 B".to_string())]);
    }

    #[test]
    fn file_labels_least_tile_at_scale_one_and_a_half_is_kept() {
        let (w, h) = (FILE_LABEL_MIN_WIDTH * 1.5, FILE_LABEL_MIN_HEIGHT * 1.5);
        let labels = file_labels_of_one_tile(0.0, 0.0, w, h, 1.5, Theme::Dark);
        assert_eq!(geometry(&labels), vec![(0.0, 0.0, FILE_LABEL_MIN_WIDTH as f32, FILE_LABEL_MIN_HEIGHT as f32, false)]);
    }

    #[test]
    fn file_labels_tile_under_the_least_size_at_scale_one_and_a_half_gets_none() {
        let (w, h) = (FILE_LABEL_MIN_WIDTH * 1.5, FILE_LABEL_MIN_HEIGHT * 1.5);
        let narrow = file_labels_of_one_tile(0.0, 0.0, w - 0.01, h, 1.5, Theme::Dark);
        let low = file_labels_of_one_tile(0.0, 0.0, w, h - 0.01, 1.5, Theme::Dark);
        assert_eq!((narrow.len(), low.len()), (0, 0));
    }

    #[test]
    fn file_labels_tile_as_high_as_two_lines_gets_two_lines() {
        let labels = file_labels_of_one_tile(0.0, 0.0, 200.0, FILE_LABEL_TWO_LINES * 2.0, 2.0, Theme::Dark);
        assert_eq!(labels.iter().map(|l| l.two_lines).collect::<Vec<_>>(), vec![true]);
    }

    #[test]
    fn file_labels_tile_under_two_lines_high_gets_one_line() {
        let labels = file_labels_of_one_tile(0.0, 0.0, 200.0, FILE_LABEL_TWO_LINES * 2.0 - 0.01, 2.0, Theme::Dark);
        assert_eq!(labels.iter().map(|l| l.two_lines).collect::<Vec<_>>(), vec![false]);
    }

    #[test]
    fn file_labels_ink_is_the_ink_of_the_tile_colour_in_each_theme() {
        let ink = |theme: Theme| {
            let [r, g, b] = treemap::filetype::ink(treemap::filetype::colour(Category::Video, theme));
            slint::Color::from_rgb_u8(r, g, b)
        };
        let got: Vec<slint::Color> = [Theme::Dark, Theme::Light]
            .into_iter()
            .flat_map(|t| file_labels_of_one_tile(0.0, 0.0, 200.0, 100.0, 1.0, t))
            .map(|l| l.ink)
            .collect();
        assert_eq!(got, vec![ink(Theme::Dark), ink(Theme::Light)]);
    }

    #[test]
    fn file_labels_folder_gets_none() {
        let (tree, dirs, _) = flat(1, &[]);
        let labels = file_labels(&tree, &[Rect { node: dirs[0], x: 0.0, y: 0.0, w: 400.0, h: 300.0 }], 1.0, Theme::Dark, usize::MAX, Unit::Dynamic);
        assert!(labels.is_empty());
    }

    /// `frames` folders with a band (100 x 50 each) and files "clip{i}.mp4" of 50 + i by 20 px, at 100%.
    fn map_of(frames: usize, files: usize) -> (Vec<FrameLabel>, Vec<FileLabel>) {
        let (tree, dirs, fs) = flat(frames, &vec![10; files]);
        let mut rects: Vec<Rect> = dirs.iter().map(|&n| Rect { node: n, x: 0.0, y: 0.0, w: 100.0, h: 50.0 }).collect();
        rects.extend(fs.iter().enumerate().map(|(i, &n)| Rect { node: n, x: 0.0, y: 0.0, w: 50.0 + i as f64, h: 20.0 }));
        map_labels(&tree, &rects, Chrome::scaled(1.0), 1.0, Theme::Dark, Unit::Dynamic)
    }

    #[test]
    fn map_labels_frames_come_first_and_files_fill_the_room_left() {
        let (frames, files) = map_of(LABEL_CAP - 10, 30);
        assert_eq!((frames.len(), files.len()), (LABEL_CAP - 10, 10));
    }

    #[test]
    fn map_labels_files_in_the_room_left_are_the_largest() {
        let (_, files) = map_of(LABEL_CAP - 3, 8);
        let names: Vec<String> = files.iter().map(|l| l.name.to_string()).collect();
        assert_eq!(names, ["clip5.mp4", "clip6.mp4", "clip7.mp4"]);
    }

    #[test]
    fn map_labels_frames_over_the_cap_leave_no_room_for_files() {
        let (frames, files) = map_of(LABEL_CAP + 5, 5);
        assert_eq!((frames.len(), files.len()), (LABEL_CAP, 0));
    }

    #[test]
    fn map_labels_few_labels_keep_every_frame_and_file() {
        let (frames, files) = map_of(2, 3);
        assert_eq!((frames.len(), files.len()), (2, 3));
    }

    // ---- label cap ----
    /// The C: drive needs 630 labels at 4K and 200% (554 frames and 76 files); the cap keeps a
    /// 15% margin under the 50 ms budget (P8.P6b fix 1).
    #[test]
    fn label_cap_covers_the_c_drive_and_keeps_the_time_margin() {
        assert!((630..=650).contains(&LABEL_CAP), "{LABEL_CAP}");
    }

    // ---- names with hidden characters reach the window as placeholders ----
    /// root(0){ folder "d\u{202E}x"(1){ file "g"(3) }, file "invoice\u{202E}fdp.exe"(2) }
    fn spoofed() -> (Tree, NodeId, NodeId) {
        let e = |id: u64, parent: u64, name: &str, is_dir: bool| RawEntry { id, parent, name: name.into(), size: 10, allocated: 10, is_dir, modified: scan::time::UNKNOWN };
        let entries = vec![e(0, 0, "root", true), e(1, 0, "d\u{202E}x", true), e(2, 0, "invoice\u{202E}fdp.exe", false), e(3, 1, "g", false)];
        let tree = rollup(0, entries).unwrap().tree;
        let find = |n: &str| tree.children(tree.root()).find(|&c| tree.name(c) == n).unwrap();
        let (folder, file) = (find("d\u{202E}x"), find("invoice\u{202E}fdp.exe"));
        (tree, folder, file)
    }

    #[test]
    fn row_model_row_name_shows_a_placeholder_and_the_tree_keeps_the_name() {
        let (tree, folder, file) = spoofed();
        let data = Arc::new(Data { counts: crate::tree_view::file_counts(&tree), tree });
        let row = |node| crate::tree_view::Row { node, depth: 0, expanded: false, has_children: false };
        let model = RowModel {
            data: RefCell::new(Arc::downgrade(&data)),
            rows: RefCell::new(vec![row(folder), row(file)]),
            notify: ModelNotify::default(),
            unit: std::cell::Cell::new(Unit::Dynamic),
        };

        let names: Vec<String> = (0..2).map(|i| slint::Model::row_data(&model, i).unwrap().name.to_string()).collect();

        assert_eq!(names, ["d\u{FFFD}x", "invoice\u{FFFD}fdp.exe"]);
        assert_eq!(data.tree.name(file), "invoice\u{202E}fdp.exe");
    }

    #[test]
    fn frame_labels_name_shows_a_placeholder() {
        let (tree, folder, _) = spoofed();
        let rects = [Rect { node: folder, x: 0.0, y: 0.0, w: 200.0, h: 100.0 }];
        let names: Vec<String> = frame_labels(&tree, &rects, Chrome::scaled(1.0), 1.0, Unit::Dynamic).iter().map(|l| l.name.to_string()).collect();
        assert_eq!(names, ["d\u{FFFD}x\\"]);
    }

    #[test]
    fn file_labels_name_shows_a_placeholder() {
        let (tree, _, file) = spoofed();
        let rects = [Rect { node: file, x: 0.0, y: 0.0, w: 200.0, h: 100.0 }];
        let names: Vec<String> = file_labels(&tree, &rects, 1.0, Theme::Dark, usize::MAX, Unit::Dynamic).iter().map(|l| l.name.to_string()).collect();
        assert_eq!(names, ["invoice\u{FFFD}fdp.exe"]);
    }

    #[test]
    fn hover_of_label_name_shows_a_placeholder() {
        let (tree, _, file) = spoofed();
        let data = Data { counts: crate::tree_view::file_counts(&tree), tree };
        let hover = hover_of(&data, &[Rect { node: file, x: 0.0, y: 0.0, w: 200.0, h: 100.0 }], file, 1.0, Theme::Dark, Unit::Dynamic);
        assert_eq!(hover.label.name.as_str(), "invoice\u{FFFD}fdp.exe");
    }

    // ---- hover ----
    #[test]
    fn note_hover_changes_only_when_the_node_changes() {
        let (a, b) = (Some(NodeId(1)), Some(NodeId(2)));
        let changed: Vec<bool> = [a, a, b, b, None, None, a].into_iter().map(note_hover).collect();
        assert_eq!(changed, [true, false, true, false, true, false, true]);
    }

    #[test]
    fn note_hover_leave_with_nothing_hovered_is_no_change() {
        assert!(!note_hover(None));
    }

    #[test]
    fn note_hover_counts_each_change() {
        let (a, b) = (Some(NodeId(1)), Some(NodeId(2)));
        for node in [a, a, b, b, None, None, a] {
            note_hover(node);
        }
        assert_eq!(SESSION.with_borrow(|s| s.hover_changes), 4);
    }

    /// `hover_of` for one file "clip0.mp4" of 10 bytes, or one empty folder "d0", drawn as the
    /// physical rect `(x, y, w, h)`.
    fn hover_of_one(folder: bool, (x, y, w, h): (f64, f64, f64, f64), scale: f64, theme: Theme) -> Hover {
        let (tree, dirs, files) = if folder { flat(1, &[]) } else { flat(0, &[10]) };
        let node = if folder { dirs[0] } else { files[0] };
        let data = Data { counts: crate::tree_view::file_counts(&tree), tree };
        hover_of(&data, &[Rect { node, x, y, w, h }], node, scale, theme, Unit::Dynamic)
    }

    fn place(h: &Hover) -> (bool, f32, f32, f32, f32) {
        (h.visible, h.x, h.y, h.width, h.height)
    }

    #[test]
    fn hover_of_file_tile_at_scale_two_is_in_logical_pixels() {
        let hover = hover_of_one(false, (10.0, 20.0, 80.0, 28.0), 2.0, Theme::Dark);
        assert_eq!(place(&hover), (true, 5.0, 10.0, 40.0, 14.0));
    }

    #[test]
    fn hover_of_file_tile_lifts() {
        assert!(hover_of_one(false, (0.0, 0.0, 80.0, 28.0), 1.0, Theme::Dark).lift);
    }

    #[test]
    fn hover_of_folder_frame_is_shown_and_does_not_lift() {
        let hover = hover_of_one(true, (0.0, 0.0, 400.0, 300.0), 1.0, Theme::Dark);
        assert_eq!((hover.visible, hover.lift), (true, false));
    }

    #[test]
    fn hover_of_file_colour_is_its_file_type_colour_in_each_theme() {
        let colour = |theme: Theme| {
            let [r, g, b] = treemap::filetype::colour(Category::Video, theme);
            slint::Color::from_rgb_u8(r, g, b)
        };
        let got = [Theme::Dark, Theme::Light].map(|t| hover_of_one(false, (0.0, 0.0, 80.0, 28.0), 1.0, t).color);
        assert_eq!(got, [colour(Theme::Dark), colour(Theme::Light)]);
    }

    #[test]
    fn hover_of_tile_big_enough_keeps_its_label_in_tile_coordinates() {
        let hover = hover_of_one(false, (100.0, 50.0, 200.0, 100.0), 2.0, Theme::Dark);
        let l = &hover.label;
        assert!(hover.labelled);
        assert_eq!((l.name.as_str(), l.x, l.y, l.width, l.height, l.two_lines), ("clip0.mp4", 0.0, 0.0, 100.0, 50.0, true));
    }

    #[test]
    fn hover_of_tile_too_narrow_for_a_label_has_none() {
        let hover = hover_of_one(false, (0.0, 0.0, FILE_LABEL_MIN_WIDTH - 0.01, 100.0), 1.0, Theme::Dark);
        assert!(!hover.labelled);
    }

    #[test]
    fn hover_of_folder_frame_has_no_file_label() {
        assert!(!hover_of_one(true, (0.0, 0.0, 400.0, 300.0), 1.0, Theme::Dark).labelled);
    }

    #[test]
    fn hover_of_tooltip_is_the_path_then_the_size_and_file_type() {
        let hover = hover_of_one(false, (0.0, 0.0, 80.0, 28.0), 1.0, Theme::Dark);
        assert_eq!((hover.title.as_str(), hover.detail.as_str()), ("root\\clip0.mp4", "10 B. MP4 file."));
    }

    #[test]
    fn hover_of_node_not_drawn_is_not_visible() {
        let (tree, _, files) = flat(0, &[10]);
        let data = Data { counts: crate::tree_view::file_counts(&tree), tree };
        assert!(!hover_of(&data, &[], files[0], 1.0, Theme::Dark, Unit::Dynamic).visible);
    }

    // ---- moving picture ----
    // ---- labels on a stretched picture ----
    #[test]
    fn stretch_at_the_layout_size_is_the_same_box() {
        for b in [(0.0, 0.0, 0.0, 0.0), (10.0, 20.0, 30.0, 40.0), (799.5, 0.25, 0.5, 599.75)] {
            assert_eq!(stretch(b, (800.0, 600.0), (800.0, 600.0)), b);
        }
    }

    #[test]
    fn stretch_growth_scales_place_and_size() {
        assert_eq!(stretch((10.0, 20.0, 30.0, 40.0), (100.0, 100.0), (200.0, 300.0)), (20.0, 60.0, 60.0, 120.0));
    }

    #[test]
    fn stretch_shrink_scales_place_and_size() {
        assert_eq!(stretch((10.0, 20.0, 30.0, 40.0), (100.0, 100.0), (50.0, 25.0)), (5.0, 5.0, 15.0, 10.0));
    }

    #[test]
    fn stretch_aspect_change_scales_each_axis_on_its_own() {
        assert_eq!(stretch((10.0, 20.0, 30.0, 40.0), (100.0, 100.0), (150.0, 100.0)), (15.0, 20.0, 45.0, 40.0));
    }

    #[test]
    fn stretch_axis_with_no_layout_size_does_not_scale() {
        let b = (10.0, 20.0, 30.0, 40.0);
        let cases = [
            ((0.0, 0.0), b),
            ((0.0, 100.0), (10.0, 60.0, 30.0, 120.0)),
            ((100.0, 0.0), (20.0, 20.0, 60.0, 40.0)),
            ((f32::NAN, 100.0), (10.0, 60.0, 30.0, 120.0)),
            ((-5.0, 100.0), (10.0, 60.0, 30.0, 120.0)),
        ];
        for (from, want) in cases {
            assert_eq!(stretch(b, from, (200.0, 300.0)), want, "from {from:?}");
        }
    }

    #[test]
    fn stretch_to_a_map_with_no_size_gives_an_empty_box() {
        assert_eq!(stretch((10.0, 20.0, 30.0, 40.0), (100.0, 100.0), (0.0, 0.0)), (0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn stretch_in_logical_pixels_matches_the_physical_picture_at_every_scale() {
        let (b, from, to) = ((12.0, 34.0, 56.0, 78.0), (640.0, 480.0), (1000.0, 300.0));
        let logical = stretch(b, from, to);
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            let s = |(x, y, w, h): LogicalBox| (x * scale, y * scale, w * scale, h * scale);

            let (x, y, w, h) = stretch(s(b), (from.0 * scale, from.1 * scale), (to.0 * scale, to.1 * scale));

            let back = [x / scale - logical.0, y / scale - logical.1, w / scale - logical.2, h / scale - logical.3];
            assert!(back.iter().all(|d| d.abs() < 1e-3), "scale {scale}: {back:?}");
        }
    }

    fn placed(width: f32, height: f32) -> Placed {
        Placed { x: 0.0, y: 0.0, width, height, shown: false, two_lines: false }
    }

    #[test]
    fn place_label_each_kind_shows_only_while_its_box_has_room() {
        let (half_wide, half_high) = ((50.0, 100.0), (100.0, 50.0));
        let cases = [
            // Frame labels: at least 24 wide and 8 high after the stretch.
            (LabelKind::Frame, (48.0, 28.0), half_wide, true),
            (LabelKind::Frame, (46.0, 28.0), half_wide, false),
            (LabelKind::Frame, (48.0, 16.0), half_high, true),
            (LabelKind::Frame, (48.0, 15.8), half_high, false),
            // File labels: at least 40 x 14 after the stretch.
            (LabelKind::File, (80.0, 28.0), half_wide, true),
            (LabelKind::File, (78.0, 28.0), half_wide, false),
            (LabelKind::File, (80.0, 28.0), half_high, true),
            (LabelKind::File, (80.0, 26.0), half_high, false),
            // Outlines always show.
            (LabelKind::Outline, (2.0, 2.0), half_wide, true),
            (LabelKind::Outline, (2.0, 2.0), half_high, true),
            (LabelKind::Outline, (0.0, 0.0), half_high, true),
        ];
        for (kind, (w, h), to, want) in cases {
            assert_eq!(place_label(placed(w, h), kind, (100.0, 100.0), to).shown, want, "{kind:?} {w} x {h} to {to:?}");
        }
    }

    #[test]
    fn place_label_frame_label_keeps_its_name_through_a_modest_vertical_shrink() {
        // A frame label band is 16 px high. Shrink factors 1.0, 0.9, 0.875, 0.7, 0.5 keep the name;
        // 0.3 (4.8 px) is below the floor.
        for (factor, want) in [(1.0, true), (0.9, true), (0.875, true), (0.7, true), (0.5, true), (0.3, false)] {
            let band = Placed { x: 2.0, y: 0.0, width: 200.0, height: 16.0, shown: false, two_lines: false };

            let got = place_label(band, LabelKind::Frame, (800.0, 600.0), (800.0, 600.0 * factor));

            assert_eq!(got.shown, want, "factor {factor}: {got:?}");
        }
    }

    #[test]
    fn place_label_file_label_still_hides_below_14_px_after_a_vertical_shrink() {
        let label = Placed { x: 0.0, y: 0.0, width: 80.0, height: 16.0, shown: false, two_lines: false };

        let got = place_label(label, LabelKind::File, (800.0, 600.0), (800.0, 600.0 * 0.8));

        assert!(!got.shown, "{got:?}");
    }

    #[test]
    fn place_label_keeps_the_label_on_its_stretched_frame() {
        let p = Placed { x: 10.0, y: 20.0, width: 80.0, height: 30.0, shown: false, two_lines: false };

        let got = place_label(p, LabelKind::Frame, (100.0, 100.0), (200.0, 50.0));

        assert_eq!(got, Placed { x: 20.0, y: 10.0, width: 160.0, height: 15.0, shown: true, two_lines: false });
    }

    #[test]
    fn place_label_file_label_has_two_lines_only_while_the_box_is_two_lines_high() {
        let cases = [((80.0, 64.0), (100.0, 50.0), true), ((80.0, 63.9), (100.0, 50.0), false), ((80.0, 20.0), (100.0, 200.0), true)];
        for ((w, h), to, want) in cases {
            assert_eq!(place_label(placed(w, h), LabelKind::File, (100.0, 100.0), to).two_lines, want, "{w} x {h} to {to:?}");
        }
    }

    #[test]
    fn apply_args_keeps_the_flags_and_the_notices_in_the_session() {
        let args = crate::cli::parse_args_with(["--threads", "3", "--method", "walk", "--timing", "--foo"].map(String::from), 8);

        apply_args(&args);

        let (flags, notices) = SESSION.with_borrow(|s| (s.flags, s.notices.clone()));
        assert_eq!((flags.threads, flags.method, flags.timing), (Some(3), Some(Method::Walk), true));
        assert_eq!(notices, vec!["Volatus ignored the unknown option --foo.".to_string()]);
    }

    fn zf(x: f32, y: f32, width: f32, height: f32, opacity: f32) -> ZoomFrame {
        ZoomFrame { x, y, width, height, opacity }
    }

    fn shrink() -> ZoomMove {
        ZoomMove { from: zf(0.0, 0.0, 800.0, 600.0, 1.0), to: zf(100.0, 50.0, 200.0, 150.0, 0.0) }
    }

    #[test]
    fn frame_at_start_of_the_move_is_the_from_frame() {
        assert_eq!(frame_at(&shrink(), 0.0), shrink().from);
    }

    #[test]
    fn frame_at_end_of_the_move_is_the_to_frame() {
        assert_eq!(frame_at(&shrink(), 1.0), shrink().to);
    }

    #[test]
    fn frame_at_half_time_place_and_opacity_are_seven_eighths_of_the_way() {
        // ease_out(0.5) = 0.875
        assert_eq!(frame_at(&shrink(), 0.5), zf(87.5, 43.75, 275.0, 206.25, 0.125));
    }

    // ---- clicks during a zoom ----
    /// The treemap view in physical pixels.
    const VIEW: (f64, f64) = (400.0, 400.0);

    /// Tile 1 at (0, 0, 100, 100) and tile 2 at (100, 0, 300, 400) of a picture of `VIEW`.
    fn two_tiles() -> Vec<Rect> {
        vec![
            Rect { node: NodeId(1), x: 0.0, y: 0.0, w: 100.0, h: 100.0 },
            Rect { node: NodeId(2), x: 100.0, y: 0.0, w: 300.0, h: 400.0 },
        ]
    }

    /// The zoom out of the picture from the frame where tile 1 fills the view, (0, 0, 1600, 1600),
    /// to the whole view.
    fn zoom_out_of_tile_1() -> Move {
        let from = treemap::zoom_frame(&two_tiles()[0], VIEW.0, VIEW.1).unwrap();
        Move { kind: Kind::Out, from, to: (0.0, 0.0, VIEW.0, VIEW.1), running: true, started: Instant::now(), frames: 0 }
    }

    /// The tile of `two_tiles` that a click at view point (x, y) hits at time `t` of `moving`.
    fn tile_under(moving: Option<Move>, t: f64, x: f64, y: f64) -> Option<NodeId> {
        let (px, py) = picture_point(moving, t, VIEW, x, y)?;
        crate::tree_view::hit_test(&two_tiles(), px, py)
    }

    #[test]
    fn zoom_frame_at_start_of_a_zoom_out_is_its_from_frame() {
        assert_eq!(zoom_frame_at(Some(zoom_out_of_tile_1()), 0.0), Some((0.0, 0.0, 1600.0, 1600.0)));
    }

    #[test]
    fn picture_point_click_at_the_start_of_a_zoom_out_hits_the_tile_that_fills_the_view() {
        // (200, 200) is in tile 2 of the full map, but tile 1 fills the view at t = 0.
        assert_eq!(tile_under(Some(zoom_out_of_tile_1()), 0.0, 200.0, 200.0), Some(NodeId(1)));
    }

    #[test]
    fn picture_point_click_half_way_through_a_zoom_out_hits_the_tile_drawn_there() {
        // ease_out(0.5) = 0.875: the frame is (0, 0, 550, 550), so tile 1 is drawn at (0, 0, 137.5, 137.5).
        assert_eq!(tile_under(Some(zoom_out_of_tile_1()), 0.5, 120.0, 120.0), Some(NodeId(1)));
    }

    #[test]
    fn picture_point_click_at_the_end_of_a_zoom_in_hits_the_tile_that_fills_the_view() {
        let full = (0.0, 0.0, VIEW.0, VIEW.1);
        let zoom_in = Move { kind: Kind::In, from: full, to: zoom_out_of_tile_1().from, ..zoom_out_of_tile_1() };
        assert_eq!(tile_under(Some(zoom_in), 1.0, 200.0, 200.0), Some(NodeId(1)));
    }

    #[test]
    fn zoom_frame_at_fade_is_none() {
        let full = (0.0, 0.0, VIEW.0, VIEW.1);
        let fade = Move { kind: Kind::Fade, from: full, to: full, ..zoom_out_of_tile_1() };
        assert_eq!(zoom_frame_at(Some(fade), 0.5), None);
    }

    // ---- glow or border ----
    #[test]
    fn glow_drawn_renderer_with_a_notifier_draws_the_glow() {
        assert!(glow_drawn(Ok(())));
    }

    #[test]
    fn glow_drawn_software_renderer_gets_the_border() {
        assert!(!glow_drawn(Err(SetRenderingNotifierError::Unsupported)));
    }

    #[test]
    fn glow_drawn_notifier_already_set_is_a_renderer_that_draws_the_glow() {
        assert!(glow_drawn(Err(SetRenderingNotifierError::AlreadySet)));
    }

    fn data() -> Arc<Data> {
        let entries = vec![
            RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true, modified: scan::time::UNKNOWN },
            RawEntry { id: 1, parent: 0, name: "a.txt".into(), size: 10, allocated: 10, is_dir: false, modified: scan::time::UNKNOWN },
        ];
        let tree = rollup(0, entries).unwrap().tree;
        Arc::new(Data { counts: crate::tree_view::file_counts(&tree), tree })
    }

    fn loaded(data: &Arc<Data>) -> Loaded {
        let root = data.tree.root();
        let model = Rc::new(RowModel {
            data: RefCell::new(Arc::downgrade(data)),
            rows: RefCell::new(Vec::new()),
            notify: ModelNotify::default(),
            unit: std::cell::Cell::new(Unit::Dynamic),
        });
        Loaded {
            data: data.clone(),
            serial: super::next_serial(),
            model,
            view: ViewState::new(root),
            selected: None,
            view_root: root,
            rects: Vec::new(),
            shown_root: None,
            totals: None,
            edits: 0,
            graph: Default::default(),
        }
    }

    fn totals() -> Vec<TypeTotal> {
        vec![TypeTotal { ext: "txt".into(), bytes: 10, files: 1 }]
    }

    fn summary() -> Summary {
        Summary {
            method: Method::Walk,
            files: 1,
            dirs: 1,
            skipped: 0,
            elapsed: std::time::Duration::ZERO,
            phases: Default::default(),
            total: 10,
        }
    }

    /// Scan 1 finished with `data` on screen. Returns its generation.
    fn show_finished_scan(data: &Arc<Data>) -> u64 {
        SESSION.with_borrow_mut(|s| {
            let (generation, _) = s.scan.start().unwrap();
            assert!(s.scan.apply(generation, Event::Finished(summary())));
            s.loaded = Some(loaded(data));
            generation
        })
    }

    #[test]
    fn store_totals_result_of_the_shown_scan_is_kept() {
        let d = data();
        let generation = show_finished_scan(&d);
        assert_eq!(store_totals(generation, &d, 0, totals()), Stored::Kept);
        assert_eq!(kept_totals(), Some(totals()));
    }

    #[test]
    fn store_totals_late_result_after_reset_is_dropped() {
        let d = data();
        let generation = show_finished_scan(&d);
        SESSION.with_borrow_mut(|s| {
            assert!(s.scan.reset());
            s.loaded = None;
        });
        assert_eq!(store_totals(generation, &d, 0, totals()), Stored::Late);
        assert_eq!(kept_totals(), None);
    }

    #[test]
    fn store_totals_late_result_of_a_scan_before_cancel_and_new_scan_is_dropped() {
        let old = data();
        let old_generation = show_finished_scan(&old);
        let new = data();
        SESSION.with_borrow_mut(|s| {
            assert!(s.scan.reset());
            let (generation, _) = s.scan.start().unwrap();
            assert!(s.scan.cancel());
            let (generation2, _) = s.scan.start().unwrap();
            assert!(generation2 > generation);
            assert!(s.scan.apply(generation2, Event::Finished(summary())));
            s.loaded = Some(loaded(&new));
        });
        assert_eq!(store_totals(old_generation, &old, 0, totals()), Stored::Late);
        assert_eq!(kept_totals(), None);
    }

    #[test]
    fn store_totals_late_result_for_other_data_of_the_same_generation_is_dropped() {
        // reset keeps the generation: the scan id alone cannot tell old data from new data.
        let shown = data();
        let late = data();
        let generation = show_finished_scan(&shown);
        assert_eq!(store_totals(generation, &late, 0, totals()), Stored::Late);
        assert_eq!(kept_totals(), None);
    }

    #[test]
    fn store_totals_result_of_an_older_scan_for_the_shown_data_is_dropped() {
        let d = data();
        let generation = show_finished_scan(&d);
        assert_eq!(store_totals(generation - 1, &d, 0, totals()), Stored::Late);
        assert_eq!(kept_totals(), None);
    }

    #[test]
    fn store_totals_same_data_after_a_theme_change_is_still_kept() {
        // A theme change does not replace the data or the scan: the totals are theme free.
        let d = data();
        let generation = show_finished_scan(&d);
        assert_eq!(store_totals(generation, &d, 0, totals()), Stored::Kept);
        assert_eq!(store_totals(generation, &d, 0, totals()), Stored::Kept);
        assert_eq!(kept_totals(), Some(totals()));
    }

    // ---- footer totals after a removal ----
    /// root(0){ docs(1){ a.txt(4)=100, b.pdf(5)=50 }, c.txt(2)=10, d.exe(3)=30 }
    fn typed() -> Arc<Data> {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry { id, parent, name: name.into(), size, allocated: size, is_dir, modified: scan::time::UNKNOWN };
        let entries = vec![
            e(0, 0, "root", 0, true),
            e(1, 0, "docs", 0, true),
            e(2, 0, "c.txt", 10, false),
            e(3, 0, "d.exe", 30, false),
            e(4, 1, "a.txt", 100, false),
            e(5, 1, "b.pdf", 50, false),
        ];
        let tree = rollup(0, entries).unwrap().tree;
        Arc::new(Data { counts: crate::tree_view::file_counts(&tree), tree })
    }

    fn kept_totals() -> Option<Vec<TypeTotal>> {
        SESSION.with_borrow(|s| s.loaded.as_ref().and_then(|l| l.totals.clone()))
    }

    #[test]
    fn store_totals_counted_before_a_removal_and_landing_after_it_are_dropped_and_counted_again() {
        let d = typed();
        let generation = show_finished_scan(&d);
        let counted = type_totals(&d.tree, d.tree.root());
        super::with_loaded(|l| drop_types(l, &[]));

        let stored = store_totals(generation, &d, 0, counted);

        assert_eq!((stored, kept_totals()), (Stored::Recount, None));
    }

    // ---- scan_apply::remove_from on the window's data (the one way a node leaves the tree) ----
    const DOCS: NodeId = NodeId(1);
    const C_TXT: NodeId = NodeId(2);
    const D_EXE: NodeId = NodeId(3);
    const A_TXT: NodeId = NodeId(4);
    const B_PDF: NodeId = NodeId(5);

    /// `loaded` of `typed()` with nothing else holding the data.
    fn loaded_typed() -> Loaded {
        loaded(&typed())
    }

    /// `scan_apply::remove_from` of `n`; no worker holds the data in these tests.
    fn remove_node(l: &mut Loaded, n: NodeId) -> Option<scan::Detached> {
        crate::scan_apply::remove_from(l, n).expect("no worker holds the data")
    }

    /// `n` is still in the tree (its parents reach the root).
    fn attached(tree: &Tree, n: NodeId) -> bool {
        crate::scan_apply::in_subtree(tree, n, tree.root())
    }

    #[test]
    fn remove_node_reduces_the_file_counts_of_every_ancestor() {
        let mut l = loaded_typed();

        remove_node(&mut l, A_TXT);

        let counts = &l.data.counts;
        assert_eq!([counts[DOCS.0 as usize], counts[0]], [1, 3]);
    }

    #[test]
    fn remove_node_rows_skip_the_node_and_show_the_reduced_size_and_percent_of_each_ancestor() {
        let mut l = loaded_typed();
        l.view.expanded.insert(DOCS);

        remove_node(&mut l, A_TXT);
        l.model.set_rows(crate::tree_view::flatten(&l.data.tree, &l.data.counts, &l.view));

        let rows: Vec<(String, String, String, String)> = (0..slint::Model::row_count(&*l.model))
            .map(|i| {
                let r = slint::Model::row_data(&*l.model, i).unwrap();
                (r.name.to_string(), r.size.to_string(), r.percent.to_string(), r.files.to_string())
            })
            .collect();
        let row = |name: &str, size: &str, percent: &str, files: &str| (name.to_string(), size.to_string(), percent.to_string(), files.to_string());
        assert_eq!(
            rows,
            vec![
                row("root", "90 B", "100.0%", "3"),
                row("docs", "50 B", "55.6%", "1"),
                row("b.pdf", "50 B", "100.0%", "1"),
                row("d.exe", "30 B", "33.3%", "1"),
                row("c.txt", "10 B", "11.1%", "1"),
            ]
        );
    }

    /// Rects of `typed()` on a 100 x 100 map: the root, docs on the left with a.txt over b.pdf,
    /// d.exe over c.txt on the right.
    fn typed_rects() -> Vec<Rect> {
        let r = |node, x, y, w, h| Rect { node, x, y, w, h };
        vec![
            r(NodeId(0), 0.0, 0.0, 100.0, 100.0),
            r(DOCS, 0.0, 0.0, 60.0, 100.0),
            r(A_TXT, 0.0, 0.0, 60.0, 60.0),
            r(B_PDF, 0.0, 60.0, 60.0, 40.0),
            r(D_EXE, 60.0, 0.0, 40.0, 60.0),
            r(C_TXT, 60.0, 60.0, 40.0, 40.0),
        ]
    }

    #[test]
    fn remove_node_drops_the_rects_of_the_subtree_and_its_old_spot_hits_the_parent_frame() {
        let mut l = loaded_typed();
        l.rects = typed_rects();

        remove_node(&mut l, A_TXT);

        let left: Vec<NodeId> = l.rects.iter().map(|r| r.node).collect();
        assert_eq!(left, [NodeId(0), DOCS, B_PDF, D_EXE, C_TXT]);
        assert_eq!(crate::tree_view::hit_test(&l.rects, 10.0, 10.0), Some(DOCS));
    }

    #[test]
    fn remove_node_with_no_other_holder_keeps_the_node_buffer() {
        let mut l = loaded_typed();
        let before = std::ptr::from_ref(l.data.tree.node(NodeId(0)));

        remove_node(&mut l, A_TXT);

        assert_eq!(std::ptr::from_ref(l.data.tree.node(NodeId(0))), before);
    }

    /// Random tree of up to 200 nodes, folders and files with a few extensions; every entry is
    /// reachable, so node `i` is entry `i`.
    fn random_data(seed: &mut u64) -> Arc<Data> {
        let mut next = |n: u64| {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*seed >> 33) % n.max(1)
        };
        let n = 1 + next(200);
        let mut dirs = vec![0u64];
        let mut entries = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true, modified: scan::time::UNKNOWN }];
        for id in 1..n {
            let parent = dirs[next(dirs.len() as u64) as usize];
            let is_dir = next(3) == 0;
            let name = if is_dir { format!("d{id}") } else { format!("f{id}{}", ["", ".txt", ".DLL", ".mp4", ".tar.gz"][next(5) as usize]) };
            let size = if is_dir { 0 } else { [0, next(10), next(1 << 30)][next(3) as usize] };
            if is_dir {
                dirs.push(id);
            }
            entries.push(RawEntry { id, parent, name, size, allocated: size, is_dir, modified: scan::time::UNKNOWN });
        }
        let tree = rollup(0, entries).unwrap().tree;
        Arc::new(Data { counts: crate::tree_view::file_counts(&tree), tree })
    }

    #[test]
    fn remove_node_random_removals_keep_counts_totals_and_rects_equal_to_a_new_count() {
        let mut seed = 0x00C0_FFEE;
        for t in 0..300 {
            let mut l = loaded(&random_data(&mut seed));
            let all = l.data.counts.len() as u64;
            l.totals = Some(type_totals(&l.data.tree, NodeId(0)));
            l.rects = (0..all as u32).map(|i| Rect { node: NodeId(i), x: 0.0, y: 0.0, w: 1.0, h: 1.0 }).collect();
            for k in 0..1 + (seed >> 40) % 10 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let n = NodeId(((seed >> 33) % all) as u32);
                let removable = n != NodeId(0) && attached(&l.data.tree, n);
                let case = format!("tree {t}, removal {k} of {n:?}");

                let after = remove_node(&mut l, n);

                let tree = &l.data.tree;
                assert_eq!(after.is_some(), removable, "{case}");
                let fresh = crate::tree_view::file_counts(tree);
                let left: Vec<u32> = (0..all as u32).filter(|&i| attached(tree, NodeId(i))).collect();
                let counts: Vec<u64> = left.iter().map(|&i| l.data.counts[i as usize]).collect();
                assert_eq!(counts, left.iter().map(|&i| fresh[i as usize]).collect::<Vec<_>>(), "{case}: counts");
                assert_eq!(l.totals, Some(type_totals(tree, NodeId(0))), "{case}: totals");
                assert!(l.rects.iter().all(|r| attached(tree, r.node)), "{case}: rects");
                assert_eq!(l.rects.len(), left.len(), "{case}: rects kept");
            }
        }
    }

    #[test]
    fn remove_node_of_the_shown_folder_drops_every_rect() {
        let mut l = loaded_typed();
        l.rects = treemap::layout_framed(&l.data.tree, DOCS, 100, 100, Chrome::scaled(1.0));
        (l.view_root, l.shown_root) = (DOCS, Some(DOCS));
        assert!(!l.rects.is_empty());

        remove_node(&mut l, DOCS);

        assert!(l.rects.is_empty(), "{:?}", l.rects);
    }

    #[test]
    fn remove_node_random_removals_on_a_real_layout_keep_exactly_the_attached_rects() {
        let mut seed: u64 = 0x0BAD_CAFE;
        let mut roll = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n.max(1)
        };
        for t in 0..300 {
            let mut l = loaded(&random_data(&mut (roll(u64::MAX) + 1)));
            let tree = &l.data.tree;
            let folders: Vec<NodeId> = (0..l.data.counts.len() as u32).map(NodeId).filter(|&n| tree.node(n).is_dir).collect();
            let shown = folders[roll(folders.len() as u64) as usize];
            l.rects = treemap::layout_framed(tree, shown, 640, 480, Chrome::scaled(1.0));
            (l.view_root, l.shown_root) = (shown, Some(shown));
            for k in 0..1 + roll(8) {
                // Mostly drawn nodes, sometimes the shown folder or any node.
                let n = match roll(4) {
                    0 => shown,
                    1 => NodeId(roll(l.data.counts.len() as u64) as u32),
                    _ => l.rects.get(roll(l.rects.len() as u64) as usize).map_or(shown, |r| r.node),
                };
                let before = l.rects.clone();

                remove_node(&mut l, n);

                let want: Vec<Rect> = before.into_iter().filter(|r| attached(&l.data.tree, r.node)).collect();
                assert_eq!(l.rects, want, "tree {t}, removal {k} of {n:?}, shown {shown:?}");
            }
        }
    }

    #[test]
    fn remove_node_of_the_root_is_none_and_changes_nothing() {
        let mut l = loaded_typed();
        let before = format!("{:?}", l.data.tree);

        let after = remove_node(&mut l, NodeId(0));

        assert_eq!((after, format!("{:?}", l.data.tree), l.edits), (None, before, 0));
    }

    #[test]
    fn begin_layout_takes_a_new_layout_seq_and_drops_the_layout_kept_for_a_zoom() {
        show_finished_scan(&typed());
        let seq = SESSION.with_borrow_mut(|s| {
            s.held = Some(super::Layout { root: DOCS, rects: Vec::new(), image: None, labels: (Vec::new(), Vec::new()), size: (0.0, 0.0) });
            s.layout_seq
        });

        let job = SESSION.with_borrow_mut(begin_layout);

        let after = SESSION.with_borrow(|s| (s.layout_seq, s.held.is_none()));
        assert_eq!((job.map(|j| (j.root, j.seq)), after), (Some((NodeId(0), seq + 1)), (seq + 1, true)));
    }

    fn run(timing: bool) -> (Result<(), String>, Option<(String, bool)>) {
        SESSION.with_borrow_mut(|s| s.flags.timing = timing);
        let seen = RefCell::new(None);
        let result = restart_admin("some folder", |p, f| {
            *seen.borrow_mut() = Some((p.to_string(), f.timing));
            Ok(())
        });
        (result, seen.into_inner())
    }

    #[test]
    fn restart_admin_with_timing_passes_the_flag_on() {
        assert_eq!(run(true), (Ok(()), Some(("some folder".to_string(), true))));
    }

    #[test]
    fn restart_admin_without_timing_leaves_the_flag_off() {
        assert_eq!(run(false), (Ok(()), Some(("some folder".to_string(), false))));
    }

    #[test]
    fn restart_admin_launch_error_is_returned() {
        let result = restart_admin("some folder", |_, _| Err("code 5".into()));
        assert_eq!(result, Err("code 5".to_string()));
    }

    #[test]
    fn relaunch_path_on_the_start_screen_is_no_folder() {
        assert_eq!(relaunch_path(true, std::path::Path::new("D:\\Old scan")), "");
    }

    #[test]
    fn relaunch_path_on_the_results_screen_is_the_scan_root() {
        assert_eq!(relaunch_path(false, std::path::Path::new("D:\\Data")), "D:\\Data");
    }

    fn launch_with(path: Option<&str>) {
        SESSION.with_borrow_mut(|s| s.launch_path = path.map(str::to_string));
    }

    #[test]
    fn take_launch_path_first_frame_first_gets_the_path_and_the_fallback_nothing() {
        launch_with(Some("D:\\Data"));

        let (frame, fallback) = (take_launch_path(), take_launch_path());

        assert_eq!((frame, fallback), (Some("D:\\Data".to_string()), None));
    }

    #[test]
    fn take_launch_path_fallback_first_gets_the_path_and_the_first_frame_nothing() {
        launch_with(Some("C:\\"));

        let (fallback, frame) = (take_launch_path(), take_launch_path());

        assert_eq!((fallback, frame), (Some("C:\\".to_string()), None));
    }

    #[test]
    fn take_launch_path_every_later_frame_gets_nothing() {
        launch_with(Some("D:\\Data"));
        take_launch_path();

        let later: Vec<_> = (0..3).map(|_| take_launch_path()).collect();

        assert_eq!(later, [None, None, None]);
    }

    #[test]
    fn take_launch_path_with_no_folder_argument_gets_nothing() {
        launch_with(None);

        let (frame, fallback) = (take_launch_path(), take_launch_path());

        assert_eq!((frame, fallback), (None, None));
    }

    #[test]
    fn shows_start_at_launch_with_no_folder_argument_is_the_start_screen() {
        assert!(shows_start_at_launch(None));
    }

    #[test]
    fn shows_start_at_launch_with_a_folder_argument_is_the_results_screen() {
        assert!(!shows_start_at_launch(Some("C:\\")));
    }

    #[test]
    fn shows_start_at_launch_with_a_blank_folder_argument_is_the_start_screen() {
        assert!(shows_start_at_launch(Some("  ")));
    }

    // ---- hover gate and the end of a shown scan ----
    /// Calls of a `show` closure, and what it returns.
    type Calls = Rc<RefCell<Vec<Option<NodeId>>>>;

    fn shown() -> (Calls, impl Fn(Option<NodeId>) -> bool + Clone) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let record = calls.clone();
        (calls, move |n: Option<NodeId>| {
            record.borrow_mut().push(n);
            n.is_some()
        })
    }

    #[test]
    fn gate_hover_same_node_twice_shows_it_once() {
        let (calls, show) = shown();
        let a = Some(NodeId(1));

        gate_hover(a, show.clone());
        gate_hover(a, show);

        assert_eq!(*calls.borrow(), [a]);
    }

    #[test]
    fn gate_hover_new_node_is_over_and_changed() {
        let (_, show) = shown();

        assert_eq!(gate_hover(Some(NodeId(1)), show), (true, true));
    }

    #[test]
    fn gate_hover_same_node_again_is_over_and_not_changed() {
        let (_, show) = shown();
        gate_hover(Some(NodeId(1)), show.clone());

        assert_eq!(gate_hover(Some(NodeId(1)), show), (true, false));
    }

    #[test]
    fn gate_hover_off_every_node_after_a_node_hides_it() {
        let (calls, show) = shown();
        gate_hover(Some(NodeId(1)), show.clone());

        let result = gate_hover(None, show);

        assert_eq!((result, calls.borrow().last().copied()), ((false, true), Some(None)));
    }

    #[test]
    fn left_map_with_a_hovered_node_hides_it_and_the_tooltip() {
        let (calls, show) = shown();
        gate_hover(Some(NodeId(1)), show.clone());
        SESSION.with_borrow_mut(|s| s.tip = tip::TipState::Shown);

        let action = left_map(show);

        let state = SESSION.with_borrow(|s| (s.hovered, s.tip));
        assert_eq!((action, state, calls.borrow().len()), (tip::TipAction::Hide, (None, tip::TipState::Idle), 2));
    }

    #[test]
    fn left_map_with_nothing_hovered_does_not_call_show() {
        let (calls, show) = shown();

        left_map(show);

        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn forget_scan_with_a_hovered_node_hides_it_and_the_tooltip() {
        let (calls, show) = shown();
        gate_hover(Some(NodeId(1)), show.clone());
        SESSION.with_borrow_mut(|s| s.tip = tip::TipState::Pending);

        let action = forget_scan(show);

        let state = SESSION.with_borrow(|s| (s.hovered, s.tip));
        assert_eq!((action, state, calls.borrow().last().copied()), (tip::TipAction::Hide, (None, tip::TipState::Idle), Some(None)));
    }

    #[test]
    fn forget_scan_drops_the_shown_scan_and_its_pictures() {
        let data = data();
        SESSION.with_borrow_mut(|s| {
            s.loaded = Some(loaded(&data));
            s.held = Some(super::Layout { root: NodeId(0), rects: Vec::new(), image: None, labels: (Vec::new(), Vec::new()), size: (0.0, 0.0) });
            s.out_from = Some(NodeId(1));
            s.fade_next = true;
        });
        let (_, show) = shown();

        forget_scan(show);

        let left = SESSION.with_borrow(|s| (s.loaded.is_none(), s.held.is_none(), s.out_from, s.fade_next));
        assert_eq!(left, (true, true, None, false));
    }

    // ---- the context menu: targets ----
    /// `loaded_typed` with the rows root, docs, c.txt shown (docs collapsed) and a.txt selected.
    fn loaded_with_rows() -> Loaded {
        let mut l = loaded_typed();
        let row = |node, has_children| crate::tree_view::Row { node, depth: 0, expanded: false, has_children };
        l.model.set_rows(vec![row(NodeId(0), true), row(DOCS, true), row(C_TXT, false)]);
        l.selected = Some(A_TXT);
        l
    }

    #[test]
    fn target_of_map_tile_is_that_node_and_selects_it() {
        let l = loaded_with_rows();

        assert_eq!(target_of(&l, Source::Map, Some(C_TXT)), Some((C_TXT, true)));
    }

    #[test]
    fn target_of_map_frame_header_is_that_folder_and_selects_it() {
        let l = loaded_with_rows();

        assert_eq!(target_of(&l, Source::Map, Some(DOCS)), Some((DOCS, true)));
    }

    #[test]
    fn target_of_map_with_no_tile_is_none() {
        assert_eq!(target_of(&loaded_with_rows(), Source::Map, None), None);
    }

    #[test]
    fn target_of_row_is_its_node_and_selects_it() {
        let l = loaded_with_rows();

        assert_eq!(target_of(&l, Source::Row(2), None), Some((C_TXT, true)));
    }

    #[test]
    fn target_of_row_that_does_not_exist_is_none() {
        let l = loaded_with_rows();

        assert_eq!([Source::Row(-1), Source::Row(3), Source::Row(i32::MAX)].map(|src| target_of(&l, src, None)), [None, None, None]);
    }

    #[test]
    fn target_of_crumb_is_that_folder_and_does_not_select_it() {
        let l = loaded_with_rows();

        assert_eq!(target_of(&l, Source::Crumb(DOCS.0 as i32), None), Some((DOCS, false)));
    }

    #[test]
    fn target_of_the_gap_crumb_and_a_crumb_past_the_tree_are_none() {
        let l = loaded_with_rows();

        assert_eq!([Source::Crumb(-1), Source::Crumb(6), Source::Crumb(i32::MAX)].map(|src| target_of(&l, src, None)), [None, None, None]);
    }

    #[test]
    fn target_of_crumb_of_a_removed_folder_is_none() {
        let mut l = loaded_with_rows();
        remove_node(&mut l, DOCS);

        assert_eq!(target_of(&l, Source::Crumb(DOCS.0 as i32), None), None);
    }

    #[test]
    fn target_of_map_tile_removed_since_the_layout_is_none() {
        let mut l = loaded_with_rows();
        remove_node(&mut l, A_TXT);

        assert_eq!(target_of(&l, Source::Map, Some(A_TXT)), None);
    }

    #[test]
    fn target_of_key_is_the_selected_node_and_does_not_select_it_again() {
        let l = loaded_with_rows();

        assert_eq!(target_of(&l, Source::Key, None), Some((A_TXT, false)));
    }

    #[test]
    fn target_of_key_with_nothing_selected_is_none() {
        let mut l = loaded_with_rows();
        l.selected = None;

        assert_eq!(target_of(&l, Source::Key, None), None);
    }

    #[test]
    fn target_of_the_same_folder_by_map_row_crumb_and_key_is_the_same_node() {
        let mut l = loaded_with_rows();
        l.selected = Some(DOCS);

        let by = [
            target_of(&l, Source::Map, Some(DOCS)),
            target_of(&l, Source::Row(1), None),
            target_of(&l, Source::Crumb(DOCS.0 as i32), None),
            target_of(&l, Source::Key, None),
        ];

        assert_eq!(by.map(|t| t.map(|(n, _)| n)), [Some(DOCS); 4]);
    }

    // ---- the context menu: the request ----
    /// A finished scan on screen, shown as `loaded_with_rows`.
    fn show_rows() {
        SESSION.with_borrow_mut(|s| {
            let (generation, _) = s.scan.start().unwrap();
            assert!(s.scan.apply(generation, Event::Finished(summary())));
            s.loaded = Some(loaded_with_rows());
        });
    }

    fn pending() -> menu::Follow {
        menu::Follow::new(0, std::path::PathBuf::from("root\\d.exe"), Disk::File(30))
    }

    #[test]
    fn menu_request_for_a_row_of_a_finished_scan_is_ready_with_the_path_of_its_node() {
        show_rows();

        let got = menu_request(Source::Row(2), None, |_| Disk::File(10));

        let path = std::path::PathBuf::from("root").join("c.txt");
        let want = Request { node: C_TXT, select: true, is_dir: false, edits: 0, prepared: menu::Prepared::Ready { path, before: Disk::File(10) } };
        assert_eq!(got, Some(want));
    }

    #[test]
    fn menu_request_without_a_finished_scan_is_none() {
        SESSION.with_borrow_mut(|s| s.loaded = Some(loaded_with_rows()));

        assert_eq!(menu_request(Source::Row(2), None, |_| Disk::File(10)), None);
    }

    #[test]
    fn menu_request_with_a_disk_that_does_not_match_is_changed() {
        show_rows();

        let got = menu_request(Source::Crumb(DOCS.0 as i32), None, |_| Disk::Gone);

        assert_eq!(got.map(|r| (r.node, r.select, r.is_dir, r.prepared)), Some((DOCS, false, true, menu::Prepared::Changed)));
    }

    #[test]
    fn menu_request_carries_the_removals_since_the_scan() {
        show_rows();
        super::with_loaded(|l| remove_node(l, B_PDF));

        let got = menu_request(Source::Key, None, |_| Disk::File(100));

        assert_eq!(got.map(|r| (r.node, r.edits)), Some((A_TXT, 1)));
    }

    #[test]
    fn menu_request_drops_the_checks_of_the_menu_before() {
        show_rows();
        SESSION.with_borrow_mut(|s| s.follow = Some(pending()));

        menu_request(Source::Key, None, |_| Disk::File(100));

        assert!(SESSION.with_borrow(|s| s.follow.is_none()));
    }

    #[test]
    fn menu_request_for_no_item_keeps_the_checks_of_the_menu_before() {
        show_rows();
        SESSION.with_borrow_mut(|s| s.follow = Some(pending()));

        let got = menu_request(Source::Map, None, |_| Disk::File(100));

        assert_eq!((got, SESSION.with_borrow(|s| s.follow.is_some())), (None, true));
    }

    #[test]
    fn hit_at_during_a_zoom_out_is_the_tile_a_left_click_hits() {
        // (200, 200) is in tile 2 of the full map, but tile 1 fills the view at t = 0.
        assert_eq!(hit_at(&two_tiles(), Some(zoom_out_of_tile_1()), 0.0, VIEW, 200.0, 200.0), Some(NodeId(1)));
    }

    #[test]
    fn hit_at_with_no_zoom_is_the_tile_under_the_point() {
        assert_eq!(hit_at(&two_tiles(), None, 0.0, VIEW, 200.0, 200.0), Some(NodeId(2)));
    }

    #[test]
    fn hit_at_outside_every_tile_is_none() {
        // Tile 1 is (0, 0, 100, 100); tile 2 starts at x = 100: (50, 300) is in neither.
        assert_eq!(hit_at(&two_tiles(), None, 0.0, VIEW, 50.0, 300.0), None);
    }

    // ---- the context menu: outdated, Rescan ----
    #[test]
    fn rescan_shown_only_for_a_finished_scan_that_is_outdated() {
        use crate::model::ScanState;
        let states = [
            ScanState::Idle,
            ScanState::Scanning { files: 1, dirs: 1 },
            ScanState::Error("x".into()),
            ScanState::Cancelled,
            ScanState::Done(summary()),
        ];

        let outdated = states.each_ref().map(|st| rescan_shown(st, true));
        let current = rescan_shown(&ScanState::Done(summary()), false);

        assert_eq!((outdated, current), ([false, false, false, false, true], false));
    }

    #[test]
    fn forget_scan_clears_outdated_and_the_pending_checks() {
        SESSION.with_borrow_mut(|s| {
            s.outdated = true;
            s.follow = Some(menu::Follow::new(0, std::path::PathBuf::from("C:\\x"), Disk::Dir));
        });
        let (_, show) = shown();

        forget_scan(show);

        assert_eq!(SESSION.with_borrow(|s| (s.outdated, s.follow.is_none())), (false, true));
    }

    #[test]
    fn rescan_target_of_a_finished_scan_is_its_root_as_typed_and_keeps_the_options() {
        show_finished_scan(&data());
        SESSION.with_borrow_mut(|s| {
            s.scan_root = std::path::PathBuf::from("D:\\Data");
            s.flags.timing = true;
        });

        let target = rescan_target();

        assert_eq!((target.as_deref(), SESSION.with_borrow(|s| s.flags.timing)), (Some("D:\\Data"), true));
    }

    #[test]
    fn rescan_target_without_a_finished_scan_is_none() {
        SESSION.with_borrow_mut(|s| s.scan_root = std::path::PathBuf::from("D:\\Data"));

        assert_eq!(rescan_target(), None);
    }

    // ---- the context menu: the checks after an action ----
    /// A finished scan on screen (`loaded_with_rows`) with checks pending for D_EXE (`pending`).
    fn show_pending() {
        show_rows();
        let f = pending();
        SESSION.with_borrow_mut(|s| s.follow = Some(f));
    }

    fn follow_and_outdated() -> (bool, bool) {
        SESSION.with_borrow(|s| (s.follow.is_some(), s.outdated))
    }

    #[test]
    fn check_follow_with_no_checks_is_done() {
        show_rows();

        assert_eq!(check_follow(|_| Disk::Gone), FollowStep::Done);
    }

    #[test]
    fn check_follow_of_an_item_gone_after_a_shell_action_marks_the_scan_outdated_and_ends() {
        // The shell's own Delete is hidden, so a shell action never removes a node: the map is
        // marked outdated and the node stays until a rescan.
        show_pending();

        let got = check_follow(|_| Disk::Gone);

        assert_eq!((got, follow_and_outdated()), (FollowStep::Outdated, (false, true)));
        assert!(SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.tree.children(NodeId(0)).any(|c| c == D_EXE)));
    }

    #[test]
    fn check_follow_of_an_item_that_changed_marks_the_scan_outdated_and_ends() {
        show_pending();

        let got = check_follow(|_| Disk::File(31));

        assert_eq!((got, follow_and_outdated()), (FollowStep::Outdated, (false, true)));
    }

    #[test]
    fn check_follow_of_an_item_that_did_not_change_checks_again_later() {
        show_pending();

        let got = check_follow(|_| Disk::File(30));

        assert_eq!((got, follow_and_outdated()), (FollowStep::Again(std::time::Duration::from_millis(500)), (true, false)));
    }

    #[test]
    fn check_follow_reads_the_disk_at_the_path_of_the_checks() {
        show_pending();
        let read = RefCell::new(None);

        check_follow(|p| {
            *read.borrow_mut() = Some(p.to_path_buf());
            Disk::File(30)
        });

        assert_eq!(read.into_inner(), Some(std::path::PathBuf::from("root\\d.exe")));
    }

    #[test]
    fn check_follow_after_a_removal_since_the_menu_ends_without_a_decision() {
        show_pending();
        super::with_loaded(|l| remove_node(l, B_PDF));

        let got = check_follow(|_| Disk::Gone);

        assert_eq!((got, follow_and_outdated()), (FollowStep::Done, (false, false)));
    }

    #[test]
    fn check_follow_with_no_scan_shown_ends_without_a_decision() {
        show_pending();
        SESSION.with_borrow_mut(|s| s.loaded = None);

        let got = check_follow(|_| Disk::Gone);

        assert_eq!((got, follow_and_outdated()), (FollowStep::Done, (false, false)));
    }

    // ---- the context menu: Volatus items ----
    /// Runs `cmd` on C:\a\f.txt (a file, or a folder) with an action that fails with "boom" or works.
    /// Gives the note and what the action was called with.
    fn run_cmd(cmd: Cmd, is_dir: bool, fail: bool) -> (Option<String>, Option<(Cmd, std::path::PathBuf)>) {
        let called = RefCell::new(None);
        let note = run_action(cmd, std::path::Path::new("C:\\a\\f.txt"), is_dir, |c, p| {
            *called.borrow_mut() = Some((c, p.to_path_buf()));
            if fail { Err("boom".into()) } else { Ok(()) }
        });
        (note, called.into_inner())
    }

    #[test]
    fn run_action_a_failing_action_gives_its_note() {
        let notes = [Cmd::ExploreFolder, Cmd::CommandPrompt, Cmd::CopyPath].map(|cmd| run_cmd(cmd, false, true).0);

        assert_eq!(
            notes,
            [
                Some("Volatus cannot open the folder.".to_string()),
                Some("Volatus cannot open a command prompt.".to_string()),
                Some("Volatus cannot copy the path.".to_string()),
            ]
        );
    }

    #[test]
    fn run_action_a_working_action_gives_no_note() {
        let notes = [Cmd::ExploreFolder, Cmd::CommandPrompt, Cmd::CopyPath].map(|cmd| run_cmd(cmd, false, false).0);

        assert_eq!(notes, [None, None, None]);
    }

    #[test]
    fn run_action_explore_and_copy_get_the_raw_path() {
        let paths = [Cmd::ExploreFolder, Cmd::CopyPath].map(|cmd| run_cmd(cmd, false, false).1);

        let raw = std::path::PathBuf::from("C:\\a\\f.txt");
        assert_eq!(paths, [Some((Cmd::ExploreFolder, raw.clone())), Some((Cmd::CopyPath, raw))]);
    }

    #[test]
    fn run_action_command_prompt_gets_the_folder_of_a_file_and_a_folder_itself() {
        let paths = [run_cmd(Cmd::CommandPrompt, false, false).1, run_cmd(Cmd::CommandPrompt, true, false).1];

        assert_eq!(
            paths,
            [
                Some((Cmd::CommandPrompt, std::path::PathBuf::from("C:\\a"))),
                Some((Cmd::CommandPrompt, std::path::PathBuf::from("C:\\a\\f.txt"))),
            ]
        );
    }

    #[test]
    fn action_note_is_the_note_of_a_failed_action_only() {
        let cmds = [Cmd::ExploreFolder, Cmd::CommandPrompt, Cmd::CopyPath, Cmd::ShowFolder, Cmd::Rescan];

        let got: Vec<(Option<&str>, Option<&str>)> = cmds.iter().map(|&cmd| (action_note(cmd, true), action_note(cmd, false))).collect();

        let want = vec![
            (Some("Volatus cannot open the folder."), None),
            (Some("Volatus cannot open a command prompt."), None),
            (Some("Volatus cannot copy the path."), None),
            (None, None),
            (None, None),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn run_action_show_folder_and_rescan_are_not_the_actions_of_the_shell() {
        let got = [Cmd::ShowFolder, Cmd::Rescan].map(|cmd| run_cmd(cmd, true, true));

        assert_eq!(got, [(None, None), (None, None)]);
    }

    // ---- settings ----
    use super::{
        SAVE_ERROR, Settings, Site, ThemeChoice, adopt_settings, current_unit, effective_dark, persist_with,
        sites_after_unit_change,
    };
    use crate::settings;
    use std::cell::Cell;
    use std::path::{Path, PathBuf};

    fn path() -> PathBuf {
        PathBuf::from("C:\\Users\\test\\AppData\\Roaming\\Volatus\\settings.json")
    }

    fn custom() -> Settings {
        Settings { theme: ThemeChoice::Dark, unit: Unit::Gb, show_tree: false, ..Settings::default() }
    }

    #[test]
    fn unit_change_resyncs_every_size_site() {
        use Site::*;
        assert_eq!(sites_after_unit_change(), &[Rows, Types, Selection, Banner, Map, Drives, Column]);
    }

    // ---- Size column ----
    use super::{size_column_width, winit_theme_for};
    use crate::model::format_size;

    /// 4 TiB: the biggest folder the Size column is made for.
    const FOUR_TB: u64 = 4 * 1024 * 1024 * 1024 * 1024;

    #[test]
    fn size_column_width_keeps_76_px_where_the_text_fits() {
        for unit in [Unit::Dynamic, Unit::Gb, Unit::Tb] {
            assert_eq!(size_column_width(unit), 76.0, "{unit:?}");
        }
    }

    #[test]
    fn size_column_width_fits_a_4_tb_folder_in_byte_kb_and_mb() {
        let texts = [Unit::Byte, Unit::Kb, Unit::Mb].map(|u| format_size(FOUR_TB, u));
        assert_eq!(texts, ["4,398,046,511,104 B", "4,294,967,296.0 KB", "4,194,304.0 MB"]);

        let widths = [Unit::Byte, Unit::Kb, Unit::Mb].map(size_column_width);

        // From the estimate: 7 px a digit, 3.5 px for ",", "." and the space, 7.5 to 9 px a
        // letter, and 8 px for the cell padding (`NumberCell` keeps 6 px) and a little room.
        assert_eq!(widths, [124.0, 118.0, 95.0]);
    }

    #[test]
    fn size_column_width_leaves_room_for_the_name_in_the_narrowest_sidebar() {
        // main.slint: the sidebar is at least 340 px; its padding (2 x 4 px), the list inset
        // (4 px), the scroll bar room (16 px), the % column (60 px) and the Files column (72 px).
        let fixed = 8.0 + 4.0 + 16.0 + 60.0 + 72.0;
        for unit in Unit::ALL {
            let name = 340.0 - fixed - size_column_width(unit);
            assert!(name >= 40.0, "{unit:?}: {name} px for the name");
        }
    }

    #[test]
    fn winit_theme_for_forces_light_and_dark_and_follows_windows_with_none() {
        use slint::winit_030::winit::window::Theme;
        assert_eq!(ThemeChoice::ALL.map(winit_theme_for), [None, Some(Theme::Light), Some(Theme::Dark)]);
    }

    #[test]
    fn session_before_loading_is_todays_behaviour() {
        let (settings, kept) = SESSION.with_borrow(|s| (s.settings.clone(), s.settings_path.clone()));
        assert_eq!((settings, kept, current_unit()), (Settings::default(), None, Unit::Dynamic));
    }

    #[test]
    fn session_starts_with_the_loaded_settings() {
        let error = SESSION.with_borrow_mut(|s| adopt_settings(s, Some(path()), (custom(), settings::Loaded::Ok)));

        let (settings, kept) = SESSION.with_borrow(|s| (s.settings.clone(), s.settings_path.clone()));
        assert_eq!((settings, kept, error, current_unit()), (custom(), Some(path()), "", Unit::Gb));
    }

    #[test]
    fn a_failed_save_keeps_the_setting_and_reports_it() {
        SESSION.with_borrow_mut(|s| {
            adopt_settings(s, Some(path()), (Settings::default(), settings::Loaded::Fresh));
            s.settings.unit = Unit::Mb;
        });
        let calls = Cell::new(0);

        let error = SESSION.with_borrow_mut(|s| {
            persist_with(s, |_, _| {
                calls.set(calls.get() + 1);
                Err(std::io::Error::other("disk full"))
            })
        });

        assert_eq!((error, calls.get(), current_unit()), (SAVE_ERROR, 1, Unit::Mb));
    }

    #[test]
    fn a_good_save_writes_the_session_settings_to_the_path() {
        SESSION.with_borrow_mut(|s| adopt_settings(s, Some(path()), (custom(), settings::Loaded::Ok)));
        let seen = RefCell::new(None);

        let error = SESSION.with_borrow_mut(|s| {
            persist_with(s, |p: &Path, set: &Settings| {
                *seen.borrow_mut() = Some((p.to_path_buf(), set.clone()));
                Ok(())
            })
        });

        assert_eq!((error, seen.into_inner()), ("", Some((path(), custom()))));
    }

    #[test]
    fn a_file_kept_as_bad_is_saved_over() {
        let error = SESSION.with_borrow_mut(|s| adopt_settings(s, Some(path()), (Settings::default(), settings::Loaded::Damaged)));
        let calls = Cell::new(0);

        let saved = SESSION.with_borrow_mut(|s| {
            persist_with(s, |_, _| {
                calls.set(calls.get() + 1);
                Ok(())
            })
        });

        assert_eq!((error, saved, calls.get()), ("", "", 1));
    }

    /// Settings that must not be saved: every save is skipped and reported, and the change stays.
    fn never_saved(path: Option<PathBuf>, loaded: settings::Loaded) {
        let error = SESSION.with_borrow_mut(|s| adopt_settings(s, path, (Settings::default(), loaded)));
        SESSION.with_borrow_mut(|s| s.settings.unit = Unit::Kb);
        let calls = Cell::new(0);

        let saves: Vec<&str> = (0..2)
            .map(|_| {
                SESSION.with_borrow_mut(|s| {
                    persist_with(s, |_, _| {
                        calls.set(calls.get() + 1);
                        Ok(())
                    })
                })
            })
            .collect();

        assert_eq!((error, saves, calls.get(), current_unit()), (SAVE_ERROR, vec![SAVE_ERROR, SAVE_ERROR], 0, Unit::Kb));
    }

    #[test]
    fn a_file_that_was_not_kept_is_never_saved_over() {
        never_saved(Some(path()), settings::Loaded::NotKept);
    }

    #[test]
    fn settings_with_no_path_are_never_saved() {
        never_saved(None, settings::Loaded::Fresh);
    }

    #[test]
    fn effective_dark_light_and_dark_ignore_the_system_and_follow_windows_returns_it() {
        for system in [false, true] {
            let got = ThemeChoice::ALL.map(|c| effective_dark(c, system));
            assert_eq!(got, [system, false, true], "system dark: {system}");
        }
    }

    #[test]
    fn row_model_row_size_uses_the_unit_of_the_model() {
        let (tree, _, files) = flat(0, &[1_234_567]);
        let data = Arc::new(Data { counts: crate::tree_view::file_counts(&tree), tree });
        let row = crate::tree_view::Row { node: files[0], depth: 0, expanded: false, has_children: false };
        let model = RowModel { data: RefCell::new(Arc::downgrade(&data)), rows: RefCell::new(vec![row]), notify: ModelNotify::default(), unit: Cell::new(Unit::Byte) };

        let byte = slint::Model::row_data(&model, 0).unwrap();
        model.unit.set(Unit::Kb);
        let kb = slint::Model::row_data(&model, 0).unwrap();

        assert_eq!((byte.size.as_str(), kb.size.as_str()), ("1,234,567 B", "1,205.6 KB"));
        assert!(byte.spoken.contains("1,234,567 B"), "{}", byte.spoken);
    }

    // ---- show or hide the parts ----
    use super::{Focus, footer_height, focus_after_hide, show_flags, toggle_part_with};
    use crate::settings::Part;

    #[test]
    fn footer_height_is_zero_only_when_the_strip_and_the_chips_are_both_hidden() {
        assert_eq!(footer_height(true, true), 48.0);
        assert_eq!(footer_height(true, false), 48.0);
        assert_eq!(footer_height(false, true), 48.0);
        assert_eq!(footer_height(false, false), 0.0);
    }

    #[test]
    fn focus_after_hide_moves_the_focus_from_a_hidden_tree_to_the_map() {
        assert_eq!(focus_after_hide(Part::Tree, Focus::Tree), Focus::Map);
        for focus in [Focus::Map, Focus::Other] {
            assert_eq!(focus_after_hide(Part::Tree, focus), focus, "tree hidden, focus at {focus:?}");
        }
        for part in Part::ALL.into_iter().filter(|&p| p != Part::Tree) {
            for focus in [Focus::Tree, Focus::Map, Focus::Other] {
                assert_eq!(focus_after_hide(part, focus), focus, "{part:?} hidden, focus at {focus:?}");
            }
        }
    }

    #[test]
    fn show_flags_follow_settings() {
        for bits in 0u8..32 {
            let mut s = Settings::default();
            let want: [bool; 5] = std::array::from_fn(|i| bits & (1 << i) != 0);
            for (part, on) in Part::ALL.into_iter().zip(want) {
                s.set_shown(part, on);
            }
            assert_eq!(show_flags(&s), want, "bits {bits:05b}");
        }
    }

    #[test]
    fn toggle_part_saves_once_and_never_after_not_kept() {
        SESSION.with_borrow_mut(|s| adopt_settings(s, Some(path()), (Settings::default(), settings::Loaded::Fresh)));
        let calls = Cell::new(0);
        let saved = RefCell::new(None);

        let error = SESSION.with_borrow_mut(|s| {
            toggle_part_with(s, Part::Tree, false, |_, set: &Settings| {
                calls.set(calls.get() + 1);
                *saved.borrow_mut() = Some(set.clone());
                Ok(())
            })
        });

        let want = Settings { show_tree: false, ..Settings::default() };
        let now = SESSION.with_borrow(|s| s.settings.clone());
        assert_eq!((error, calls.get(), saved.into_inner(), now), ("", 1, Some(want.clone()), want));

        // After a file that was not kept: the change still applies, and no save is ever made.
        SESSION.with_borrow_mut(|s| adopt_settings(s, Some(path()), (Settings::default(), settings::Loaded::NotKept)));
        let calls = Cell::new(0);
        let errors: Vec<&str> = [(Part::Labels, false), (Part::Header, false)]
            .into_iter()
            .map(|(part, on)| {
                SESSION.with_borrow_mut(|s| {
                    toggle_part_with(s, part, on, |_, _| {
                        calls.set(calls.get() + 1);
                        Ok(())
                    })
                })
            })
            .collect();
        let now = SESSION.with_borrow(|s| s.settings.clone());
        assert_eq!(
            (errors, calls.get(), now),
            (vec![SAVE_ERROR, SAVE_ERROR], 0, Settings { show_labels: false, show_header: false, ..Settings::default() })
        );
    }

    // ---- always start as administrator ----
    use super::{should_auto_elevate, auto_elevate_on_start};

    #[test]
    fn should_auto_elevate_only_when_setting_on_and_not_admin_and_not_flagged() {
        assert!(!should_auto_elevate(false, false, false));
        assert!(!should_auto_elevate(false, true, false));
        assert!(!should_auto_elevate(true, true, false));
        assert!(!should_auto_elevate(true, false, true));
        assert!(!should_auto_elevate(true, true, true));
        assert!(should_auto_elevate(true, false, false));
    }

    #[test]
    fn auto_elevate_on_start_success_launches_and_sets_no_elevate() {
        let mut args = crate::cli::parse_args(["C:\\Data".to_string()]);
        let launched = Cell::new(None);
        let ok = auto_elevate_on_start(true, false, &mut args, |path, flags| {
            launched.set(Some((path.to_string(), flags)));
            Ok(())
        });
        assert!(ok);
        let (path, flags) = launched.into_inner().unwrap();
        assert_eq!(path, "C:\\Data");
        assert!(flags.no_elevate);
        assert!(args.notices.is_empty());
    }

    #[test]
    fn auto_elevate_on_start_declined_records_notice_and_continues() {
        let mut args = crate::cli::parse_args(["C:\\Data".to_string()]);
        let ok = auto_elevate_on_start(true, false, &mut args, |_, _| {
            Err("UAC cancelled".into())
        });
        assert!(!ok);
        assert_eq!(args.notices, vec!["Administrator rights were declined. Running as standard user.".to_string()]);
    }

    #[test]
    fn toggle_always_admin_saves_and_persists() {
        SESSION.with_borrow_mut(|s| adopt_settings(s, Some(path()), (Settings::default(), settings::Loaded::Fresh)));
        let calls = Cell::new(0);
        let saved = RefCell::new(None);

        let error = SESSION.with_borrow_mut(|s| {
            s.settings.always_admin = true;
            persist_with(s, |_, set: &Settings| {
                calls.set(calls.get() + 1);
                *saved.borrow_mut() = Some(set.clone());
                Ok(())
            })
        });

        let want = Settings { always_admin: true, ..Settings::default() };
        let now = SESSION.with_borrow(|s| s.settings.clone());
        assert_eq!((error, calls.get(), saved.into_inner(), now), ("", 1, Some(want.clone()), want));
    }
}
//! Volatus window: Mica backdrop, scan controls and the result banner.
//!
//! Mica recipe (proved by a spike, see `.claude/work/storagetool-wiztree-clone/logs/P3-mica-spike.md`):
//! the root `Window` background starts transparent; `slint::Window::window_handle()` only yields
//! a handle after the winit window exists, so `apply_mica` runs from `invoke_from_event_loop`
//! queued before the event loop starts. Calling it right after `show()` returns `NotSupported`.
//! If Mica is not available (Windows 10), the window falls back to the opaque Fluent background.

mod browse;
mod cli;
mod drives;
mod elevate;
mod footer;
mod model;
mod motion;
mod nav;
mod tip;
mod tree_view;

use browse::BrowseState;
use model::{
    Event, ScanControl, ScanState, Summary, TimingRun, append_timing, banner_text, error_message, format_size,
    group_digits, offer_for, shows_start, timing_for_state,
};
use nav::{Dir, TreeMove};
use scan::{NodeId, Progress, ScanError, Tree};
use slint::{
    ComponentHandle, Image, Model, ModelNotify, ModelRc, ModelTracker, Rgba8Pixel, SetRenderingNotifierError,
    SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel, Weak,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tree_view::{Row, SortKey, ViewState};
use treemap::{Chrome, Frame, Rect, Theme};

slint::include_modules!();

/// Minimum gap between progress updates sent to the UI.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Quiet time after the last treemap size change before it is laid out again.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(80);
/// Narrowest frame label, in logical pixels: room for a few characters and "…".
const LABEL_MIN_WIDTH: f64 = 24.0;
/// Most labels on the map, frames and file tiles together: the frames first, then the largest
/// tiles. Measured at 3840x2160 and 200%, release, 3 runs of 10 frames, each with a new image
/// (P8.P6b fix 1): 650 labels (554 frames and 96 files) show in a median of 36.6 to 38.1 ms and a
/// worst of 43.6 ms, 13% under the 50 ms target. 750 gave a worst of 49.8 ms (0.4% margin), 1000
/// a median of 50.7 to 55.7 ms. The C: drive has 554 frames and 76 file tiles with a label at that
/// size (630), so the cap rarely applies.
const LABEL_CAP: usize = 650;
/// Least file tile that shows its name, in logical pixels: room for a few characters and "…".
const FILE_LABEL_MIN_WIDTH: f64 = 40.0;
const FILE_LABEL_MIN_HEIGHT: f64 = 14.0;
/// File tiles at least this high, in logical pixels, show the size on a second line.
const FILE_LABEL_TWO_LINES: f64 = 32.0;
/// Time the pointer stays on one treemap node before its tooltip shows.
const TIP_DELAY: Duration = Duration::from_millis(400);
/// Most file-type chips in the footer before "Other".
const FOOTER_CHIPS: usize = 8;

/// A finished scan plus its derived per-node data. Shared with treemap layout threads.
struct Data {
    tree: Tree,
    /// Files under each node, from `tree_view::file_counts`.
    counts: Vec<u64>,
}

/// The file tree as a Slint model. Rows are cheap ids; text is built only for rows the list
/// asks for, so a tree with millions of nodes costs nothing for the rows nobody sees.
struct RowModel {
    data: Arc<Data>,
    rows: RefCell<Vec<Row>>,
    notify: ModelNotify,
}

impl RowModel {
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
        let tree = &self.data.tree;
        let node = tree.node(row.node);
        let parent_size = node.parent.map_or(node.size, |p| tree.node(p).size);
        Some(TreeRow {
            name: model::display_name(tree.name(row.node)).as_ref().into(),
            size: format_size(node.size).into(),
            percent: tree_view::format_percent(node.size, parent_size).into(),
            files: group_digits(self.data.counts[row.node.0 as usize]).into(),
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
    model: Rc<RowModel>,
    view: ViewState,
    selected: Option<NodeId>,
    /// Folder the treemap shows, or will show when its layout arrives.
    view_root: NodeId,
    /// Treemap rects in physical pixels, from the last layout that arrived.
    rects: Vec<Rect>,
    /// The folder of the shown picture and its `rects`; `None` before the first one.
    shown_root: Option<NodeId>,
    /// Footer chips of the whole scan; `None` while the file types are counted.
    chips: Option<Vec<footer::Chip>>,
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
    /// `--timing` was given. Also passed on by "Restart as administrator".
    timing: bool,
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
}

/// A finished layout of folder `root`: its rects in physical pixels, its picture and its labels.
struct Layout {
    root: NodeId,
    rects: Vec<Rect>,
    image: Option<SharedPixelBuffer<Rgba8Pixel>>,
    labels: (Vec<FrameLabel>, Vec<FileLabel>),
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
        timing: false,
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
    });
    /// Starts one relayout after the window stops changing the treemap size.
    static RESIZE_TIMER: Timer = Timer::default();
    /// Shows the tooltip of the hovered node after `TIP_DELAY`.
    static TIP_TIMER: Timer = Timer::default();
    /// Ends the time of the move of the treemap picture.
    static MOVE_TIMER: Timer = Timer::default();
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
    ui.set_banner(append_timing(banner_text(&state, offer), line.as_deref()).into());
    ui.set_show_elevate(offer);
    ui.set_scan_title(SESSION.with_borrow(|s| model::scan_title(&s.scan_root)).into());
}

fn with_loaded<R>(f: impl FnOnce(&mut Loaded) -> R) -> Option<R> {
    SESSION.with_borrow_mut(|s| s.loaded.as_mut().map(f))
}

/// Rebuilds the visible tree rows and the sort and selection markers.
fn sync_rows(ui: &MainWindow) {
    let Some((selected, key, descending)) = with_loaded(|l| {
        let rows = tree_view::flatten(&l.data.tree, &l.data.counts, &l.view);
        let selected = l.selected.and_then(|n| tree_view::row_index(&rows, n));
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
fn sync_selection(ui: &MainWindow) {
    let scale = ui.window().scale_factor() as f64;
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

/// Lays out and draws the treemap for the current folder and window size on a worker thread.
/// The window keeps the old image until the new one arrives. A result is dropped if a newer
/// layout or a new scan started meanwhile.
/// ponytail: one thread per request and no early stop; fine while a layout takes well under
/// a second and resizes are debounced. Upgrade path: a cancel flag per request.
fn relayout(ui: &MainWindow) {
    let scale = ui.window().scale_factor();
    let theme = if ui.get_dark() { Theme::Dark } else { Theme::Light };
    let Some((data, root, generation, seq, w, h)) = SESSION.with_borrow_mut(|s| {
        let l = s.loaded.as_ref()?;
        let (lw, lh) = s.map_size;
        let (w, h) = ((lw * scale).round() as u32, (lh * scale).round() as u32);
        let (data, root) = (l.data.clone(), l.view_root);
        s.layout_seq += 1;
        // A layout kept for a zoom in is older than this one now.
        s.held = None;
        // With `--timing`, each layout (and new picture) shows, so a hover that starts one is seen.
        if s.timing {
            eprintln!("Layout {}.", s.layout_seq);
        }
        Some((data, root, s.scan.generation(), s.layout_seq, w, h))
    }) else {
        return;
    };
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let chrome = Chrome::scaled(f64::from(scale));
        let rects = treemap::layout_framed(&data.tree, root, w, h, chrome);
        let image = (w > 0 && h > 0).then(|| {
            let px = treemap::render_framed(&data.tree, &rects, w, h, chrome, theme);
            SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&px, w, h)
        });
        let labels = map_labels(&data.tree, &rects, chrome, f64::from(scale), theme);
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let current = SESSION.with_borrow(|s| (s.scan.generation(), s.layout_seq));
            if let Some(ui) = weak.upgrade().filter(|_| current == (generation, seq)) {
                land(&ui, Layout { root, rects, image, labels });
            }
        });
    });
}

/// The labels of the frames in `rects` (physical pixels) for the window, in logical pixels.
fn frame_labels(tree: &Tree, rects: &[Rect], chrome: Chrome, scale: f64) -> Vec<FrameLabel> {
    let px = |v: f64| (v / scale) as f32;
    treemap::label_spots(tree, rects, chrome, LABEL_MIN_WIDTH * scale, LABEL_CAP)
        .iter()
        .map(|r| {
            let (name, size) = tree_view::frame_label(tree, r.node);
            FrameLabel { x: px(r.x), y: px(r.y), width: px(r.w), height: px(r.h), name: name.into(), size: size.into() }
        })
        .collect()
}

/// The labels of the file tiles in `rects` (physical pixels) for the window, in logical pixels:
/// at most `cap`, the largest tiles first, with the text colour of each tile in `theme`.
fn file_labels(tree: &Tree, rects: &[Rect], scale: f64, theme: Theme, cap: usize) -> Vec<FileLabel> {
    let px = |v: f64| (v / scale) as f32;
    let (min_w, min_h) = (FILE_LABEL_MIN_WIDTH * scale, FILE_LABEL_MIN_HEIGHT * scale);
    treemap::file_label_spots(tree, rects, min_w, min_h, cap)
        .iter()
        .map(|r| {
            let (name, size) = tree_view::file_label(tree, r.node);
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
fn map_labels(tree: &Tree, rects: &[Rect], chrome: Chrome, scale: f64, theme: Theme) -> (Vec<FrameLabel>, Vec<FileLabel>) {
    let frames = frame_labels(tree, rects, chrome, scale);
    let files = file_labels(tree, rects, scale, theme, LABEL_CAP.saturating_sub(frames.len()));
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
fn hover_of(data: &Data, rects: &[Rect], node: NodeId, scale: f64, theme: Theme) -> Hover {
    let Some((x, y, w, h)) = tree_view::selection_bounds(rects, node) else {
        return Hover::default();
    };
    let tree = &data.tree;
    let px = |v: f64| (v / scale) as f32;
    let [red, green, blue] = treemap::filetype::colour(treemap::filetype::category_of(tree.name(node)), theme);
    // The same label rules as on the map, for the tile alone at its own origin (none for a folder).
    let label = file_labels(tree, &[Rect { node, x: 0.0, y: 0.0, w, h }], scale, theme, 1).pop();
    let (title, detail) = tree_view::tooltip_lines(tree, &data.counts, node);
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
    let (x, y) = picture_point(moving, f64::from(ui.get_zoom_t()), map_px(ui), x, y)?;
    with_loaded(|l| tree_view::hit_test(&l.rects, x, y)).flatten()
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
    let changed = note_hover(node);
    let over = if changed { show_hover(ui, node) } else { node.is_some() };
    tip_event(ui, tip::TipEvent::Moved { over, changed });
    let took = started.elapsed();
    SESSION.with_borrow_mut(|s| s.hover_slowest = s.hover_slowest.max(took));
}

/// The pointer left the treemap: nothing is hovered.
fn leave(ui: &MainWindow) {
    if note_hover(None) {
        show_hover(ui, None);
    }
    tip_event(ui, tip::TipEvent::Leave);
}

/// The shown rects changed or the shown folder changes: the old hover and tooltip go. The node
/// under a pointer that has not moved is not looked up; the next pointer move finds it.
fn drop_hover(ui: &MainWindow) {
    if note_hover(None) {
        show_hover(ui, None);
    }
    tip_event(ui, tip::TipEvent::Layout);
}

/// Pushes the hover properties of `node` (`None`: nothing hovered) into the window. True when
/// the hover is visible.
fn show_hover(ui: &MainWindow, node: Option<NodeId>) -> bool {
    let scale = f64::from(ui.window().scale_factor());
    let theme = if ui.get_dark() { Theme::Dark } else { Theme::Light };
    let hover = node.and_then(|n| with_loaded(|l| hover_of(&l.data, &l.rects, n, scale, theme))).unwrap_or_default();
    let shown = hover.visible;
    ui.set_hover(hover);
    shown
}

/// Feeds `event` to the tooltip state machine and does what it says: the tooltip of the old node
/// hides; the one of the new node shows after `TIP_DELAY`, at the pointer.
fn tip_event(ui: &MainWindow, event: tip::TipEvent) {
    let action = SESSION.with_borrow_mut(|s| {
        let (next, action) = tip::step(s.tip, event);
        s.tip = next;
        action
    });
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
fn show_layout(ui: &MainWindow, Layout { root, rects, image, labels: (labels, file_labels) }: Layout) {
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
    motion::allowed(motion::client_area_animation())
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
    if SESSION.with_borrow(|s| s.timing) {
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

/// Replaces the shown scan with a new one.
fn install(ui: &MainWindow, data: Arc<Data>) {
    let root = data.tree.root();
    let model =
        Rc::new(RowModel { data: data.clone(), rows: RefCell::new(Vec::new()), notify: ModelNotify::default() });
    ui.set_rows(ModelRc::from(model.clone()));
    SESSION.with_borrow_mut(|s| {
        s.loaded = Some(Loaded {
            data: data.clone(),
            model,
            view: ViewState::new(root),
            selected: Some(root),
            view_root: root,
            rects: Vec::new(),
            shown_root: None,
            chips: None,
        });
        s.fade_next = true;
    });
    sync_rows(ui);
    relayout(ui);
    count_types(ui, data);
}

/// Counts the file types of the scan for the footer on its own worker thread, started after the
/// layout worker of the first treemap, which does not wait for it. Until it ends the footer
/// says "Counting file types.".
/// The result is dropped if a newer scan started or the scan was cancelled meanwhile.
fn count_types(ui: &MainWindow, data: Arc<Data>) {
    let generation = SESSION.with_borrow(|s| s.scan.generation());
    sync_types(ui);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let root = data.tree.root();
        let totals = treemap::filetype::type_totals(&data.tree, root);
        let chips = footer::chips(&totals, data.tree.node(root).size, FOOTER_CHIPS);
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            if store_chips(generation, &data, chips) {
                sync_types(&ui);
            }
        });
    });
}

/// Keeps the footer `chips` counted for `data` of scan `generation`. `false`, and nothing kept, for a
/// late result: of an older scan, or for data the window no longer shows (`ScanControl::reset`
/// keeps the generation, so the generation alone does not tell).
fn store_chips(generation: u64, data: &Arc<Data>, chips: Vec<footer::Chip>) -> bool {
    SESSION.with_borrow_mut(|s| {
        let shown = s.scan.is_current(generation).then_some(s.loaded.as_mut()).flatten();
        shown.filter(|l| Arc::ptr_eq(&l.data, data)).map(|l| l.chips = Some(chips)).is_some()
    })
}

/// Pushes the footer chips into the window, in the tile colours of the current theme.
fn sync_types(ui: &MainWindow) {
    let theme = if ui.get_dark() { Theme::Dark } else { Theme::Light };
    let (counting, chips) = with_loaded(|l| match &l.chips {
        None => (true, Vec::new()),
        Some(chips) => (false, chips.iter().map(|c| type_chip(c, theme)).collect()),
    })
    .unwrap_or_default();
    ui.set_types_counting(counting);
    ui.set_type_chips(ModelRc::new(VecModel::from(chips)));
}

/// The window's form of chip `c`, in its tile colour in `theme`.
fn type_chip(c: &footer::Chip, theme: Theme) -> TypeChip {
    let [r, g, b] = treemap::filetype::colour(c.category, theme);
    TypeChip {
        label: c.label.as_str().into(),
        percent: c.percent.as_str().into(),
        share: c.share,
        color: slint::Color::from_rgb_u8(r, g, b),
    }
}

/// Drops the shown scan.
fn clear(ui: &MainWindow) {
    stop_move(ui);
    SESSION.with_borrow_mut(|s| {
        s.loaded = None;
        s.held = None;
        s.out_from = None;
        s.fade_next = false;
    });
    ui.set_rows(ModelRc::default());
    ui.set_treemap(Image::default());
    ui.set_crumbs(ModelRc::default());
    ui.set_labels(ModelRc::default());
    ui.set_file_labels(ModelRc::default());
    ui.set_map_note(SharedString::new());
    sync_types(ui);
    leave(ui);
    ui.set_has_selection(false);
    ui.set_selected_row(-1);
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
        s.run = s.timing.then(|| TimingRun::new(Instant::now()));
    });
    show_state(ui);
    clear(ui);
    ui.set_elevate_note(SharedString::new());
    if path.is_empty() {
        apply(ui, generation, Event::Failed("Choose a folder or drive first.".into()));
        return;
    }
    let root = PathBuf::from(path);
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

    std::thread::spawn(move || match scan::scan(&root, tx, &cancel) {
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
        Err(e) => post(&weak, generation, Event::Failed(error_message(&e))),
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
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let list = drives::list();
        let note = drives::admin_note(&list, elevate::is_admin());
        let buttons = drives::drive_buttons(&list);
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
    ui.on_zoom_frame(|m, t| frame_at(&m, t));
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
            let root = SESSION.with_borrow(|s| s.scan_root.to_string_lossy().into_owned());
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

fn main() {
    let args = cli::parse_args(std::env::args().skip(1));
    SESSION.with_borrow_mut(|s| s.timing = args.timing);
    let ui = MainWindow::new().expect("create window");
    // The renderer is chosen when the window is made (winit falls back to software when OpenGL
    // cannot start), so the answer is final here. The notifier only counts the frames drawn.
    let notifier = ui.window().set_rendering_notifier(|state, _| {
        if matches!(state, slint::RenderingState::AfterRendering) {
            FRAMES.with(|f| f.set(f.get() + 1));
        }
    });
    if !glow_drawn(notifier) {
        eprintln!("This renderer draws no shadows. The hovered tile gets a border.");
        ui.set_glow(false);
    }
    ui.set_banner(banner_text(&ScanState::Idle, false).into());
    ui.on_scan_drive({
        let weak = ui.as_weak();
        move |root| {
            if let Some(ui) = weak.upgrade() {
                start_scan(&ui, &root);
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

    ui.show().expect("show window");
    let weak = ui.as_weak();
    slint::invoke_from_event_loop(move || {
        let Some(ui) = weak.upgrade() else { return };
        let handle = ui.window().window_handle();
        if let Err(e) = window_vibrancy::apply_mica(&handle, None) {
            eprintln!("Mica is not available: {e}");
            ui.set_mica(false);
        }
    })
    .expect("queue Mica setup");
    // Started by "Restart as administrator": scan the folder that was open.
    if let Some(path) = args.path {
        start_scan(&ui, &path);
    } else {
        refresh_drives(&ui);
    }
    slint::run_event_loop().expect("run event loop");
    SESSION.with_borrow(|s| {
        if s.timing {
            let slowest = s.hover_slowest.as_secs_f64() * 1000.0;
            eprintln!("Hover changes: {}. Slowest hover: {slowest:.2} ms.", s.hover_changes);
            eprintln!("Frames drawn: {}.", FRAMES.with(Cell::get));
        }
    });
}

/// Starts the elevated copy through `launch(path, timing)`. `--timing` is carried over from the session.
fn restart_admin(path: &str, launch: impl FnOnce(&str, bool) -> Result<(), String>) -> Result<(), String> {
    launch(path, SESSION.with_borrow(|s| s.timing))
}

#[cfg(test)]
mod tests {
    use super::{
        Data, FILE_LABEL_MIN_HEIGHT, FILE_LABEL_MIN_WIDTH, FILE_LABEL_TWO_LINES, FileLabel, FrameLabel, Hover, LABEL_CAP, ZoomFrame, ZoomMove, frame_at,
        Loaded, RowModel, SESSION, file_labels, frame_labels, glow_drawn, hover_of, map_labels, note_hover, restart_admin,
        store_chips, Move, picture_point, zoom_frame_at,
    };
    use crate::motion::Kind;
    use std::time::Instant;
    use slint::SetRenderingNotifierError;
    use scan::{NodeId, Tree};
    use treemap::Theme;
    use crate::footer::Chip;
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
            RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true },
            RawEntry { id: 1, parent: 0, name: "f".into(), size: 0, allocated: 0, is_dir: true },
            RawEntry { id: 2, parent: 1, name: "g".into(), size: 10, allocated: 10, is_dir: false },
        ];
        let tree = rollup(0, entries).unwrap().tree;
        let f = tree.children(tree.root()).next().unwrap();
        let rects = [Rect { node: f, x: 0.0, y: 0.0, w, h }];
        frame_labels(&tree, &rects, Chrome::scaled(scale), scale).iter().map(|l| (l.x, l.y, l.width, l.height)).collect()
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
        let mut e = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true }];
        e.extend((0..folders).map(|i| RawEntry { id: i as u64 + 1, parent: 0, name: format!("d{i}"), size: 0, allocated: 0, is_dir: true }));
        e.extend(files.iter().enumerate().map(|(i, &s)| RawEntry {
            id: (folders + i) as u64 + 1,
            parent: 0,
            name: format!("clip{i}.mp4"),
            size: s,
            allocated: s,
            is_dir: false,
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
        file_labels(&tree, &[Rect { node: files[0], x, y, w, h }], scale, theme, usize::MAX)
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
        let labels = file_labels(&tree, &[Rect { node: dirs[0], x: 0.0, y: 0.0, w: 400.0, h: 300.0 }], 1.0, Theme::Dark, usize::MAX);
        assert!(labels.is_empty());
    }

    /// `frames` folders with a band (100 x 50 each) and files "clip{i}.mp4" of 50 + i by 20 px, at 100%.
    fn map_of(frames: usize, files: usize) -> (Vec<FrameLabel>, Vec<FileLabel>) {
        let (tree, dirs, fs) = flat(frames, &vec![10; files]);
        let mut rects: Vec<Rect> = dirs.iter().map(|&n| Rect { node: n, x: 0.0, y: 0.0, w: 100.0, h: 50.0 }).collect();
        rects.extend(fs.iter().enumerate().map(|(i, &n)| Rect { node: n, x: 0.0, y: 0.0, w: 50.0 + i as f64, h: 20.0 }));
        map_labels(&tree, &rects, Chrome::scaled(1.0), 1.0, Theme::Dark)
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
        let e = |id: u64, parent: u64, name: &str, is_dir: bool| RawEntry { id, parent, name: name.into(), size: 10, allocated: 10, is_dir };
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
        let model = RowModel { data: data.clone(), rows: RefCell::new(vec![row(folder), row(file)]), notify: ModelNotify::default() };

        let names: Vec<String> = (0..2).map(|i| slint::Model::row_data(&model, i).unwrap().name.to_string()).collect();

        assert_eq!(names, ["d\u{FFFD}x", "invoice\u{FFFD}fdp.exe"]);
        assert_eq!(data.tree.name(file), "invoice\u{202E}fdp.exe");
    }

    #[test]
    fn frame_labels_name_shows_a_placeholder() {
        let (tree, folder, _) = spoofed();
        let rects = [Rect { node: folder, x: 0.0, y: 0.0, w: 200.0, h: 100.0 }];
        let names: Vec<String> = frame_labels(&tree, &rects, Chrome::scaled(1.0), 1.0).iter().map(|l| l.name.to_string()).collect();
        assert_eq!(names, ["d\u{FFFD}x\\"]);
    }

    #[test]
    fn file_labels_name_shows_a_placeholder() {
        let (tree, _, file) = spoofed();
        let rects = [Rect { node: file, x: 0.0, y: 0.0, w: 200.0, h: 100.0 }];
        let names: Vec<String> = file_labels(&tree, &rects, 1.0, Theme::Dark, usize::MAX).iter().map(|l| l.name.to_string()).collect();
        assert_eq!(names, ["invoice\u{FFFD}fdp.exe"]);
    }

    #[test]
    fn hover_of_label_name_shows_a_placeholder() {
        let (tree, _, file) = spoofed();
        let data = Data { counts: crate::tree_view::file_counts(&tree), tree };
        let hover = hover_of(&data, &[Rect { node: file, x: 0.0, y: 0.0, w: 200.0, h: 100.0 }], file, 1.0, Theme::Dark);
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
        hover_of(&data, &[Rect { node, x, y, w, h }], node, scale, theme)
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
        assert!(!hover_of(&data, &[], files[0], 1.0, Theme::Dark).visible);
    }

    // ---- moving picture ----
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
            RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true },
            RawEntry { id: 1, parent: 0, name: "a.txt".into(), size: 10, allocated: 10, is_dir: false },
        ];
        let tree = rollup(0, entries).unwrap().tree;
        Arc::new(Data { counts: crate::tree_view::file_counts(&tree), tree })
    }

    fn loaded(data: &Arc<Data>) -> Loaded {
        let root = data.tree.root();
        let model = Rc::new(RowModel { data: data.clone(), rows: RefCell::new(Vec::new()), notify: ModelNotify::default() });
        Loaded { data: data.clone(), model, view: ViewState::new(root), selected: None, view_root: root, rects: Vec::new(), shown_root: None, chips: None }
    }

    fn chips() -> Vec<Chip> {
        vec![Chip { label: ".txt".into(), percent: "100.0%".into(), share: 1.0, category: Category::Document }]
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

    fn kept_chips() -> Option<Vec<Chip>> {
        SESSION.with_borrow(|s| s.loaded.as_ref().and_then(|l| l.chips.clone()))
    }

    #[test]
    fn store_chips_result_of_the_shown_scan_is_kept() {
        let d = data();
        let generation = show_finished_scan(&d);
        assert!(store_chips(generation, &d, chips()));
        assert_eq!(kept_chips(), Some(chips()));
    }

    #[test]
    fn store_chips_late_result_after_reset_is_dropped() {
        let d = data();
        let generation = show_finished_scan(&d);
        SESSION.with_borrow_mut(|s| {
            assert!(s.scan.reset());
            s.loaded = None;
        });
        assert!(!store_chips(generation, &d, chips()));
        assert_eq!(kept_chips(), None);
    }

    #[test]
    fn store_chips_late_result_of_a_scan_before_cancel_and_new_scan_is_dropped() {
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
        assert!(!store_chips(old_generation, &old, chips()));
        assert_eq!(kept_chips(), None);
    }

    #[test]
    fn store_chips_late_result_for_other_data_of_the_same_generation_is_dropped() {
        // reset keeps the generation: the scan id alone cannot tell old data from new data.
        let shown = data();
        let late = data();
        let generation = show_finished_scan(&shown);
        assert!(!store_chips(generation, &late, chips()));
        assert_eq!(kept_chips(), None);
    }

    #[test]
    fn store_chips_result_of_an_older_scan_for_the_shown_data_is_dropped() {
        let d = data();
        let generation = show_finished_scan(&d);
        assert!(!store_chips(generation - 1, &d, chips()));
        assert_eq!(kept_chips(), None);
    }

    #[test]
    fn store_chips_same_data_after_a_theme_change_is_still_kept() {
        // A theme change does not replace the data or the scan: the chips (categories) are theme free.
        let d = data();
        let generation = show_finished_scan(&d);
        assert!(store_chips(generation, &d, chips()));
        assert!(store_chips(generation, &d, chips()));
        assert_eq!(kept_chips(), Some(chips()));
    }

    fn run(timing: bool) -> (Result<(), String>, Option<(String, bool)>) {
        SESSION.with_borrow_mut(|s| s.timing = timing);
        let seen = RefCell::new(None);
        let result = restart_admin("some folder", |p, t| {
            *seen.borrow_mut() = Some((p.to_string(), t));
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
}

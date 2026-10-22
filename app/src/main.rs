//! Storage tool window: Mica backdrop, scan controls and the result banner.
//!
//! Mica recipe (proved by a spike, see `.claude/work/storagetool-wiztree-clone/logs/P3-mica-spike.md`):
//! the root `Window` background starts transparent; `slint::Window::window_handle()` only yields
//! a handle after the winit window exists, so `apply_mica` runs from `invoke_from_event_loop`
//! queued before the event loop starts. Calling it right after `show()` returns `NotSupported`.
//! If Mica is not available (Windows 10), the window falls back to the opaque Fluent background.

mod browse;
mod cli;
mod elevate;
mod model;
mod nav;
mod tree_view;

use model::{Event, ScanControl, ScanState, Summary, banner_text, error_message, format_size, group_digits, offer_elevation};
use nav::{Dir, TreeMove};
use scan::{NodeId, Progress, ScanError, Tree};
use slint::{
    ComponentHandle, Image, Model, ModelNotify, ModelRc, ModelTracker, Rgba8Pixel, SharedPixelBuffer, SharedString,
    Timer, TimerMode, VecModel, Weak,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use tree_view::{Row, SortKey, ViewState};
use treemap::Rect;

slint::include_modules!();

/// Minimum gap between progress updates sent to the UI.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Quiet time after the last treemap size change before it is laid out again.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(80);

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
            name: tree.name(row.node).into(),
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
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session {
        scan: ScanControl::default(),
        loaded: None,
        map_size: (0.0, 0.0),
        layout_seq: 0,
        scan_root: PathBuf::new(),
    });
    /// Starts one relayout after the window stops changing the treemap size.
    static RESIZE_TIMER: Timer = Timer::default();
}

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
    ui.set_scanning(matches!(state, ScanState::Scanning { .. }));
    ui.set_banner_is_error(matches!(state, ScanState::Error(_)));
    let root = SESSION.with_borrow(|s| s.scan_root.clone());
    // A root that cannot be resolved is not a drive root (same rule as `scan::scan_with`).
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let offer = matches!(&state, ScanState::Done(s) if offer_elevation(s.method, &root, elevate::is_admin()));
    ui.set_banner(banner_text(&state, offer).into());
    ui.set_show_elevate(offer);
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

/// Moves the selection outline to the selected node's area in the treemap.
fn sync_selection(ui: &MainWindow) {
    let scale = ui.window().scale_factor() as f64;
    let bounds =
        with_loaded(|l| l.selected.and_then(|n| tree_view::selection_bounds(&l.rects, n))).flatten();
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
    let Some((data, root, generation, seq, w, h)) = SESSION.with_borrow_mut(|s| {
        let l = s.loaded.as_ref()?;
        let (lw, lh) = s.map_size;
        let (w, h) = ((lw * scale).round() as u32, (lh * scale).round() as u32);
        let (data, root) = (l.data.clone(), l.view_root);
        s.layout_seq += 1;
        Some((data, root, s.scan.generation(), s.layout_seq, w, h))
    }) else {
        return;
    };
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let rects = treemap::layout(&data.tree, root, w, h);
        let image = (w > 0 && h > 0)
            .then(|| SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&treemap::render(&rects, w, h), w, h));
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let current = SESSION.with_borrow(|s| (s.scan.generation(), s.layout_seq));
            if let Some(ui) = weak.upgrade().filter(|_| current == (generation, seq)) {
                show_layout(&ui, root, rects, image);
            }
        });
    });
}

/// Shows a finished layout of `root`: the image, its rects, the title and the selection.
fn show_layout(ui: &MainWindow, root: NodeId, rects: Vec<Rect>, image: Option<SharedPixelBuffer<Rgba8Pixel>>) {
    let Some((title, can_go_up)) = with_loaded(|l| {
        l.rects = rects;
        let tree = &l.data.tree;
        (tree.path(root).display().to_string(), tree.node(root).parent.is_some())
    }) else {
        return;
    };
    if let Some(px) = image {
        ui.set_treemap(Image::from_rgba8(px));
    }
    ui.set_treemap_can_go_up(can_go_up);
    ui.set_treemap_title(title.into());
    sync_selection(ui);
}

/// Selects `node` in the tree and the treemap and scrolls the tree to it.
fn select(ui: &MainWindow, node: NodeId) {
    with_loaded(|l| {
        l.selected = Some(node);
        l.view.reveal(&l.data.tree, node);
    });
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
            data,
            model,
            view: ViewState::new(root),
            selected: Some(root),
            view_root: root,
            rects: Vec::new(),
        });
    });
    sync_rows(ui);
    relayout(ui);
}

/// Drops the shown scan.
fn clear(ui: &MainWindow) {
    SESSION.with_borrow_mut(|s| s.loaded = None);
    ui.set_rows(ModelRc::default());
    ui.set_treemap(Image::default());
    ui.set_treemap_title(SharedString::new());
    ui.set_treemap_can_go_up(false);
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
fn post_done(ui: &Weak<MainWindow>, generation: u64, data: Data, summary: Summary) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = ui.upgrade() else { return };
        if let Some(data) = SESSION.with_borrow_mut(|s| s.scan.finish(generation, summary, data)) {
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
    SESSION.with_borrow_mut(|s| s.scan_root = PathBuf::from(path));
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
            let summary = Summary::new(&stats, tree.node(tree.root()).size);
            // Counted here, not on the UI thread: it visits every node.
            let counts = tree_view::file_counts(&tree);
            post_done(&weak, generation, Data { tree, counts }, summary);
        }
        Err(ScanError::Cancelled) => post(&weak, generation, Event::Cancelled),
        Err(e) => post(&weak, generation, Event::Failed(error_message(&e))),
    });
}

/// Label shown in the drive box until the user picks a drive. Row 0 of the model.
const DRIVE_LABEL: &str = "Choose drive";

/// The drive box rows: the label, then the existing drive roots `A:\` to `Z:\`.
fn drive_roots() -> Vec<SharedString> {
    std::iter::once(DRIVE_LABEL.to_string())
        .chain(('A'..='Z').map(|c| format!("{c}:\\")).filter(|d| Path::new(d).exists()))
        .map(Into::into)
        .collect()
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

/// Shows `folder` in the treemap.
fn open_folder(ui: &MainWindow, folder: NodeId) {
    with_loaded(|l| l.view_root = folder);
    relayout(ui);
}

/// Shows the parent of the treemap folder.
fn go_up(ui: &MainWindow) {
    with_loaded(|l| {
        if let Some(p) = l.data.tree.node(l.view_root).parent {
            l.view_root = p;
        }
    });
    relayout(ui);
}

/// Opens the selected node in the treemap if it is a folder with content.
fn open_selected(ui: &MainWindow) {
    if let Some(folder) = with_loaded(|l| l.selected.and_then(|n| nav::enter_target(&l.data.tree, n))).flatten() {
        open_folder(ui, folder);
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
                tree_key(&ui, &key);
            }
        }
    });
    ui.on_map_key({
        let weak = ui.as_weak();
        move |key| {
            if let Some(ui) = weak.upgrade() {
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
            let (x, y) = to_physical(&ui, x, y);
            if let Some(n) = with_loaded(|l| tree_view::hit_test(&l.rects, x, y)).flatten() {
                select(&ui, n);
            }
        }
    });
    ui.on_treemap_open({
        let weak = ui.as_weak();
        move |x, y| {
            let Some(ui) = weak.upgrade() else { return };
            let (x, y) = to_physical(&ui, x, y);
            let target = with_loaded(|l| {
                let leaf = tree_view::hit_test(&l.rects, x, y)?;
                tree_view::drill_target(&l.data.tree, l.view_root, leaf)
            })
            .flatten();
            if let Some(folder) = target {
                open_folder(&ui, folder);
            }
        }
    });
    ui.on_treemap_up({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                go_up(&ui);
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
            match elevate::restart_as_admin(ui.get_path().trim()) {
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

fn main() {
    let ui = MainWindow::new().expect("create window");
    ui.set_drives(ModelRc::new(VecModel::from(drive_roots())));
    ui.set_banner(banner_text(&ScanState::Idle, false).into());
    ui.on_scan({
        let weak = ui.as_weak();
        move |path| {
            if let Some(ui) = weak.upgrade() {
                start_scan(&ui, &path);
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
    ui.on_browse({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                ui.set_path(dir.to_string_lossy().as_ref().into());
                ui.invoke_reset_drive();
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
    if let Some(path) = std::env::args().nth(1) {
        ui.set_path(path.as_str().into());
        start_scan(&ui, &path);
    }
    slint::run_event_loop().expect("run event loop");
}

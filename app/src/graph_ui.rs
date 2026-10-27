//! The folder flow graph's workers: the layout and picture of the graph (`spawn_graph`) and the
//! file types of the bands on screen (`count_graph_types`). Both follow the treemap's rules: a
//! late result is dropped (`is_current`, `store_types`), and every worker lets go of its
//! `Arc<Data>` clone and then calls `scan_apply::data_released`, so a pending removal can apply.
//! Also the window side of the view switch and the colour toggle (`wire`, `sync_view`).

use crate::graph_view::{self, AGE_LEGEND, ColourMode};
use crate::model::{Unit, display_name};
use crate::settings::View;
use crate::{
    Data, LABEL_CAP, LayoutJob, LegendEntry, MainWindow, SESSION, Session, Stored, current_unit, drop_hover, is_current, persist, relayout,
    scan_apply, with_loaded,
};
use flow::{Expander, Graph, Part, Theme};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use scan::{NodeId, Tree};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use treemap::FILE_LABEL_MIN_HEIGHT;
use treemap::filetype::Category;

/// Side of the expand or collapse button of a folder band, in logical pixels.
const EXPANDER_SIZE: f64 = 16.0;

/// A band label for the window, in logical pixels: the band's box and its two texts
/// (`graph_view::band_label`). The markup (P3.T4) maps it into its Slint struct.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphLabel {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub name: String,
    pub size: String,
}

/// The expand or collapse button of a folder band, in logical pixels: an `EXPANDER_SIZE` square
/// that starts at the band's right edge, centred on the band. `name` is the folder's display name;
/// `band` is the index in `Graph::bands`. The markup (P3.T4) maps it into its Slint struct.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphExpander {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub expanded: bool,
    pub name: String,
    pub band: i32,
}

/// What the window shows of one graph layout: the picture (straight RGBA, `width` x `height`
/// physical pixels), its size in logical pixels, the labels and expanders (logical pixels) and
/// the note over the graph.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphPicture {
    pub image: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub size: (f32, f32),
    pub labels: Vec<GraphLabel>,
    pub expanders: Vec<GraphExpander>,
    pub note: &'static str,
}

/// What one graph layout brings to the UI thread: the graph (physical pixels) and its picture.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphLanding {
    pub graph: Graph,
    pub picture: GraphPicture,
}

/// The graph state a layout reads, copied on the UI thread when the job starts.
#[derive(Debug, Clone, Default)]
pub struct GraphInputs {
    pub expanded: HashSet<NodeId>,
    pub open_other: HashSet<NodeId>,
    pub first_column: u16,
    pub colour: ColourMode,
    pub types: HashMap<Part, Option<Category>>,
}

thread_local! {
    /// A file-type worker runs: at most one at a time.
    static TYPES_RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// A graph ready for the window, made on the worker: the picture as a pixel buffer that the
/// window takes without a copy (`Image::from_rgba8`, as the treemap's `spawn_layout`), and the
/// labels and expanders in the window's own structs.
struct Ready {
    graph: Graph,
    /// `None` for a picture with no area.
    image: Option<SharedPixelBuffer<Rgba8Pixel>>,
    size: (f32, f32),
    labels: Vec<crate::GraphLabel>,
    expanders: Vec<crate::GraphExpander>,
    note: &'static str,
}

impl Ready {
    /// Turns a landing into what the window shows. Runs on the worker: the one copy of the
    /// picture is made there.
    fn of(GraphLanding { graph, picture }: GraphLanding) -> Ready {
        let GraphPicture { image, width, height, size, labels, expanders, note } = picture;
        let image = (width > 0 && height > 0).then(|| SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&image, width, height));
        let labels = labels
            .into_iter()
            .map(|l| crate::GraphLabel { x: l.x, y: l.y, width: l.width, height: l.height, name: l.name.into(), size: l.size.into() })
            .collect();
        let expanders = expanders
            .into_iter()
            .map(|e| crate::GraphExpander { x: e.x, y: e.y, size: e.size, expanded: e.expanded, name: e.name.into(), band: e.band })
            .collect();
        Ready { graph, image, size, labels, expanders, note }
    }
}

/// Lays out and draws the graph of folder `view` at `w` x `h` physical pixels (`scale` physical
/// pixels per logical pixel), coloured by `inputs.colour` at time `now`: pure, for the worker.
#[expect(clippy::too_many_arguments, reason = "one plain call from the worker")]
pub fn build(
    tree: &Tree,
    view: NodeId,
    inputs: &GraphInputs,
    w: u32,
    h: u32,
    scale: f64,
    theme: Theme,
    now: u32,
    unit: Unit,
) -> GraphLanding {
    let graph = flow::layout(&flow::Input {
        tree,
        view,
        expanded: &inputs.expanded,
        open_other: &inputs.open_other,
        w: f64::from(w),
        h: f64::from(h),
        scale,
        first_column: inputs.first_column,
    });
    let colours = graph_view::colours(&graph, inputs.colour, now, theme, &inputs.types);
    let image = flow::render(&graph, &colours, w, h, theme);
    let px = |v: f64| (v / scale) as f32;
    let labels = flow::label_spots(&graph, FILE_LABEL_MIN_HEIGHT * scale, LABEL_CAP)
        .into_iter()
        .map(|i| {
            let b = &graph.bands[i];
            let (name, size) = graph_view::band_label(tree, b, unit);
            GraphLabel { x: px(b.x), y: px(b.y), width: px(b.w), height: px(b.h), name, size }
        })
        .collect();
    let expanders = graph
        .bands
        .iter()
        .enumerate()
        .filter_map(|(i, b)| {
            let (Part::Folder(n), Expander::Collapsed | Expander::Expanded) = (b.part, b.expander) else { return None };
            Some(GraphExpander {
                x: px(b.x + b.w),
                y: px(b.y + b.h / 2.0) - (EXPANDER_SIZE / 2.0) as f32,
                size: EXPANDER_SIZE as f32,
                expanded: b.expander == Expander::Expanded,
                name: display_name(tree.name(n)).into_owned(),
                band: i32::try_from(i).unwrap_or(i32::MAX),
            })
        })
        .collect();
    let note = graph_view::graph_note(tree, view, &graph, w > 0 && h > 0);
    let size = (px(f64::from(w)), px(f64::from(h)));
    GraphLanding { graph, picture: GraphPicture { image, width: w, height: h, size, labels, expanders, note } }
}

/// Runs graph `job` (`begin_layout`) on a worker thread and lands it when it is still current
/// (`is_current`, the treemap's rule). The worker drops its data before the UI thread runs, and
/// the landing always calls `scan_apply::data_released`. In File type mode the landing then starts
/// the file-type worker for the bands it has no type for.
/// ponytail: one thread per request and no early stop, as `spawn_layout`.
pub(crate) fn spawn_graph(ui: &MainWindow, job: LayoutJob) {
    let LayoutJob { data, root, generation, seq, size: (lw, lh), .. } = job;
    let started = Instant::now();
    // Physical pixels, as the treemap: the layout, the picture and the label line.
    let scale = ui.window().scale_factor();
    let (w, h) = ((lw * scale).round() as u32, (lh * scale).round() as u32);
    let theme = if ui.get_dark() { Theme::Dark } else { Theme::Light };
    let unit = current_unit();
    let inputs = with_loaded(|l| GraphInputs {
        expanded: l.graph.expanded.clone(),
        open_other: l.graph.open_other.clone(),
        first_column: l.graph.first_column,
        colour: l.graph.colour,
        types: l.graph.types.clone(),
    })
    .unwrap_or_default();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let now = scan::time::from_system_time(SystemTime::now());
        let ready = Ready::of(build(&data.tree, root, &inputs, w, h, f64::from(scale), theme, now, unit));
        // Let go of the data before the UI thread runs, so a delete waiting for it can apply.
        drop(data);
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current = SESSION.with_borrow(|s| is_current(s, generation, seq));
            if current {
                land(&ui, ready, seq, started);
            }
            scan_apply::data_released(&ui);
            // After the release: a removal it applied makes this graph late (`is_current`).
            if current && SESSION.with_borrow(|s| is_current(s, generation, seq)) {
                start_types(&ui);
            }
        });
    });
}

/// Shows a current graph: the graph goes to `Loaded::graph`, the picture, labels and expanders to
/// the window, which takes the picture's buffer as it is. With `--timing`, prints how long the
/// graph took from the job's start.
fn land(ui: &MainWindow, Ready { graph, image, size, labels, expanders, note }: Ready, seq: u64, started: Instant) {
    if with_loaded(|l| l.graph.shown = Some(graph)).is_none() {
        return;
    }
    // Red stub: the window gets nothing yet.
    let _ = (ui, image, size, labels, expanders, note);
    if SESSION.with_borrow(|s| s.flags.timing) {
        eprintln!("Graph {seq}: {} ms.", started.elapsed().as_millis());
    }
}

/// Drops the graph the window shows: its picture, labels, expanders and text, and the shown
/// `Graph` (nothing is hit-tested on a graph that does not show). For a new scan and when the view
/// leaves the graph.
pub(crate) fn clear_picture(ui: &MainWindow) {
    let _ = ui;
}

/// The legend of the age colours in `theme`: the texts of `AGE_LEGEND` in the colours the picture
/// uses (`flow::age::age_colour`).
pub(crate) fn age_legend(theme: Theme) -> Vec<LegendEntry> {
    AGE_LEGEND
        .iter()
        .map(|&(age, text)| {
            let [r, g, b] = flow::age::age_colour(age, theme);
            LegendEntry { text: text.into(), color: slint::Color::from_rgb_u8(r, g, b) }
        })
        .collect()
}

/// Pushes the view switch and the graph bar into the window: which view shows, whether the switch
/// and the toggle work (the graph could show: a finished scan with data), the chosen colours and
/// the age legend in the window's theme.
pub(crate) fn sync_view(ui: &MainWindow) {
    let _ = ui;
}

/// True when the graph could show: the view switch works.
fn offered(s: &Session) -> bool {
    graph_view::shown_view(s.scan.state(), s.loaded.is_some(), View::Graph) == View::Graph
}

/// A view was chosen in the top bar (0 treemap, 1 graph): it is kept in the settings and saved,
/// the hover goes, a graph left is dropped, and the chosen view is laid out. Nothing while the
/// switch is off or for the view that is already chosen.
fn view_chosen(ui: &MainWindow, i: i32) {
    let _ = (ui, i);
}

/// Colours were chosen in the graph bar (0 age, 1 file type): the graph is drawn again in them.
/// The choice lasts for the session only.
fn colour_chosen(ui: &MainWindow, i: i32) {
    let _ = (ui, i);
}

/// Wires the view switch, the colour toggle and the graph's size. The graph's own input (click,
/// keys, menu, expanders, hover, wheel) gets its handlers in P4.T3; until then it does nothing.
pub(crate) fn wire(ui: &MainWindow) {
    ui.on_view_chosen({
        let weak = ui.as_weak();
        move |i| {
            if let Some(ui) = weak.upgrade() {
                view_chosen(&ui, i);
            }
        }
    });
    ui.on_colour_chosen({
        let weak = ui.as_weak();
        move |i| {
            if let Some(ui) = weak.upgrade() {
                colour_chosen(&ui, i);
            }
        }
    });
    // The graph has the treemap's place: one size, one debounced layout (`treemap-resized`).
    ui.on_graph_resized({
        let weak = ui.as_weak();
        move |w, h| {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_treemap_resized(w, h);
            }
        }
    });
    ui.on_graph_click(|_, _| {});
    ui.on_graph_open(|_, _| {});
    ui.on_graph_menu(|_, _, _, _| {});
    ui.on_graph_menu_key(|_, _, _, _| {});
    ui.on_graph_key(|_| {});
    ui.on_graph_expand(|_| {});
    ui.on_graph_hover(|_, _| {});
    ui.on_graph_leave(|| {});
    ui.on_graph_scroll(|_| {});
}

/// True when the graph shows in File type mode: its bands want their file types.
fn wants_types(s: &Session) -> bool {
    s.loaded.as_ref().is_some_and(|l| {
        l.graph.colour == ColourMode::FileType && graph_view::shown_view(s.scan.state(), true, s.settings.view) == View::Graph
    })
}

/// In File type mode, starts the file-type worker for the shown graph's bands that have no type
/// yet (`graph_view::missing_types`). Nothing while one runs: its landing lays out again, and that
/// layout asks for what is still missing.
fn start_types(ui: &MainWindow) {
    if TYPES_RUNNING.get() {
        return;
    }
    let job = SESSION.with_borrow(|s| {
        let l = s.loaded.as_ref().filter(|_| wants_types(s))?;
        let missing = graph_view::missing_types(l.graph.shown.as_ref()?, &l.graph.types);
        (!missing.is_empty()).then(|| (s.scan.generation(), l.data.clone(), l.edits, missing))
    });
    if let Some((generation, data, edits, parts)) = job {
        count_graph_types(ui, generation, data, edits, parts);
    }
}

/// The dominant file type of each of `parts` (`flow::members` then `flow::dominant`).
fn types_of(tree: &Tree, parts: &[Part]) -> Vec<(Part, Option<Category>)> {
    parts.iter().map(|p| (*p, flow::dominant(tree, &flow::members(tree, p)))).collect()
}

/// Finds the file types of `parts` in `data` (scan `generation` after `edits` removals) on one
/// worker thread. The landing keeps them by the rules of `store_totals` (`store_types`), lets go
/// of the data, always calls `scan_apply::data_released`, then lays out again while the graph
/// shows in File type mode: with the new types, or to count again after a removal.
fn count_graph_types(ui: &MainWindow, generation: u64, data: Arc<Data>, edits: u64, parts: Vec<Part>) {
    TYPES_RUNNING.set(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let found = types_of(&data.tree, &parts);
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            TYPES_RUNNING.set(false);
            let Some(ui) = weak.upgrade() else { return };
            store_types(generation, &data, edits, found);
            // Let go of the data, so a delete waiting for it can apply.
            drop(data);
            scan_apply::data_released(&ui);
            if SESSION.with_borrow(wants_types) {
                relayout(&ui);
            }
        });
    });
}

/// Keeps the file types `found` for `data` of scan `generation` after `edits` removals, with the
/// rules of `store_totals`: `Late` for another scan or data the window no longer shows, `Recount`
/// when a removal came after the count. Only `Kept` adds them to `GraphState::types`.
pub(crate) fn store_types(generation: u64, data: &Arc<Data>, edits: u64, found: Vec<(Part, Option<Category>)>) -> Stored {
    SESSION.with_borrow_mut(|s| {
        let Some(l) = s.scan.is_current(generation).then_some(s.loaded.as_mut()).flatten() else {
            return Stored::Late;
        };
        if l.edits != edits {
            Stored::Recount
        } else if !Arc::ptr_eq(&l.data, data) {
            Stored::Late
        } else {
            l.graph.types.extend(found);
            Stored::Kept
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, Summary};
    use crate::settings::View;
    use crate::{Loaded, RowModel, SESSION, begin_layout, is_current};
    use i_slint_backend_testing::{AccessibleRole, ElementHandle};
    use scan::Method;
    use slint::Model;
    use scan::rollup::{RawEntry, rollup};
    use std::rc::Rc;

    /// root { a { a1.mp4 550, s { s1.bin 50 } }, b { b1.zip 300 }, c.txt 100 }
    fn data() -> Arc<Data> {
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
            modified: scan::time::UNKNOWN,
        };
        let entries = vec![
            e(0, 0, "root", 0, true),
            e(1, 0, "a", 0, true),
            e(2, 0, "b", 0, true),
            e(3, 0, "c.txt", 100, false),
            e(4, 1, "a1.mp4", 550, false),
            e(5, 2, "b1.zip", 300, false),
            e(6, 1, "s", 0, true),
            e(7, 6, "s1.bin", 50, false),
        ];
        let tree = rollup(0, entries).unwrap().tree;
        Arc::new(Data { counts: crate::tree_view::file_counts(&tree), tree })
    }

    fn loaded(data: &Arc<Data>) -> Loaded {
        let root = data.tree.root();
        Loaded {
            data: data.clone(),
            serial: crate::next_serial(),
            model: Rc::new(RowModel::new(data.clone())),
            view: crate::tree_view::ViewState::new(root),
            selected: None,
            view_root: root,
            rects: Vec::new(),
            shown_root: None,
            totals: None,
            edits: 0,
            graph: Default::default(),
        }
    }

    fn summary() -> Summary {
        Summary { method: Method::Walk, files: 3, dirs: 3, skipped: 0, elapsed: std::time::Duration::ZERO, phases: Default::default(), total: 1000 }
    }

    /// A finished scan with `data` on screen and `view` chosen. Returns its generation.
    fn show_finished_scan(data: &Arc<Data>, view: View) -> u64 {
        SESSION.with_borrow_mut(|s| {
            let (generation, _) = s.scan.start().unwrap();
            assert!(s.scan.apply(generation, Event::Finished(summary())));
            s.loaded = Some(loaded(data));
            s.settings.view = view;
            s.map_size = (400.0, 300.0);
            generation
        })
    }

    fn found(d: &Data) -> Vec<(Part, Option<Category>)> {
        vec![(Part::Folder(d.tree.root()), Some(Category::Video))]
    }

    fn kept_types() -> HashMap<Part, Option<Category>> {
        SESSION.with_borrow(|s| s.loaded.as_ref().map(|l| l.graph.types.clone()).unwrap_or_default())
    }

    // ---- begin_layout: one layout sequence for both views ----
    #[test]
    fn begin_layout_treemap_view_is_the_treemap_job_as_before() {
        let d = data();
        let generation = show_finished_scan(&d, View::Treemap);
        let seq = SESSION.with_borrow(|s| s.layout_seq);

        let job = SESSION.with_borrow_mut(begin_layout).unwrap();

        assert!(Arc::ptr_eq(&job.data, &d));
        assert_eq!(
            (job.root, job.generation, job.seq, job.size, job.view),
            (d.tree.root(), generation, seq + 1, (400.0, 300.0), View::Treemap)
        );
        assert_eq!(SESSION.with_borrow(|s| s.layout_seq), seq + 1);
    }

    #[test]
    fn begin_layout_graph_view_is_a_graph_job_with_the_next_seq() {
        let d = data();
        let generation = show_finished_scan(&d, View::Graph);
        let seq = SESSION.with_borrow(|s| s.layout_seq);

        let job = SESSION.with_borrow_mut(begin_layout).unwrap();

        assert!(Arc::ptr_eq(&job.data, &d));
        assert_eq!(
            (job.root, job.generation, job.seq, job.size, job.view),
            (d.tree.root(), generation, seq + 1, (400.0, 300.0), View::Graph)
        );
        assert_eq!(SESSION.with_borrow(|s| s.layout_seq), seq + 1);
    }

    #[test]
    fn begin_layout_graph_chosen_while_scanning_is_a_treemap_job() {
        let d = data();
        show_finished_scan(&d, View::Graph);
        SESSION.with_borrow_mut(|s| {
            let _ = s.scan.start().unwrap();
        });
        let job = SESSION.with_borrow_mut(begin_layout).unwrap();
        assert_eq!(job.view, View::Treemap);
    }

    // ---- is_current: the one landing rule of both views ----
    #[test]
    fn is_current_latest_job_of_the_shown_scan_lands() {
        show_finished_scan(&data(), View::Graph);
        let job = SESSION.with_borrow_mut(begin_layout).unwrap();
        assert!(SESSION.with_borrow(|s| is_current(s, job.generation, job.seq)));
    }

    #[test]
    fn is_current_after_a_newer_layout_is_dropped() {
        show_finished_scan(&data(), View::Graph);
        let old = SESSION.with_borrow_mut(begin_layout).unwrap();
        let _new = SESSION.with_borrow_mut(begin_layout).unwrap();
        assert!(!SESSION.with_borrow(|s| is_current(s, old.generation, old.seq)));
    }

    #[test]
    fn is_current_of_another_generation_is_dropped() {
        show_finished_scan(&data(), View::Graph);
        let job = SESSION.with_borrow_mut(begin_layout).unwrap();
        assert!(!SESSION.with_borrow(|s| is_current(s, job.generation - 1, job.seq)));
        SESSION.with_borrow_mut(|s| {
            let _ = s.scan.start().unwrap();
        });
        assert!(!SESSION.with_borrow(|s| is_current(s, job.generation, job.seq)));
    }

    // ---- store_types: the rules of store_totals ----
    #[test]
    fn store_types_result_for_the_shown_data_is_kept() {
        let d = data();
        let generation = show_finished_scan(&d, View::Graph);
        assert_eq!(store_types(generation, &d, 0, found(&d)), Stored::Kept);
        assert_eq!(kept_types(), found(&d).into_iter().collect());
    }

    #[test]
    fn store_types_result_of_another_scan_is_late() {
        let d = data();
        let generation = show_finished_scan(&d, View::Graph);
        assert_eq!(store_types(generation - 1, &d, 0, found(&d)), Stored::Late);
        // Other data of the same generation (reset keeps it) is late too.
        let other = data();
        assert_eq!(store_types(generation, &other, 0, found(&other)), Stored::Late);
        assert!(kept_types().is_empty());
    }

    #[test]
    fn store_types_counted_before_a_removal_is_recount() {
        let d = data();
        let generation = show_finished_scan(&d, View::Graph);
        crate::with_loaded(|l| crate::drop_types(l, &[]));
        assert_eq!(store_types(generation, &d, 0, found(&d)), Stored::Recount);
        assert!(kept_types().is_empty());
    }

    // ---- TP3.4: the view switch, the colour toggle and the legend (headless) ----

    /// A window on the testing backend (this thread only), wired as `main` wires it, with a
    /// finished scan of `data()` installed and `view` chosen. No delete is started here, so the
    /// delete wiring (`delete_ui::wire`) is not needed.
    fn window(view: View) -> MainWindow {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        crate::wire_tree_and_treemap(&ui);
        wire(&ui);
        SESSION.with_borrow_mut(|s| {
            let (generation, _) = s.scan.start().expect("no scan runs");
            assert!(s.scan.apply(generation, Event::Finished(summary())));
            s.settings.view = view;
        });
        crate::install(&ui, data());
        crate::show_state(&ui);
        ui.show().unwrap();
        ui
    }

    /// The buttons the window shows with this label (found once per component level).
    fn buttons(ui: &MainWindow, label: &str) -> Vec<ElementHandle> {
        ElementHandle::find_by_accessible_label(ui, label).filter(|e| e.accessible_role() == Some(AccessibleRole::Button)).collect()
    }

    fn button(ui: &MainWindow, label: &str) -> ElementHandle {
        buttons(ui, label).into_iter().next().unwrap_or_else(|| panic!("a {label:?} button"))
    }

    fn click(ui: &MainWindow, label: &str) {
        button(ui, label).invoke_accessible_default_action();
    }

    fn graph_shows(ui: &MainWindow) -> bool {
        ElementHandle::find_by_accessible_label(ui, "Folder graph").next().is_some()
    }

    fn chosen_view() -> View {
        SESSION.with_borrow(|s| s.settings.view)
    }

    fn colour() -> ColourMode {
        SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().graph.colour)
    }

    const LEGEND: [&str; 4] =
        ["Changed in the last 6 months", "Changed 6 to 24 months ago", "Changed more than 24 months ago", "Date not known"];

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn switch_after_a_finished_scan_is_two_enabled_buttons_and_while_scanning_both_are_disabled() {
        let ui = window(View::Treemap);
        for label in ["Treemap", "Graph"] {
            assert_eq!(button(&ui, label).accessible_enabled(), Some(true), "{label} after a finished scan");
        }
        assert_eq!(button(&ui, "Treemap").accessible_checked(), Some(true));
        assert_eq!(button(&ui, "Graph").accessible_checked(), Some(false));

        SESSION.with_borrow_mut(|s| {
            let _ = s.scan.start().expect("no scan runs");
        });
        crate::show_state(&ui);

        for label in ["Treemap", "Graph"] {
            assert_eq!(button(&ui, label).accessible_enabled(), Some(false), "{label} while scanning");
        }
        // A disabled switch changes nothing.
        click(&ui, "Graph");
        assert_eq!(chosen_view(), View::Treemap);
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn graph_button_shows_the_folder_graph_and_treemap_goes_back() {
        let ui = window(View::Treemap);
        assert!(!graph_shows(&ui));

        click(&ui, "Graph");

        assert_eq!(chosen_view(), View::Graph);
        assert!(graph_shows(&ui), "the element labelled \"Folder graph\" shows");
        assert_eq!(button(&ui, "Graph").accessible_checked(), Some(true));

        click(&ui, "Treemap");

        assert_eq!(chosen_view(), View::Treemap);
        assert!(!graph_shows(&ui));
        assert_eq!(button(&ui, "Treemap").accessible_checked(), Some(true));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn graph_bar_has_age_and_file_type_and_age_shows_the_four_legend_texts() {
        assert_eq!(AGE_LEGEND.map(|(_, text)| text), LEGEND);
        let ui = window(View::Graph);
        assert!(graph_shows(&ui));
        assert_eq!(button(&ui, "Age").accessible_checked(), Some(true));
        assert_eq!(button(&ui, "File type").accessible_checked(), Some(false));
        for text in LEGEND {
            assert!(ElementHandle::find_by_accessible_label(&ui, text).next().is_some(), "legend {text:?}");
        }

        click(&ui, "File type");

        assert_eq!(colour(), ColourMode::FileType);
        assert_eq!(button(&ui, "File type").accessible_checked(), Some(true));
        // The footer chips are the legend of the file types: no age legend.
        for text in LEGEND {
            assert!(ElementHandle::find_by_accessible_label(&ui, text).next().is_none(), "no legend {text:?}");
        }

        click(&ui, "Age");

        assert_eq!(colour(), ColourMode::Age);
        assert!(ElementHandle::find_by_accessible_label(&ui, LEGEND[0]).next().is_some());
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn legend_swatches_are_the_age_colours_of_the_window_theme() {
        let ui = window(View::Graph);
        for (scheme, theme) in [(slint::language::ColorScheme::Dark, Theme::Dark), (slint::language::ColorScheme::Light, Theme::Light)] {
            ui.global::<crate::Palette>().set_color_scheme(scheme);
            sync_view(&ui);
            let got: Vec<(String, slint::Color)> = ui.get_graph_legend().iter().map(|e| (e.text.to_string(), e.color)).collect();
            let want: Vec<(String, slint::Color)> = AGE_LEGEND
                .iter()
                .map(|&(age, text)| {
                    let [r, g, b] = flow::age::age_colour(age, theme);
                    (text.to_string(), slint::Color::from_rgb_u8(r, g, b))
                })
                .collect();
            assert_eq!(got, want, "{theme:?}");
        }
    }

    // ---- land: the picture goes to the window as a buffer, without a copy on the UI thread ----
    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn land_shows_the_picture_labels_and_expanders_and_a_new_scan_drops_them() {
        let ui = window(View::Graph);
        let d = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.clone());
        let landing = build(&d.tree, d.tree.root(), &GraphInputs::default(), 400, 300, 1.0, Theme::Light, 0, Unit::Dynamic);
        assert!(!landing.picture.labels.is_empty() && !landing.picture.expanders.is_empty());

        land(&ui, Ready::of(landing.clone()), 1, Instant::now());

        let shown = SESSION.with_borrow(|s| s.loaded.as_ref().and_then(|l| l.graph.shown.clone()));
        assert_eq!(shown, Some(landing.graph.clone()));
        let picture = ui.get_graph_picture().to_rgba8();
        assert!(picture.is_some(), "an RGBA picture in the window");
        let picture = picture.unwrap();
        assert_eq!((picture.width(), picture.height()), (400, 300));
        assert_eq!(picture.as_bytes(), &landing.picture.image[..]);
        assert_eq!((ui.get_graph_layout_width(), ui.get_graph_layout_height()), (400.0, 300.0));
        let labels: Vec<String> = ui.get_graph_labels().iter().map(|l| l.name.to_string()).collect();
        assert_eq!(labels, landing.picture.labels.iter().map(|l| l.name.clone()).collect::<Vec<_>>());
        let expanders: Vec<(String, i32)> = ui.get_graph_expanders().iter().map(|e| (e.name.to_string(), e.band)).collect();
        assert_eq!(expanders, landing.picture.expanders.iter().map(|e| (e.name.clone(), e.band)).collect::<Vec<_>>());
        // The expander has its accessible label.
        let name = &landing.picture.expanders[0].name;
        assert!(!buttons(&ui, &format!("Expand {name}")).is_empty(), "an \"Expand {name}\" button");

        // A new scan drops the graph of the old one.
        crate::install(&ui, data());

        let size = ui.get_graph_picture().size();
        assert_eq!((size.width, size.height), (0, 0), "no picture");
        assert_eq!((ui.get_graph_labels().row_count(), ui.get_graph_expanders().row_count()), (0, 0));
        assert!(SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().graph.shown.is_none()));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn treemap_chosen_drops_the_graph_picture() {
        let ui = window(View::Graph);
        let d = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.clone());
        let landing = build(&d.tree, d.tree.root(), &GraphInputs::default(), 40, 30, 1.0, Theme::Light, 0, Unit::Dynamic);
        land(&ui, Ready::of(landing), 1, Instant::now());

        click(&ui, "Treemap");

        let size = ui.get_graph_picture().size();
        assert_eq!((size.width, size.height), (0, 0), "no picture");
        assert!(SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().graph.shown.is_none()));
    }

    // ---- build: the worker's pure part ----
    #[test]
    fn build_labels_expanders_and_size_are_in_logical_pixels() {
        let d = data();
        let inputs = GraphInputs::default();
        let (w, h, scale) = (800, 600, 2.0);

        let got = build(&d.tree, d.tree.root(), &inputs, w, h, scale, Theme::Light, 0, Unit::Dynamic);

        let pic = &got.picture;
        assert_eq!((pic.width, pic.height, pic.size), (w, h, (400.0, 300.0)));
        assert_eq!(pic.image.len(), (w * h * 4) as usize);
        assert!(!got.graph.bands.is_empty());
        let spots = flow::label_spots(&got.graph, treemap::FILE_LABEL_MIN_HEIGHT * scale, crate::LABEL_CAP);
        assert_eq!(pic.labels.len(), spots.len());
        for (label, &i) in pic.labels.iter().zip(&spots) {
            let b = &got.graph.bands[i];
            assert_eq!((label.x, label.y, label.height), ((b.x / scale) as f32, (b.y / scale) as f32, (b.h / scale) as f32));
        }
        let with_expander: Vec<usize> =
            (0..got.graph.bands.len()).filter(|&i| got.graph.bands[i].expander != flow::Expander::None).collect();
        assert_eq!(pic.expanders.iter().map(|e| e.band as usize).collect::<Vec<_>>(), with_expander);
        assert!(!with_expander.is_empty());
        for e in &pic.expanders {
            let b = &got.graph.bands[e.band as usize];
            assert_eq!(e.x, ((b.x + b.w) / scale) as f32);
            assert_eq!(e.name, crate::model::display_name(d.tree.name(match b.part {
                Part::Folder(n) => n,
                _ => unreachable!(),
            })));
            assert!(!e.expanded);
        }
        assert_eq!(pic.note, "");
    }

    #[test]
    fn build_zero_area_is_an_empty_picture_without_a_note() {
        let d = data();
        let got = build(&d.tree, d.tree.root(), &GraphInputs::default(), 0, 0, 1.0, Theme::Dark, 0, Unit::Dynamic);
        assert!(got.picture.image.is_empty() && got.graph.bands.is_empty());
        assert_eq!(got.picture.note, "");
    }
}

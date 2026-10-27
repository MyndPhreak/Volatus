//! The folder flow graph's workers: the layout and picture of the graph (`spawn_graph`) and the
//! file types of the bands on screen (`count_graph_types`). Both follow the treemap's rules: a
//! late result is dropped (`is_current`, `store_types`), and every worker lets go of its
//! `Arc<Data>` clone and then calls `scan_apply::data_released`, so a pending removal can apply.

use crate::graph_view::{self, ColourMode};
use crate::model::{Unit, display_name};
use crate::settings::View;
use crate::{Data, LABEL_CAP, LayoutJob, MainWindow, SESSION, Session, Stored, current_unit, is_current, relayout, scan_apply, with_loaded};
use flow::{Expander, Graph, Part, Theme};
use slint::ComponentHandle;
use scan::{NodeId, Tree};
use std::cell::{Cell, RefCell};
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
    /// The picture of the latest graph that landed, with the `Loaded::serial` of its scan. The
    /// markup (P3.T4) shows it; a picture of another scan is never given out (`shown_picture`).
    static PICTURE: RefCell<Option<(u64, GraphPicture)>> = const { RefCell::new(None) };
    /// A file-type worker runs: at most one at a time.
    static TYPES_RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// The picture of the shown scan's latest graph, if one landed.
#[cfg_attr(not(test), expect(dead_code, reason = "read by the markup in P3.T4"))]
pub(crate) fn shown_picture() -> Option<GraphPicture> {
    let serial = SESSION.with_borrow(|s| s.loaded.as_ref().map(|l| l.serial))?;
    PICTURE.with_borrow(|p| p.as_ref().filter(|(of, _)| *of == serial).map(|(_, pic)| pic.clone()))
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
        let landing = build(&data.tree, root, &inputs, w, h, f64::from(scale), theme, now, unit);
        // Let go of the data before the UI thread runs, so a delete waiting for it can apply.
        drop(data);
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current = SESSION.with_borrow(|s| is_current(s, generation, seq));
            if current {
                land(landing, seq, started);
            }
            scan_apply::data_released(&ui);
            // After the release: a removal it applied makes this graph late (`is_current`).
            if current && SESSION.with_borrow(|s| is_current(s, generation, seq)) {
                start_types(&ui);
            }
        });
    });
}

/// Shows a current graph: the graph goes to `Loaded::graph`, the picture to the window. With
/// `--timing`, prints how long the graph took from the job's start.
fn land(GraphLanding { graph, picture }: GraphLanding, seq: u64, started: Instant) {
    let Some(serial) = with_loaded(|l| {
        l.graph.shown = Some(graph);
        l.serial
    }) else {
        return;
    };
    PICTURE.with_borrow_mut(|p| *p = Some((serial, picture)));
    if SESSION.with_borrow(|s| s.flags.timing) {
        eprintln!("Graph {seq}: {} ms.", started.elapsed().as_millis());
    }
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
    use scan::Method;
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

    // ---- land: the graph and the picture of the shown scan ----
    #[test]
    fn land_keeps_the_graph_and_gives_the_picture_of_the_shown_scan_only() {
        let d = data();
        show_finished_scan(&d, View::Graph);
        let landing = build(&d.tree, d.tree.root(), &GraphInputs::default(), 40, 30, 1.0, Theme::Light, 0, Unit::Dynamic);

        land(landing.clone(), 1, Instant::now());

        let shown = SESSION.with_borrow(|s| s.loaded.as_ref().and_then(|l| l.graph.shown.clone()));
        assert_eq!((shown, shown_picture()), (Some(landing.graph), Some(landing.picture)));
        // Another scan on screen: the picture is not its own.
        SESSION.with_borrow_mut(|s| s.loaded = Some(loaded(&data())));
        assert_eq!(shown_picture(), None);
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

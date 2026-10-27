//! The folder flow graph's workers: the layout and picture of the graph (`spawn_graph`) and the
//! file types of the bands on screen (`count_graph_types`). Both follow the treemap's rules: a
//! late result is dropped, and every worker lets go of its `Arc<Data>` clone and then calls
//! `scan_apply::data_released`, so a pending removal can apply.
#![cfg_attr(not(test), expect(dead_code, reason = "wired in this task"))]

use crate::graph_view::ColourMode;
use crate::model::Unit;
use crate::{Data, LayoutJob, MainWindow, Stored};
use flow::{Graph, Part, Theme};
use scan::{NodeId, Tree};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use treemap::filetype::Category;

/// A band label for the window, in logical pixels: the band's box and its two texts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphLabel {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub name: String,
    pub size: String,
}

/// The expand or collapse button of a folder band, in logical pixels: a `size` square at the
/// band's right edge, centred on the band. `band` is the index in `Graph::bands`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphExpander {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub expanded: bool,
    pub name: String,
    pub band: i32,
}

/// What one graph layout brings to the window: the graph (physical pixels), its picture
/// (straight RGBA, `width` x `height` physical pixels), the picture's size in logical pixels,
/// the labels and expanders (logical pixels) and the note.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphLanding {
    pub graph: Graph,
    pub image: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub size: (f32, f32),
    pub labels: Vec<GraphLabel>,
    pub expanders: Vec<GraphExpander>,
    pub note: &'static str,
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

/// Lays out and draws the graph of `view` at `w` x `h` physical pixels: pure, for the worker.
#[allow(clippy::too_many_arguments)]
pub fn build(
    _tree: &Tree,
    _view: NodeId,
    _inputs: &GraphInputs,
    _w: u32,
    _h: u32,
    _scale: f64,
    _theme: Theme,
    _now: u32,
    _unit: Unit,
) -> GraphLanding {
    GraphLanding::default()
}

/// Runs graph `job` (`begin_layout`) on a worker thread and lands it when it is still current.
pub(crate) fn spawn_graph(_ui: &MainWindow, _job: LayoutJob) {}

/// Keeps the file types `found` for `data` of scan `generation` after `edits` removals, with the
/// rules of `store_totals`: `Late` for another scan or other data, `Recount` after a removal.
pub(crate) fn store_types(_generation: u64, _data: &Arc<Data>, _edits: u64, _found: Vec<(Part, Option<Category>)>) -> Stored {
    Stored::Late
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

    // ---- build: the worker's pure part ----
    #[test]
    fn build_labels_expanders_and_size_are_in_logical_pixels() {
        let d = data();
        let inputs = GraphInputs::default();
        let (w, h, scale) = (800, 600, 2.0);

        let got = build(&d.tree, d.tree.root(), &inputs, w, h, scale, Theme::Light, 0, Unit::Dynamic);

        assert_eq!((got.width, got.height, got.size), (w, h, (400.0, 300.0)));
        assert_eq!(got.image.len(), (w * h * 4) as usize);
        assert!(!got.graph.bands.is_empty());
        let spots = flow::label_spots(&got.graph, treemap::FILE_LABEL_MIN_HEIGHT * scale, crate::LABEL_CAP);
        assert_eq!(got.labels.len(), spots.len());
        for (label, &i) in got.labels.iter().zip(&spots) {
            let b = &got.graph.bands[i];
            assert_eq!((label.x, label.y, label.height), ((b.x / scale) as f32, (b.y / scale) as f32, (b.h / scale) as f32));
        }
        let with_expander: Vec<usize> =
            (0..got.graph.bands.len()).filter(|&i| got.graph.bands[i].expander != flow::Expander::None).collect();
        assert_eq!(got.expanders.iter().map(|e| e.band as usize).collect::<Vec<_>>(), with_expander);
        assert!(!with_expander.is_empty());
        for e in &got.expanders {
            let b = &got.graph.bands[e.band as usize];
            assert_eq!(e.x, ((b.x + b.w) / scale) as f32);
            assert_eq!(e.name, crate::model::display_name(d.tree.name(match b.part {
                Part::Folder(n) => n,
                _ => unreachable!(),
            })));
            assert!(!e.expanded);
        }
        assert_eq!(got.note, "");
    }

    #[test]
    fn build_zero_area_is_an_empty_picture_without_a_note() {
        let d = data();
        let got = build(&d.tree, d.tree.root(), &GraphInputs::default(), 0, 0, 1.0, Theme::Dark, 0, Unit::Dynamic);
        assert!(got.image.is_empty() && got.graph.bands.is_empty());
        assert_eq!(got.note, "");
    }
}

//! The folder flow graph's workers: the layout and picture of the graph (`spawn_graph`) and the
//! file types of the bands on screen (`count_graph_types`). Both follow the treemap's rules: a
//! late result is dropped (`is_current`, `store_types`), and every worker lets go of its
//! `Arc<Data>` clone and then calls `scan_apply::data_released`, so a pending removal can apply.
//! Also the window side of the view switch and the colour toggle (`wire`, `sync_view`), and the
//! graph's input: every action reads `GraphState::shown` (`with_shown`) and ends in the treemap's
//! own paths (`select`, `open_node`, `go_up`, `open_menu`, `scan_apply::request_delete_of`).

use crate::graph_view::{self, AGE_LEGEND, Act, ColourMode, KeyAct};
use crate::menu::At;
use crate::model::{Unit, display_name};
use crate::settings::View;
use crate::{
    Data, Hover, LABEL_CAP, LayoutJob, LegendEntry, Loaded, MainWindow, SESSION, Session, Source, Stored, current_unit, delete_ui,
    drop_hover, go_up, is_current, open_menu, open_node, persist, relayout, scan_apply, select, stop_move, sync_selection, tip,
    tip_event, to_physical, with_loaded,
};
use flow::{Expander, Graph, Part, Theme, VISIBLE_COLUMNS};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use scan::{NodeId, Tree};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
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
    let inputs = graph_inputs();
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

/// The graph state a layout reads now (`GraphInputs`), copied from `Loaded::graph`.
fn graph_inputs() -> GraphInputs {
    with_loaded(|l| GraphInputs {
        expanded: l.graph.expanded.clone(),
        open_other: l.graph.open_other.clone(),
        first_column: l.graph.first_column,
        colour: l.graph.colour,
        // Only the file type colours read the cache.
        types: if l.graph.colour == ColourMode::FileType { l.graph.types.clone() } else { HashMap::new() },
    })
    .unwrap_or_default()
}

/// Shows a current graph: the graph goes to `Loaded::graph`, the picture, labels and expanders to
/// the window, which takes the picture's buffer as it is. With `--timing`, prints how long the
/// graph took from the job's start.
fn land(ui: &MainWindow, Ready { graph, image, size, labels, expanders, note }: Ready, seq: u64, started: Instant) {
    let unit = current_unit();
    let Some((value, crumbs)) = with_loaded(|l| {
        let tree = &l.data.tree;
        // A current graph is of the view folder (`show_folder` lays out again).
        let value = graph_view::summary(tree, &graph, l.view_root, unit);
        let crumbs: Vec<crate::Crumb> = crate::tree_view::crumbs(tree, l.view_root).into_iter().map(crate::crumb).collect();
        l.graph.shown = Some(graph);
        (value, crumbs)
    }) else {
        return;
    };
    ui.set_graph_picture(image.map(Image::from_rgba8).unwrap_or_default());
    ui.set_graph_layout_width(size.0);
    ui.set_graph_layout_height(size.1);
    ui.set_graph_labels(ModelRc::new(VecModel::from(labels)));
    ui.set_graph_expanders(ModelRc::new(VecModel::from(expanders)));
    ui.set_graph_note(note.into());
    ui.set_graph_value(value.into());
    // The crumbs are shared with the treemap, as its landing sets them (`show_layout`): a new model
    // only for another folder, so a crumb keeps its focus.
    if !ui.get_crumbs().iter().eq(crumbs.iter().cloned()) {
        ui.set_crumbs(ModelRc::new(VecModel::from(crumbs)));
    }
    // The legend in the colours of the theme the picture was drawn in.
    sync_view(ui);
    // The outline on the new bands, and the old hover goes.
    sync_selection(ui);
    drop_hover(ui);
    if SESSION.with_borrow(|s| s.flags.timing) {
        eprintln!("Graph {seq}: {} ms.", started.elapsed().as_millis());
    }
}

/// Drops the graph the window shows: its picture, labels, expanders and text, and the shown
/// `Graph` (nothing is hit-tested on a graph that does not show). For a new scan and when the view
/// leaves the graph.
pub(crate) fn clear_picture(ui: &MainWindow) {
    with_loaded(|l| l.graph.shown = None);
    forget_hover(ui);
    ui.set_graph_picture(Image::default());
    ui.set_graph_labels(ModelRc::default());
    ui.set_graph_expanders(ModelRc::default());
    ui.set_graph_note(SharedString::new());
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
    let (shown, enabled, colour) = SESSION.with_borrow(|s| {
        let loaded = s.loaded.is_some();
        let colour = s.loaded.as_ref().map(|l| l.graph.colour).unwrap_or_default();
        let shown = graph_view::shown_view(s.scan.state(), loaded, s.settings.view) == View::Graph;
        (shown, offered(s), colour)
    });
    let theme = if ui.get_dark() { Theme::Dark } else { Theme::Light };
    ui.set_graph_shown(shown);
    ui.set_views_enabled(enabled);
    ui.set_colour_choice(match colour {
        ColourMode::Age => 0,
        ColourMode::FileType => 1,
    });
    ui.set_graph_legend(ModelRc::new(VecModel::from(age_legend(theme))));
}

/// True when the graph could show: the view switch works.
fn offered(s: &Session) -> bool {
    graph_view::shown_view(s.scan.state(), s.loaded.is_some(), View::Graph) == View::Graph
}

/// A view was chosen in the top bar (0 treemap, 1 graph): it is kept in the settings and saved,
/// the hover goes, a graph left is dropped, and the chosen view is laid out. Nothing while the
/// switch is off or for the view that is already chosen.
fn view_chosen(ui: &MainWindow, i: i32) {
    let view = if i == 1 { View::Graph } else { View::Treemap };
    if !SESSION.with_borrow_mut(|s| offered(s) && std::mem::replace(&mut s.settings.view, view) != view) {
        return;
    }
    persist(ui);
    drop_hover(ui);
    match view {
        View::Treemap => clear_picture(ui),
        // The treemap hides: a move on it stops, and a zoom out waiting for its next layout goes.
        // A first-scan fade still waiting (`fade_next`) stays: the treemap of this scan was never
        // drawn, so its first picture fades in as after any scan.
        View::Graph => {
            stop_move(ui);
            SESSION.with_borrow_mut(|s| s.out_from = None);
        }
    }
    sync_view(ui);
    // The outline of the view that shows now: the old view's box is not on its picture.
    sync_selection(ui);
    relayout(ui);
}

/// Colours were chosen in the graph bar (0 age, 1 file type): the graph is drawn again in them.
/// The choice lasts for the session only.
fn colour_chosen(ui: &MainWindow, i: i32) {
    let mode = if i == 1 { ColourMode::FileType } else { ColourMode::Age };
    if with_loaded(|l| std::mem::replace(&mut l.graph.colour, mode) != mode) != Some(true) {
        return;
    }
    sync_view(ui);
    relayout(ui);
}

/// Wires the view switch, the colour toggle, the graph's size and the graph's own input: click,
/// double-click, menu (right-click, Menu key, Shift+F10), keys, expanders, hover and wheel.
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
    // Each handler runs with the window, when it still exists.
    let weak = ui.as_weak();
    let at = move |f: &dyn Fn(&MainWindow)| {
        if let Some(ui) = weak.upgrade() {
            f(&ui);
        }
    };
    ui.on_graph_click({
        let at = at.clone();
        move |x, y| at(&|ui| clicked(ui, x, y))
    });
    ui.on_graph_open({
        let at = at.clone();
        move |x, y| at(&|ui| opened(ui, x, y))
    });
    ui.on_graph_menu({
        let at = at.clone();
        move |x, y, wx, wy| at(&|ui| menu_with(ui, x, y, At::Pointer(wx, wy), map_menu))
    });
    ui.on_graph_menu_key({
        let at = at.clone();
        move |x, y, w, h| at(&|ui| menu_key_with(ui, At::Box(x, y, w, h), map_menu))
    });
    ui.on_graph_key({
        let at = at.clone();
        move |k| at(&|ui| key_pressed(ui, &k))
    });
    ui.on_graph_expand({
        let at = at.clone();
        move |i| at(&|ui| expand(ui, i))
    });
    ui.on_graph_hover({
        let at = at.clone();
        move |x, y| at(&|ui| hovered(ui, x, y))
    });
    ui.on_graph_leave({
        let at = at.clone();
        move || at(&left)
    });
    ui.on_graph_scroll(move |n| at(&|ui| scrolled(ui, n)));
}

/// Runs `f` on the shown scan and the graph it shows (`GraphState::shown`), only while the graph
/// view shows. No graph (none landed yet, or just dropped) or the treemap: `None`, and the caller
/// does nothing. Every action of the graph reads the graph through here, never a copy.
fn with_shown<R>(f: impl FnOnce(&Loaded, &Graph) -> R) -> Option<R> {
    SESSION.with_borrow(|s| {
        if graph_view::shown_view(s.scan.state(), s.loaded.is_some(), s.settings.view) != View::Graph {
            return None;
        }
        let l = s.loaded.as_ref()?;
        Some(f(l, l.graph.shown.as_ref()?))
    })
}

/// The band of graph `g` under graph point (x, y) in physical pixels (`flow::hit`).
fn band_at(g: &Graph, (x, y): (f64, f64)) -> Option<usize> {
    flow::hit(g, x, y)
}

/// A click at graph point (x, y), in logical pixels: a folder band selects its folder, a files band
/// the folder that holds the files (`graph_view::click`), an "other" band shows its members.
fn clicked(ui: &MainWindow, x: f32, y: f32) {
    tip_event(ui, tip::TipEvent::Click);
    // The pointer comes in logical pixels, the graph is in physical pixels (as `node_at`).
    let p = to_physical(ui, x, y);
    match with_shown(|_, g| graph_view::click(g, band_at(g, p))) {
        Some(Act::Select(n)) => select(ui, n),
        Some(Act::ShowOther(parent)) => {
            with_loaded(|l| l.graph.show_other(parent));
            relayout(ui);
        }
        Some(Act::Nothing) | None => {}
    }
}

/// A double-click at graph point (x, y): a folder band's folder becomes the view folder of both
/// views (`open_node`, which re-roots the graph through `show_folder`).
fn opened(ui: &MainWindow, x: f32, y: f32) {
    let p = to_physical(ui, x, y);
    if let Some(n) = with_shown(|_, g| graph_view::open_target(g, band_at(g, p))).flatten() {
        open_node(ui, n);
    }
}

/// The folder a right-click at graph point (x, y), in logical pixels, opens the menu for: only a
/// drawn folder band of the shown graph (`graph_view::menu_target`).
pub(crate) fn menu_hit(ui: &MainWindow, x: f32, y: f32) -> Option<NodeId> {
    let p = to_physical(ui, x, y);
    with_shown(|_, g| graph_view::menu_target(g, band_at(g, p))).flatten()
}

/// A right-click at graph point (x, y): `open` gets the folder band's folder and `at`. A files band,
/// an "other" band, empty space or no shown graph: nothing (no menu, also not for the selection).
pub(crate) fn menu_with(ui: &MainWindow, x: f32, y: f32, at: At, open: impl FnOnce(&MainWindow, NodeId, At)) {
    if let Some(n) = menu_hit(ui, x, y) {
        open(ui, n, at);
    }
}

/// The Menu key or Shift+F10 in the graph: `open` gets the selection only when it is drawn as its own
/// folder band (`graph_view::key_target`), and `at`.
pub(crate) fn menu_key_with(ui: &MainWindow, at: At, open: impl FnOnce(&MainWindow, NodeId, At)) {
    if let Some(n) = with_shown(|l, g| graph_view::key_target(g, l.selected)).flatten() {
        open(ui, n, at);
    }
}

/// The sink of both menu paths: the existing menu of the treemap's right-click, for folder `n`.
fn map_menu(ui: &MainWindow, n: NodeId, at: At) {
    open_menu(ui, Source::Map, Some(n), at);
}

/// A key in the graph (`graph_view::key`). Delete and Shift+Delete go to the delete flow of the
/// treemap for the selected folder band (`scan_apply::request_delete_of`), which refuses on its own
/// while a scan runs or a removal waits.
fn key_pressed(ui: &MainWindow, key: &str) {
    tip_event(ui, tip::TipEvent::Key);
    // Right and Left read the graph state: two presses before a landing do not undo each other.
    let Some(act) = with_shown(|l, g| graph_view::key_with(g, &l.data.tree, &l.graph.expanded, l.selected, key)) else { return };
    match act {
        KeyAct::Select(n) => select(ui, n),
        KeyAct::Toggle(n) => toggle(ui, n),
        KeyAct::Open(n) => open_node(ui, n),
        KeyAct::Up => go_up(ui),
        KeyAct::Delete { node, shift } => {
            scan_apply::request_delete_of(ui, Some(node), shift);
            delete_ui::sync(ui);
        }
        KeyAct::Nothing => {}
    }
}

/// The expander of band `i` of the shown graph was used.
fn expand(ui: &MainWindow, i: i32) {
    let folder = with_shown(|_, g| match g.bands.get(usize::try_from(i).ok()?)?.part {
        Part::Folder(n) => Some(n),
        _ => None,
    });
    if let Some(n) = folder.flatten() {
        toggle(ui, n);
    }
}

/// Expands or collapses folder `n`, drawn in the shown graph, and lays the graph out again. A folder
/// that opens scrolls its new column (the next one) into view; that column may be new, so the graph
/// has at least one more column than its band's.
fn toggle(ui: &MainWindow, n: NodeId) {
    let done = with_loaded(|l| {
        let g = &mut l.graph;
        let shown = g.shown.as_ref()?;
        let column = shown.bands.iter().find(|b| b.part == Part::Folder(n))?.column;
        let columns = shown.columns.max(column.saturating_add(2));
        g.toggle(n);
        if g.expanded.contains(&n) {
            g.scroll_to(column.saturating_add(1), columns);
        }
        Some(())
    });
    if done.flatten().is_some() {
        relayout(ui);
    }
}

/// The wheel moved the columns by `delta` (-1 left, 1 right): the graph is laid out again when the
/// first column changed. An expander may have scrolled to a column the shown graph does not have
/// yet (`toggle`): the graph has at least the columns up to the last one in view, so the wheel
/// never scrolls that column away before its layout lands.
fn scrolled(ui: &MainWindow, delta: i32) {
    tip_event(ui, tip::TipEvent::Scroll);
    let Some(shown) = with_shown(|_, g| g.columns) else { return };
    let moved = with_loaded(|l| {
        let before = l.graph.first_column;
        let columns = shown.max(before.saturating_add(VISIBLE_COLUMNS));
        l.graph.scroll_by(delta, columns);
        l.graph.first_column != before
    });
    if moved == Some(true) {
        relayout(ui);
    }
}

/// What the window shows for hovered band `b` of `data`, in logical pixels: a highlight of the band
/// and its tooltip (`graph_view::tooltip`).
fn hover_of(data: &Data, b: &flow::Band, scale: f64, unit: Unit) -> Hover {
    let (title, detail) = graph_view::tooltip(&data.tree, &data.counts, b, unit);
    let px = |v: f64| (v / scale) as f32;
    Hover {
        visible: true,
        x: px(b.x),
        y: px(b.y),
        width: px(b.w),
        height: px(b.h),
        title: title.into(),
        detail: detail.into(),
        ..Hover::default()
    }
}

/// The pointer moved over the graph to (x, y) in logical pixels: as the treemap's `hover`, the
/// window gets a new hover box only when the band part under the pointer changes.
fn hovered(ui: &MainWindow, x: f32, y: f32) {
    SESSION.with_borrow_mut(|s| s.pointer = (x, y));
    let p = to_physical(ui, x, y);
    let scale = f64::from(ui.window().scale_factor());
    let unit = current_unit();
    let found = with_shown(|l, g| {
        let b = g.bands.get(band_at(g, p)?)?;
        Some((b.part, hover_of(&l.data, b, scale, unit)))
    })
    .flatten();
    let part = found.as_ref().map(|(part, _)| *part);
    let changed = note_graph_hover(part);
    if changed {
        ui.set_hover(found.map(|(_, h)| h).unwrap_or_default());
    }
    tip_event(ui, tip::TipEvent::Moved { over: part.is_some(), changed });
}

/// The pointer left the graph: nothing is hovered and the tooltip gets `Leave`.
fn left(ui: &MainWindow) {
    forget_hover(ui);
    tip_event(ui, tip::TipEvent::Leave);
}

/// No band is hovered any more; the window's hover box goes when one was.
pub(crate) fn forget_hover(ui: &MainWindow) {
    if note_graph_hover(None) {
        ui.set_hover(Hover::default());
    }
}

/// While the graph shows: the boxes the selection outline is placed on, the shown graph's folder
/// bands (`flow::folder_rects`, physical pixels; none before a graph landed). `None` while the
/// treemap shows.
pub(crate) fn outline_rects(s: &Session) -> Option<Vec<treemap::Rect>> {
    if graph_view::shown_view(s.scan.state(), s.loaded.is_some(), s.settings.view) != View::Graph {
        return None;
    }
    let l = s.loaded.as_ref()?;
    Some(l.graph.shown.as_ref().map(flow::folder_rects).unwrap_or_default())
}

/// After a change of the selection (`select`, from any view): while the graph shows, its columns
/// scroll the least that puts the selection's band in view (`graph_view::drawn_column`), and the
/// graph is laid out again when they moved. Only for a selection change: a landing keeps the scroll
/// of the wheel or of an expander.
pub(crate) fn scroll_to_selection(ui: &MainWindow) {
    let moved = SESSION.with_borrow_mut(|s| {
        if graph_view::shown_view(s.scan.state(), s.loaded.is_some(), s.settings.view) != View::Graph {
            return false;
        }
        let Some(l) = s.loaded.as_mut() else { return false };
        let Some(columns) = l.graph.shown.as_ref().map(|g| g.columns) else { return false };
        let before = l.graph.first_column;
        if let Some(column) = l.selected.and_then(|n| graph_view::drawn_column(&l.data.tree, l.view_root, &l.graph.expanded, n)) {
            l.graph.scroll_to(column, columns);
        }
        l.graph.first_column != before
    });
    if moved {
        relayout(ui);
    }
}

/// Keeps `part` as the hovered band part. `true` when it is not the part hovered before: only then
/// does the window get a new hover box.
pub(crate) fn note_graph_hover(part: Option<Part>) -> bool {
    SESSION.with_borrow_mut(|s| std::mem::replace(&mut s.graph_hovered, part) != part)
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
/// A count that panics lands as `None`: the running flag still clears and the data is let go;
/// nothing is kept, and the bands keep the `Other` colour.
fn count_graph_types(ui: &MainWindow, generation: u64, data: Arc<Data>, edits: u64, parts: Vec<Part>) {
    TYPES_RUNNING.set(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let found = catch_unwind(AssertUnwindSafe(|| types_of(&data.tree, &parts))).ok();
        // Err means the event loop has stopped: nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            TYPES_RUNNING.set(false);
            let Some(ui) = weak.upgrade() else { return };
            if let Some(found) = found {
                store_types(generation, &data, edits, found);
            }
            // Let go of the data, so a delete waiting for it can apply.
            drop(data);
            let seq = SESSION.with_borrow(|s| s.layout_seq);
            scan_apply::data_released(&ui);
            // A removal the release applied took a new `layout_seq` and laid out again already.
            if SESSION.with_borrow(|s| s.layout_seq == seq && wants_types(s)) {
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

    // ---- the treemap's motion state while the graph shows ----

    fn child(name: &str) -> NodeId {
        SESSION.with_borrow(|s| {
            let tree = &s.loaded.as_ref().unwrap().data.tree;
            tree.children(tree.root()).find(|&n| tree.name(n) == name).unwrap()
        })
    }

    /// A treemap of `shown` as if it had landed at 400 x 300 before the graph showed: the rects a
    /// zoom is computed against.
    fn stale_treemap_of(shown: NodeId) {
        SESSION.with_borrow_mut(|s| {
            s.map_size = (400.0, 300.0);
            let l = s.loaded.as_mut().unwrap();
            l.rects = treemap::layout_framed(&l.data.tree, shown, 400, 300, treemap::Chrome::scaled(1.0));
            l.shown_root = Some(shown);
            l.view_root = shown;
        });
    }

    /// No treemap move shows or waits, and no zoom out waits for the next treemap.
    fn assert_no_treemap_motion(ui: &MainWindow, at: &str) {
        let (moving, out_from) = SESSION.with_borrow(|s| (s.moving.map(|m| m.kind), s.out_from));
        assert_eq!((moving, out_from, ui.get_zoom_shown()), (None, None, false), "{at}");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn show_folder_while_the_graph_shows_starts_no_treemap_zoom_with_animations_on() {
        show_folder_in_the_graph_with(Some(true));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn show_folder_while_the_graph_shows_starts_no_treemap_zoom_with_animations_off() {
        show_folder_in_the_graph_with(Some(false));
    }

    /// The graph shows over a stale treemap of the root, with the animation setting forced to
    /// `animation`; `show_folder(a)` changes the folder and leaves no treemap motion.
    fn show_folder_in_the_graph_with(animation: Option<bool>) {
        let _forced = crate::motion::set_test_animation(animation);
        let ui = window(View::Graph);
        let root = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.tree.root());
        stale_treemap_of(root);
        let a = child("a");
        // Precondition, so the test cannot pass by chance: without the graph check,
        // `move_to_folder` would start a zoom in here exactly when motion is on.
        let way = SESSION.with_borrow(|s| {
            let l = s.loaded.as_ref().unwrap();
            crate::tree_view::zoom_way(&l.data.tree, l.shown_root, &l.rects, a)
        });
        assert!(matches!(way, crate::tree_view::ZoomWay::In(_)), "{way:?}");
        assert_eq!(crate::motion_on(), animation == Some(true), "the forced setting is read");

        crate::show_folder(&ui, a);

        assert_eq!(SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().view_root), a);
        assert_no_treemap_motion(&ui, &format!("into a child folder, animation {animation:?}"));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn crumb_up_while_the_graph_shows_leaves_no_zoom_out_waiting() {
        let _forced = crate::motion::set_test_animation(Some(true));
        let ui = window(View::Graph);
        let root = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.tree.root());
        stale_treemap_of(child("a"));
        // Precondition, so the test cannot pass by chance: without the graph check, going up
        // would leave a zoom out waiting for the next treemap (motion on, the treemap shows `a`).
        let way = SESSION.with_borrow(|s| {
            let l = s.loaded.as_ref().unwrap();
            crate::tree_view::zoom_way(&l.data.tree, l.shown_root, &l.rects, root)
        });
        assert_eq!(way, crate::tree_view::ZoomWay::Out(child("a")));
        assert!(crate::motion_on(), "motion is forced on");

        ui.invoke_crumb_open(root.0 as i32);

        assert_eq!(SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().view_root), root);
        assert_no_treemap_motion(&ui, "up to the root");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn graph_chosen_stops_a_treemap_move_and_drops_a_waiting_zoom_out() {
        let ui = window(View::Treemap);
        crate::start_move(&ui, crate::motion::Kind::In, Image::default(), (0.0, 0.0, 400.0, 300.0), (0.0, 0.0, 800.0, 600.0), (1.0, 1.0));
        let a = child("a");
        SESSION.with_borrow_mut(|s| s.out_from = Some(a));

        click(&ui, "Graph");

        assert_eq!(chosen_view(), View::Graph);
        assert_no_treemap_motion(&ui, "graph chosen");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn treemap_chosen_after_folder_changes_in_the_graph_shows_no_zoom() {
        // The last treemap showed `a`; in the graph the view goes up to the root.
        let _forced = crate::motion::set_test_animation(Some(true));
        let ui = window(View::Graph);
        let root = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.tree.root());
        stale_treemap_of(child("a"));
        // Precondition, so the test cannot pass by chance: without the graph check, going up
        // would leave a zoom out waiting for the next treemap (motion on, the treemap shows `a`).
        let way = SESSION.with_borrow(|s| {
            let l = s.loaded.as_ref().unwrap();
            crate::tree_view::zoom_way(&l.data.tree, l.shown_root, &l.rects, root)
        });
        assert_eq!(way, crate::tree_view::ZoomWay::Out(child("a")));
        assert!(crate::motion_on(), "motion is forced on");
        ui.invoke_crumb_open(root.0 as i32);

        click(&ui, "Treemap");

        assert_eq!(chosen_view(), View::Treemap);
        assert_no_treemap_motion(&ui, "treemap chosen");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn view_chosen_while_scanning_changes_nothing_in_rust() {
        let ui = window(View::Treemap);
        SESSION.with_borrow_mut(|s| {
            let _ = s.scan.start().expect("no scan runs");
        });

        // Past the markup's guard: the Rust handler itself, and the callback.
        view_chosen(&ui, 1);
        ui.invoke_view_chosen(1);

        assert_eq!(chosen_view(), View::Treemap);
        assert!(!ui.get_graph_shown());
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn graph_chosen_in_the_settings_shows_after_the_first_scan() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        crate::wire_tree_and_treemap(&ui);
        wire(&ui);
        // As read from settings.json at startup, before any scan.
        SESSION.with_borrow_mut(|s| s.settings.view = View::Graph);
        crate::show_state(&ui);
        assert!(!ui.get_graph_shown(), "no scan: the graph does not show");
        SESSION.with_borrow_mut(|s| {
            let (generation, _) = s.scan.start().expect("no scan runs");
            assert!(s.scan.apply(generation, Event::Finished(summary())));
        });

        // As `post_done`: install, then the state.
        crate::install(&ui, data());
        crate::show_state(&ui);

        assert!(ui.get_graph_shown());
        assert!(graph_shows(&ui));
    }

    // ---- note_graph_hover (TP4.2) ----
    #[test]
    fn note_graph_hover_is_true_only_when_the_part_changes() {
        let (a, b) = (NodeId(1), NodeId(2));
        let parts = [
            Some(Part::Folder(a)),
            Some(Part::Folder(a)),
            Some(Part::Files(a)),
            Some(Part::Files(a)),
            Some(Part::Other { parent: a, folders: 2, files: false }),
            Some(Part::Other { parent: a, folders: 3, files: false }),
            Some(Part::Folder(b)),
            None,
            None,
            Some(Part::Folder(b)),
        ];
        let changed: Vec<bool> = parts.into_iter().map(note_graph_hover).collect();
        assert_eq!(changed, [true, false, true, false, true, true, true, true, false, true]);
        assert_eq!(SESSION.with_borrow(|s| s.graph_hovered), Some(Part::Folder(b)));
    }

    #[test]
    fn note_graph_hover_leave_with_nothing_hovered_is_no_change() {
        assert!(!note_graph_hover(None));
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

    // ---- TP4.3: the graph's input, wired (headless) ----

    use crate::DeleteStep;
    use flow::Band;
    use slint::LogicalPosition;
    use slint::platform::{Key, PointerEventButton, WindowEvent};
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A folder the test made on the disk, removed at the end.
    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `<temp>` { a { a1.mp4 400, s { t { u { v { w.bin 200 } } } } }, b { b1.zip 300 }, c.txt 100,
    /// d { d.bin 20 } } on the disk (the delete dialog checks it) and as a scan. `d` is under 3 % of
    /// the root: it is in the root's "other (1)". With a, s, t and u expanded the graph has six
    /// columns (root, a, s, t, u, v).
    fn disk_fixture() -> (TempDir, Arc<Data>) {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("volatus-graph-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        for (path, size) in [(r"a\a1.mp4", 400), (r"a\s\t\u\v\w.bin", 200), (r"b\b1.zip", 300), ("c.txt", 100), (r"d\d.bin", 20)] {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, vec![0u8; size]).unwrap();
        }
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
            e(0, 0, &root.to_string_lossy(), 0, true),
            e(1, 0, "a", 0, true),
            e(2, 1, "a1.mp4", 400, false),
            e(3, 1, "s", 0, true),
            e(4, 3, "t", 0, true),
            e(5, 4, "u", 0, true),
            e(6, 5, "v", 0, true),
            e(7, 6, "w.bin", 200, false),
            e(8, 0, "b", 0, true),
            e(9, 8, "b1.zip", 300, false),
            e(10, 0, "c.txt", 100, false),
            e(11, 0, "d", 0, true),
            e(12, 11, "d.bin", 20, false),
        ];
        let tree = rollup(0, entries).unwrap().tree;
        (TempDir(root), Arc::new(Data { counts: crate::tree_view::file_counts(&tree), tree }))
    }

    /// Delete engines that are never asked for: no test here confirms a delete.
    fn fake_deps() -> crate::delete_ui::Deps {
        crate::delete_ui::Deps {
            remover_for: Box::new(|_, _| -> Arc<dyn crate::delete::Remover> { unreachable!("no delete runs in these tests") }),
            spawn: Box::new(|_, _, _, _, _| -> std::io::Result<()> { unreachable!("no delete runs in these tests") }),
        }
    }

    /// A window on the testing backend (this thread only), wired as `main` wires it, showing the
    /// graph of a finished scan of `disk_fixture`, with the first graph landed (`land_now`).
    fn graph_window() -> (MainWindow, TempDir) {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        crate::wire_tree_and_treemap(&ui);
        wire(&ui);
        crate::wire_settings(&ui);
        crate::delete_ui::wire(&ui, fake_deps());
        let (dir, data) = disk_fixture();
        SESSION.with_borrow_mut(|s| {
            let (generation, _) = s.scan.start().expect("no scan runs");
            assert!(s.scan.apply(generation, Event::Finished(summary())));
            s.scan_root = dir.0.clone();
            s.settings.view = View::Graph;
        });
        crate::install(&ui, data);
        crate::show_state(&ui);
        ui.window().set_size(slint::LogicalSize::new(1000.0, 700.0));
        ui.show().unwrap();
        land_now(&ui);
        (ui, dir)
    }

    /// The buttons labelled `label` inside the graph (the tree rows have "Expand <name>" too).
    fn graph_buttons(ui: &MainWindow, label: &str) -> Vec<ElementHandle> {
        let g = graph_element(ui);
        let (p, s) = (g.absolute_position(), g.size());
        buttons(ui, label)
            .into_iter()
            .filter(|e| {
                let q = e.absolute_position();
                q.x >= p.x && q.x < p.x + s.width && q.y >= p.y && q.y < p.y + s.height
            })
            .collect()
    }

    fn graph_button(ui: &MainWindow, label: &str) -> ElementHandle {
        graph_buttons(ui, label).into_iter().next().unwrap_or_else(|| panic!("a {label:?} button in the graph"))
    }

    /// The graph's focus scope, which has the graph view's place and size.
    fn graph_element(ui: &MainWindow) -> ElementHandle {
        ElementHandle::find_by_accessible_label(ui, "Folder graph").next().expect("the folder graph shows")
    }

    /// Lays out the graph of the session now, on this thread, at the size of the graph view, and
    /// lands it as `spawn_graph` does.
    fn land_now(ui: &MainWindow) {
        let size = graph_element(ui).size();
        assert!(size.width > 0.0 && size.height > 0.0, "the graph view has an area: {size:?}");
        let job = SESSION.with_borrow_mut(begin_layout).expect("a scan shows");
        assert_eq!(job.view, View::Graph);
        let scale = f64::from(ui.window().scale_factor());
        let (w, h) = ((f64::from(size.width) * scale).round() as u32, (f64::from(size.height) * scale).round() as u32);
        let landing = build(&job.data.tree, job.root, &graph_inputs(), w, h, scale, Theme::Light, 0, Unit::Dynamic);
        land(ui, Ready::of(landing), job.seq, Instant::now());
    }

    /// The node named `name` (names are unique in the fixture; the root has the folder's path).
    fn node(name: &str) -> NodeId {
        SESSION.with_borrow(|s| {
            let tree = &s.loaded.as_ref().unwrap().data.tree;
            let mut stack = vec![tree.root()];
            while let Some(n) = stack.pop() {
                if tree.name(n) == name {
                    return n;
                }
                stack.extend(tree.children(n));
            }
            panic!("no node {name}")
        })
    }

    fn root() -> NodeId {
        SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.tree.root())
    }

    fn selected() -> Option<NodeId> {
        SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().selected)
    }

    fn view_root() -> NodeId {
        SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().view_root)
    }

    fn graph_state<R>(f: impl FnOnce(&graph_view::GraphState) -> R) -> R {
        SESSION.with_borrow(|s| f(&s.loaded.as_ref().unwrap().graph))
    }

    fn set_graph_state(f: impl FnOnce(&mut graph_view::GraphState)) {
        crate::with_loaded(|l| f(&mut l.graph)).unwrap();
    }

    fn shown_graph() -> Graph {
        graph_state(|g| g.shown.clone()).expect("a graph landed")
    }

    fn band(part: Part) -> Band {
        shown_graph().bands.into_iter().find(|b| b.part == part).unwrap_or_else(|| panic!("no band {part:?}"))
    }

    fn scale(ui: &MainWindow) -> f64 {
        f64::from(ui.window().scale_factor())
    }

    /// The centre of band `b`, in logical pixels of the graph.
    fn centre(ui: &MainWindow, b: &Band) -> (f32, f32) {
        let s = scale(ui);
        (((b.x + b.w / 2.0) / s) as f32, ((b.y + b.h / 2.0) / s) as f32)
    }

    /// The box of band `b` in logical pixels of the graph: (x, y, width, height).
    fn logical_box(ui: &MainWindow, b: &Band) -> (f32, f32, f32, f32) {
        let s = scale(ui);
        ((b.x / s) as f32, (b.y / s) as f32, (b.w / s) as f32, (b.h / s) as f32)
    }

    /// The selection outline the window shows, in logical pixels, or `None`.
    fn outline(ui: &MainWindow) -> Option<(f32, f32, f32, f32)> {
        ui.get_has_selection().then(|| (ui.get_selection_x(), ui.get_selection_y(), ui.get_selection_width(), ui.get_selection_height()))
    }

    /// Graph point (x, y) as a window point.
    fn in_window(ui: &MainWindow, (x, y): (f32, f32)) -> LogicalPosition {
        let p = graph_element(ui).absolute_position();
        LogicalPosition::new(p.x + x, p.y + y)
    }

    /// A press and release of `button` at a window point, through the testing backend.
    fn press_at(ui: &MainWindow, position: LogicalPosition, button: PointerEventButton) {
        ui.window().dispatch_event(WindowEvent::PointerMoved { position });
        ui.window().dispatch_event(WindowEvent::PointerPressed { position, button });
        ui.window().dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    /// A key press and release, to the element with the focus, through the testing backend.
    fn press(ui: &MainWindow, k: Key) {
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: k.into() });
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: k.into() });
    }

    /// The keys that reach the graph from now on (`graph-key`), in order, in place of their handler.
    fn record_graph_keys(ui: &MainWindow) -> Rc<RefCell<Vec<String>>> {
        let keys = Rc::new(RefCell::new(Vec::new()));
        let log = keys.clone();
        ui.on_graph_key(move |k| log.borrow_mut().push(k.to_string()));
        keys
    }

    /// One turn of the event loop's own work: the change handlers of the markup run.
    fn run_change_handlers() {
        slint::platform::update_timers_and_animations();
    }

    /// The tree row of `n`, which must be shown.
    fn row_of(n: NodeId) -> i32 {
        SESSION.with_borrow(|s| {
            let l = s.loaded.as_ref().unwrap();
            l.model.rows.borrow().iter().position(|r| r.node == n).map(|i| i as i32).expect("the row shows")
        })
    }

    fn row_node(i: i32) -> Option<NodeId> {
        crate::row_node(i)
    }

    /// A menu sink in place of `map_menu`.
    type MenuSink = Box<dyn FnOnce(&MainWindow, NodeId, At)>;

    /// The menus `menu_with` or `menu_key_with` asked for, through a sink that records them.
    fn record_menus() -> (Rc<RefCell<Vec<NodeId>>>, impl Fn() -> MenuSink) {
        let opened = Rc::new(RefCell::new(Vec::new()));
        let log = opened.clone();
        let sink = move || -> MenuSink {
            let log = log.clone();
            Box::new(move |_, n, _| log.borrow_mut().push(n))
        };
        (opened, sink)
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn click_at_a_folder_band_centre_selects_it_and_the_tree_follows() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        let b = band(Part::Folder(a));

        press_at(&ui, in_window(&ui, centre(&ui, &b)), PointerEventButton::Left);

        assert_eq!(selected(), Some(a));
        assert_eq!(row_node(ui.get_selected_row()), Some(a), "the tree's selected row is a");
        assert_eq!(outline(&ui), Some(logical_box(&ui, &b)), "the outline is on a's band");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_tree_selection_moves_the_outline_to_its_band_or_its_nearest_drawn_folder() {
        let (ui, _dir) = graph_window();
        let b = node("b");
        ui.invoke_tree_select(row_of(b));
        assert_eq!(outline(&ui), Some(logical_box(&ui, &band(Part::Folder(b)))), "b's own band");

        // A file deep under the collapsed a: the outline is on a's band.
        crate::select(&ui, node("w.bin"));
        assert_eq!(outline(&ui), Some(logical_box(&ui, &band(Part::Folder(node("a"))))), "a file under a");

        // d is merged into "other": the nearest drawn folder is the root.
        crate::select(&ui, node("d"));
        assert_eq!(outline(&ui), Some(logical_box(&ui, &band(Part::Folder(root())))), "a merged folder");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn the_expander_labelled_expand_a_opens_a_then_it_is_labelled_collapse_a() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        assert!(graph_buttons(&ui, "Collapse a").is_empty());

        graph_button(&ui, "Expand a").invoke_accessible_default_action();

        assert!(graph_state(|g| g.expanded.contains(&a)), "a is expanded");
        land_now(&ui);
        assert!(!graph_buttons(&ui, "Collapse a").is_empty(), "a \"Collapse a\" button");
        assert!(graph_buttons(&ui, "Expand a").is_empty());
        assert!(shown_graph().bands.iter().any(|b| b.part == Part::Folder(node("s"))), "a's subfolder shows");

        graph_button(&ui, "Collapse a").invoke_accessible_default_action();

        assert!(!graph_state(|g| g.expanded.contains(&a)), "a is collapsed");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn an_expander_scrolls_the_new_column_into_view() {
        let (ui, _dir) = graph_window();
        let chain = [node("a"), node("s"), node("t")];
        set_graph_state(|g| g.expanded.extend(chain));
        land_now(&ui);
        assert_eq!((shown_graph().columns, shown_graph().first_column), (5, 0), "precondition");
        // Columns 1 to 4 show: u, in column 4, and its expander are drawn.
        set_graph_state(|g| g.first_column = 1);
        land_now(&ui);

        // u is in column 4: its subfolder v opens column 5.
        graph_button(&ui, "Expand u").invoke_accessible_default_action();

        assert_eq!(graph_state(|g| g.first_column), 2, "columns 2 to 5 show");
        land_now(&ui);
        assert_eq!((shown_graph().columns, shown_graph().first_column), (6, 2));
        // The landing keeps the scroll (the selection, the root, is out of view now).
        assert_eq!(graph_state(|g| g.first_column), 2, "kept after the landing");
        assert!(shown_graph().bands.iter().any(|b| b.part == Part::Folder(node("v"))), "v shows");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn the_wheel_scrolls_by_whole_columns_within_the_graph() {
        let (ui, _dir) = graph_window();
        let chain = [node("a"), node("s"), node("t"), node("u")];
        set_graph_state(|g| g.expanded.extend(chain));
        land_now(&ui);
        assert_eq!(shown_graph().columns, 6, "precondition");

        let mut got = Vec::new();
        for delta in [1, 1, 1, -1, -1, -1] {
            ui.invoke_graph_scroll(delta);
            got.push(graph_state(|g| g.first_column));
        }

        assert_eq!(got, [1, 2, 2, 1, 0, 0]);
        // A landing keeps the scroll of the wheel.
        ui.invoke_graph_scroll(1);
        land_now(&ui);
        assert_eq!((graph_state(|g| g.first_column), shown_graph().first_column), (1, 1), "kept after the landing");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_selection_scrolls_its_folder_band_into_view() {
        let (ui, _dir) = graph_window();
        let chain = [node("a"), node("s"), node("t"), node("u")];
        set_graph_state(|g| g.expanded.extend(chain));
        land_now(&ui);
        assert_eq!((shown_graph().columns, shown_graph().first_column), (6, 0), "precondition");

        // v is in column 5, out of view: the graph scrolls to show it.
        crate::select(&ui, node("v"));
        assert_eq!(graph_state(|g| g.first_column), 2, "v's column comes into view");
        land_now(&ui);
        assert_eq!(outline(&ui), Some(logical_box(&ui, &band(Part::Folder(node("v"))))), "the outline is on v");

        // a file in v: the outline goes to v, which shows.
        crate::select(&ui, node("w.bin"));
        assert_eq!(graph_state(|g| g.first_column), 2);

        // a is in column 1, out of view now: back to it.
        crate::select(&ui, node("a"));
        assert_eq!(graph_state(|g| g.first_column), 1, "a's column comes into view");
        land_now(&ui);
        crate::select(&ui, root());
        assert_eq!(graph_state(|g| g.first_column), 0, "the view band's column");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn delete_with_a_drawn_folder_opens_its_confirm_and_with_a_file_selected_stays_closed() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        crate::select(&ui, a);

        ui.invoke_graph_key("delete".into());

        let d = ui.get_delete();
        assert_eq!((d.step, d.name.as_str(), d.permanent), (DeleteStep::Confirm, "a", false));
        ui.invoke_close_delete();
        ui.invoke_graph_key("shift-delete".into());
        let d = ui.get_delete();
        assert_eq!((d.step, d.name.as_str(), d.permanent), (DeleteStep::Confirm, "a", true));
        ui.invoke_close_delete();
        assert_eq!(ui.get_delete().step, DeleteStep::Closed);

        // A file, selected from the tree, has no band: Delete does nothing in the graph.
        ui.invoke_tree_select(row_of(node("c.txt")));
        assert_eq!(selected(), Some(node("c.txt")));
        ui.invoke_graph_key("delete".into());
        ui.invoke_graph_key("shift-delete".into());
        assert_eq!(ui.get_delete().step, DeleteStep::Closed);
    }

    /// A known surprise (PR notes): a click on a "files" band selects its folder, so Delete then
    /// deletes the whole folder. The outline is therefore on the folder's own band.
    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_click_on_a_files_band_outlines_its_folder_band_and_delete_names_the_folder() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        crate::open_node(&ui, a);
        land_now(&ui);
        let files = band(Part::Files(a));

        press_at(&ui, in_window(&ui, centre(&ui, &files)), PointerEventButton::Left);

        assert_eq!(selected(), Some(a));
        let folder = logical_box(&ui, &band(Part::Folder(a)));
        assert_eq!(outline(&ui), Some(folder), "the outline is on a's own band");
        assert_ne!(outline(&ui), Some(logical_box(&ui, &files)), "not on the files band");
        // The click gave the graph the focus: Delete goes to it.
        press(&ui, Key::Delete);
        let d = ui.get_delete();
        assert_eq!((d.step, d.name.as_str()), (DeleteStep::Confirm, "a"), "the dialog names the folder, not \"files\"");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn graph_and_treemap_menu_requests_are_equal() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        set_graph_state(|g| {
            g.expanded.insert(a);
        });
        land_now(&ui);
        let disk = |_: &std::path::Path| crate::menu::Disk::Dir;
        let folders: Vec<(NodeId, Band)> =
            shown_graph().bands.into_iter().filter_map(|b| if let Part::Folder(n) = b.part { Some((n, b)) } else { None }).collect();
        assert!(folders.len() >= 4, "{folders:?}");

        for (n, b) in folders {
            let (x, y) = centre(&ui, &b);
            let graph = crate::menu_request(Source::Map, menu_hit(&ui, x, y), disk);
            // The treemap's right-click asks for the tile under the pointer: `Some(n)`.
            let treemap = crate::menu_request(Source::Map, Some(n), disk);
            assert!(treemap.is_some());
            assert_eq!(graph, treemap, "band of {n:?}");
        }
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_right_click_on_a_files_band_an_other_band_or_empty_space_opens_no_menu() {
        let (ui, _dir) = graph_window();
        let r = root();
        let a = node("a");
        let (opened, sink) = record_menus();
        let view = band(Part::Folder(r));
        // Right of the view band, in the flows: no band there.
        let s = scale(&ui);
        let empty = (((view.x + view.w + 30.0) / s) as f32, ((view.y + view.h / 2.0) / s) as f32);
        assert_eq!(flow::hit(&shown_graph(), f64::from(empty.0) * s, f64::from(empty.1) * s), None, "precondition: empty space");
        let other = shown_graph().bands.into_iter().find(|b| matches!(b.part, Part::Other { .. })).expect("an other band");
        let before = selected();

        for (what, point) in [("files", centre(&ui, &band(Part::Files(r)))), ("other", centre(&ui, &other)), ("empty", empty)] {
            assert_eq!(menu_hit(&ui, point.0, point.1), None, "{what}");
            menu_with(&ui, point.0, point.1, At::Pointer(0.0, 0.0), sink());
        }
        assert_eq!(*opened.borrow(), Vec::<NodeId>::new(), "no menu");
        assert_eq!(selected(), before, "the selection stays");

        // A folder band: the menu of that folder.
        let (x, y) = centre(&ui, &band(Part::Folder(a)));
        menu_with(&ui, x, y, At::Pointer(0.0, 0.0), sink());
        assert_eq!(*opened.borrow(), vec![a]);

        // The markup gives a right-click on empty space to `graph-menu` at that graph point.
        let asked = Rc::new(RefCell::new(Vec::new()));
        let log = asked.clone();
        ui.on_graph_menu(move |x, y, _, _| log.borrow_mut().push((x, y)));
        press_at(&ui, in_window(&ui, empty), PointerEventButton::Right);
        assert_eq!(*asked.borrow(), vec![empty]);
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn the_menu_key_opens_the_menu_only_for_a_selection_drawn_as_its_own_folder_band() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        let (opened, sink) = record_menus();

        crate::select(&ui, a);
        menu_key_with(&ui, At::Box(0.0, 0.0, 1.0, 1.0), sink());
        assert_eq!(*opened.borrow(), vec![a], "a drawn folder");

        for name in ["w.bin", "d", "c.txt"] {
            crate::select(&ui, node(name));
            menu_key_with(&ui, At::Box(0.0, 0.0, 1.0, 1.0), sink());
        }
        assert_eq!(*opened.borrow(), vec![a], "a file, a merged folder: no menu");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn enter_on_a_folder_band_makes_it_the_view_folder_for_both_views_and_empties_the_expanded_set() {
        let (ui, _dir) = graph_window();
        let (a, s) = (node("a"), node("s"));
        let r = root();
        set_graph_state(|g| {
            g.expanded.insert(a);
            g.open_other.insert(r);
        });
        land_now(&ui);
        crate::select(&ui, s);

        ui.invoke_graph_key("enter".into());

        assert_eq!(view_root(), s);
        assert!(graph_state(|g| g.expanded.is_empty() && g.open_other.is_empty() && g.first_column == 0), "nothing expanded");
        // The treemap's next layout is of the same folder.
        click(&ui, "Treemap");
        assert_eq!(view_root(), s);
        assert_eq!(SESSION.with_borrow_mut(begin_layout).map(|j| (j.root, j.view)), Some((s, View::Treemap)));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_double_click_on_a_folder_band_opens_it_and_on_a_files_band_does_nothing() {
        let (ui, _dir) = graph_window();
        let (r, b) = (root(), node("b"));
        let (x, y) = centre(&ui, &band(Part::Files(r)));
        ui.invoke_graph_open(x, y);
        assert_eq!(view_root(), r);

        let (x, y) = centre(&ui, &band(Part::Folder(b)));
        ui.invoke_graph_open(x, y);

        assert_eq!(view_root(), b);
        assert_eq!(selected(), Some(b));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn backspace_goes_up_and_selects_the_folder_it_left() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        crate::open_node(&ui, a);
        land_now(&ui);

        ui.invoke_graph_key("backspace".into());

        assert_eq!((view_root(), selected()), (root(), Some(a)));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn arrows_move_the_selection_between_folder_bands_and_right_expands() {
        let (ui, _dir) = graph_window();
        let (a, b) = (node("a"), node("b"));
        crate::select(&ui, a);

        ui.invoke_graph_key("down".into());
        assert_eq!(selected(), Some(b));
        ui.invoke_graph_key("up".into());
        assert_eq!(selected(), Some(a));
        ui.invoke_graph_key("right".into());
        assert!(graph_state(|g| g.expanded.contains(&a)), "Right expands a");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_click_on_the_other_band_shows_its_members() {
        let (ui, _dir) = graph_window();
        let other = shown_graph().bands.into_iter().find(|b| matches!(b.part, Part::Other { .. })).expect("an other band");
        let before = selected();
        let (x, y) = centre(&ui, &other);

        ui.invoke_graph_click(x, y);

        assert!(graph_state(|g| g.open_other.contains(&root())), "the root's other band is open");
        assert_eq!(selected(), before);
        land_now(&ui);
        assert!(shown_graph().bands.iter().any(|b| b.part == Part::Folder(node("d"))), "d shows as its own band");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn with_no_shown_graph_every_action_does_nothing() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        let (x, y) = centre(&ui, &band(Part::Folder(a)));
        let expander = ui.get_graph_expanders().iter().next().expect("an expander").band;
        crate::select(&ui, a);
        clear_picture(&ui);
        let (opened, sink) = record_menus();

        ui.invoke_graph_click(x, y);
        ui.invoke_graph_open(x, y);
        ui.invoke_graph_expand(expander);
        ui.invoke_graph_key("delete".into());
        ui.invoke_graph_key("down".into());
        ui.invoke_graph_scroll(1);
        menu_with(&ui, x, y, At::Pointer(0.0, 0.0), sink());
        menu_key_with(&ui, At::Box(0.0, 0.0, 1.0, 1.0), sink());

        assert_eq!((selected(), view_root()), (Some(a), root()));
        assert_eq!(ui.get_delete().step, DeleteStep::Closed);
        assert!(graph_state(|g| g.expanded.is_empty() && g.first_column == 0));
        assert!(opened.borrow().is_empty());
    }

    fn hovered() -> Option<Part> {
        SESSION.with_borrow(|s| s.graph_hovered)
    }

    /// Hovers the centre of a's band; checks that it shows.
    fn hover_a(ui: &MainWindow) {
        let a = node("a");
        let (x, y) = centre(ui, &band(Part::Folder(a)));
        ui.invoke_graph_hover(x, y);
        assert_eq!(hovered(), Some(Part::Folder(a)), "a is hovered");
        assert!(ui.get_hover().visible);
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn graph_hover_shows_the_band_and_its_tooltip_and_leave_clears_them() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        let b = band(Part::Folder(a));

        hover_a(&ui);

        let h: Hover = ui.get_hover();
        let want = SESSION.with_borrow(|s| {
            let d = &s.loaded.as_ref().unwrap().data;
            graph_view::tooltip(&d.tree, &d.counts, &b, Unit::Dynamic)
        });
        assert_eq!((h.title.to_string(), h.detail.to_string()), want);
        assert_eq!((h.x, h.y, h.width, h.height), logical_box(&ui, &b));

        ui.invoke_graph_leave();

        assert_eq!(hovered(), None);
        assert!(!ui.get_hover().visible);
        assert!(!ui.get_tip_visible());
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn graph_hover_is_reset_by_a_landing_a_menu_a_view_switch_and_a_new_scan() {
        let (ui, _dir) = graph_window();
        hover_a(&ui);
        land_now(&ui);
        assert_eq!((hovered(), ui.get_hover().visible), (None, false), "a landing");

        hover_a(&ui);
        // What `open_menu` does before the menu shows.
        crate::drop_hover(&ui);
        assert_eq!((hovered(), ui.get_hover().visible), (None, false), "a menu");

        hover_a(&ui);
        click(&ui, "Treemap");
        assert_eq!((hovered(), ui.get_hover().visible), (None, false), "a view switch");
        click(&ui, "Graph");
        land_now(&ui);

        hover_a(&ui);
        crate::install(&ui, disk_fixture().1);
        assert_eq!((hovered(), ui.get_hover().visible), (None, false), "a new scan");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_landing_gives_the_accessible_summary_of_the_view_folder_and_the_crumbs() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        crate::open_node(&ui, a);
        land_now(&ui);

        let want = SESSION.with_borrow(|s| {
            let d = &s.loaded.as_ref().unwrap().data;
            graph_view::summary(&d.tree, &shown_graph(), a, Unit::Dynamic)
        });
        assert_eq!(ui.get_graph_value().as_str(), want);
        assert_eq!(graph_element(&ui).accessible_value().map(|v| v.to_string()), Some(want));
        let crumbs: Vec<String> = ui.get_crumbs().iter().map(|c| c.text.to_string()).collect();
        assert_eq!(crumbs.last().map(String::as_str), Some("a"), "{crumbs:?}");
    }

    // ---- the keyboard focus goes to the graph, not to the hidden treemap ----

    /// The first element with `role` whose label starts with `start`.
    fn element_starting(ui: &MainWindow, role: AccessibleRole, start: &str) -> ElementHandle {
        i_slint_backend_testing::ElementQuery::from_root(ui)
            .match_descendants()
            .match_accessible_role(role)
            .find_all()
            .into_iter()
            .find(|e| e.accessible_label().is_some_and(|l| l.starts_with(start)))
            .unwrap_or_else(|| panic!("an element {start:?}"))
    }

    /// The crumbs of the view folder, as a treemap landing sets them.
    fn set_crumbs(ui: &MainWindow) {
        let crumbs = SESSION.with_borrow(|s| {
            let l = s.loaded.as_ref().unwrap();
            crate::tree_view::crumbs(&l.data.tree, l.view_root).into_iter().map(crate::crumb).collect::<Vec<_>>()
        });
        ui.set_crumbs(ModelRc::new(VecModel::from(crumbs)));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_crumb_opened_gives_the_focus_to_the_graph() {
        let (ui, _dir) = graph_window();
        crate::open_node(&ui, node("a"));
        set_crumbs(&ui);
        let keys = record_graph_keys(&ui);
        let root_crumb = element_starting(&ui, AccessibleRole::Button, "Open ");

        root_crumb.invoke_accessible_default_action();
        run_change_handlers();
        press(&ui, Key::DownArrow);

        assert_eq!(view_root(), root());
        assert_eq!(*keys.borrow(), vec!["down".to_string()], "the graph has the focus");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn closing_a_delete_dialog_asked_for_in_the_graph_gives_the_focus_back_to_the_graph() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        // A click on a's band selects it and gives the graph the focus; Delete opens the dialog.
        press_at(&ui, in_window(&ui, centre(&ui, &band(Part::Folder(a)))), PointerEventButton::Left);
        press(&ui, Key::Delete);
        assert_eq!(ui.get_delete().step, DeleteStep::Confirm, "precondition: the dialog shows");
        let keys = record_graph_keys(&ui);

        press(&ui, Key::Escape);
        run_change_handlers();
        assert_eq!(ui.get_delete().step, DeleteStep::Closed);
        press(&ui, Key::DownArrow);

        assert_eq!(*keys.borrow(), vec!["down".to_string()], "the graph has the focus");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn hiding_the_tree_while_it_has_the_focus_gives_the_focus_to_the_graph() {
        let (ui, _dir) = graph_window();
        let row = element_starting(&ui, AccessibleRole::ListItem, "b,");
        let (p, s) = (row.absolute_position(), row.size());
        press_at(&ui, LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0), PointerEventButton::Left);
        assert!(ui.get_tree_has_focus(), "precondition: the tree has the focus");
        let keys = record_graph_keys(&ui);

        // Folder tree: part 1.
        ui.invoke_part_toggled(1, false);
        run_change_handlers();
        press(&ui, Key::DownArrow);

        assert_eq!(*keys.borrow(), vec!["down".to_string()], "the graph has the focus");
    }

    // ---- TP4.4: the graph after a delete, a rescan and a new scan (headless) ----

    fn in_tree(n: NodeId) -> bool {
        SESSION.with_borrow(|s| {
            let tree = &s.loaded.as_ref().unwrap().data.tree;
            scan_apply::in_subtree(tree, n, tree.root())
        })
    }

    /// Folder `name` leaves the disk; a Delete on it finds it gone, so it leaves the tree by the
    /// pending removal (`scan_apply::step_pending`, `refresh_after_remove`). The workers the window
    /// started let go of the data on their own; until then each release is tried again, as
    /// `data_released` does in the app. The dialog closes.
    fn remove_gone(ui: &MainWindow, dir: &TempDir, name: &str) {
        let n = node(name);
        std::fs::remove_dir_all(dir.0.join(name)).unwrap();
        scan_apply::request_delete_of(ui, Some(n), false);
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while in_tree(n) {
            assert!(Instant::now() < deadline, "the removal of {name} never applied");
            std::thread::sleep(std::time::Duration::from_millis(5));
            scan_apply::data_released(ui);
        }
        ui.invoke_close_delete();
        assert_eq!(ui.get_delete().step, DeleteStep::Closed);
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_click_on_a_removed_band_does_nothing() {
        let (ui, dir) = graph_window();
        let (a, s, b) = (node("a"), node("s"), node("b"));
        set_graph_state(|g| {
            g.expanded.insert(a);
        });
        land_now(&ui);
        let (a_band, s_band) = (band(Part::Folder(a)), band(Part::Folder(s)));
        crate::select(&ui, b);

        remove_gone(&ui, &dir, "a");

        // Until the next landing nothing is hit-tested on the graph of the old tree.
        assert!(graph_state(|g| g.shown.is_none()), "no graph of the old tree is kept");
        assert!(graph_state(|g| !g.expanded.contains(&a)));
        let before = (selected(), view_root());
        // Delete right after the removal: b is drawn in the old graph, but nothing opens.
        ui.invoke_graph_key("delete".into());
        assert_eq!(ui.get_delete().step, DeleteStep::Closed, "no dialog from the old graph");
        for band in [a_band, s_band] {
            let (x, y) = centre(&ui, &band);
            ui.invoke_graph_click(x, y);
            ui.invoke_graph_open(x, y);
            press_at(&ui, in_window(&ui, (x, y)), PointerEventButton::Left);
            ui.invoke_graph_key("right".into());
        }
        assert_eq!((selected(), view_root()), before, "the removed bands do nothing");
        assert!(graph_state(|g| g.expanded.is_empty() && g.open_other.is_empty()));
        assert_eq!(ui.get_delete().step, DeleteStep::Closed);

        // The next landing is of the new tree: a is not in it.
        land_now(&ui);
        assert!(!shown_graph().bands.iter().any(|band| matches!(band.part, Part::Folder(n) if n == a || n == s)));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn install_after_expand_starts_with_an_empty_expanded_set() {
        let (ui, _dir) = graph_window();
        let (a, s) = (node("a"), node("s"));
        let r = root();
        set_graph_state(|g| {
            g.expanded.extend([a, s]);
            g.open_other.insert(r);
            g.first_column = 1;
            g.types.insert(Part::Files(r), Some(Category::Video));
        });
        land_now(&ui);
        assert!(graph_state(|g| g.shown.is_some()), "precondition");
        let empty = || graph_state(|g| (g.expanded.is_empty(), g.open_other.is_empty(), g.first_column, g.shown.is_none(), g.types.is_empty()));

        // A new scan over the shown one.
        crate::install(&ui, disk_fixture().1);
        assert_eq!(empty(), (true, true, 0, true, true), "a new scan");

        // A rescan: `start_scan` clears the window, then the finished scan installs.
        let r = root();
        set_graph_state(|g| {
            g.expanded.insert(r);
            g.first_column = 2;
        });
        land_now(&ui);
        crate::clear(&ui);
        crate::install(&ui, disk_fixture().1);
        assert_eq!(empty(), (true, true, 0, true, true), "a rescan");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn the_colour_mode_survives_a_rescan_and_a_new_scan() {
        let (ui, _dir) = graph_window();
        click(&ui, "File type");
        assert_eq!(colour(), ColourMode::FileType);

        // A new scan installed over the shown one.
        crate::install(&ui, disk_fixture().1);
        assert_eq!(colour(), ColourMode::FileType, "a new scan");

        // A rescan or "New scan": the window is cleared, then the finished scan installs.
        crate::clear(&ui);
        crate::install(&ui, disk_fixture().1);
        crate::show_state(&ui);
        assert_eq!(colour(), ColourMode::FileType, "a rescan");
        assert_eq!(button(&ui, "File type").accessible_checked(), Some(true));

        // Only the colour is kept.
        assert!(graph_state(|g| g.expanded.is_empty() && g.shown.is_none()));
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn two_rights_before_a_landing_leave_the_folder_expanded_and_two_lefts_collapsed() {
        let (ui, _dir) = graph_window();
        let a = node("a");
        crate::select(&ui, a);

        ui.invoke_graph_key("right".into());
        ui.invoke_graph_key("right".into());

        assert!(graph_state(|g| g.expanded.contains(&a)), "a second Right does not undo the first");
        land_now(&ui);
        assert_eq!(band(Part::Folder(a)).expander, Expander::Expanded);

        ui.invoke_graph_key("left".into());
        ui.invoke_graph_key("left".into());

        assert!(graph_state(|g| !g.expanded.contains(&a)), "a second Left does not undo the first");
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_wheel_turn_after_an_expander_and_before_the_landing_keeps_the_new_column() {
        let (ui, _dir) = graph_window();
        let chain = [node("a"), node("s"), node("t")];
        set_graph_state(|g| g.expanded.extend(chain));
        land_now(&ui);
        set_graph_state(|g| g.first_column = 1);
        land_now(&ui);
        assert_eq!((shown_graph().columns, shown_graph().first_column), (5, 1), "precondition");
        graph_button(&ui, "Expand u").invoke_accessible_default_action();
        assert_eq!(graph_state(|g| g.first_column), 2, "the expander scrolled to the new column");

        // The shown graph still has 5 columns: the wheel must not scroll the new column away.
        ui.invoke_graph_scroll(1);
        assert_eq!(graph_state(|g| g.first_column), 2, "at the last full view already");
        ui.invoke_graph_scroll(-1);
        assert_eq!(graph_state(|g| g.first_column), 1);
        // Back to the new column once its layout landed (before it, the shown graph's 5 columns
        // are all that is known after a scroll left).
        land_now(&ui);
        ui.invoke_graph_scroll(1);
        assert_eq!(graph_state(|g| g.first_column), 2);
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn a_click_at_a_scale_of_2_and_of_1_25_selects_the_band_under_the_pointer() {
        let (ui, _dir) = graph_window();
        for (factor, name) in [(2.0, "b"), (1.25, "a"), (2.0, "a")] {
            ui.window().dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: factor });
            assert_eq!(ui.window().scale_factor(), factor, "the testing backend takes the scale");
            land_now(&ui);
            crate::select(&ui, root());
            let n = node(name);
            let b = band(Part::Folder(n));

            press_at(&ui, in_window(&ui, centre(&ui, &b)), PointerEventButton::Left);

            assert_eq!(selected(), Some(n), "{name} at scale {factor}");
            assert_eq!(outline(&ui), Some(logical_box(&ui, &b)), "the outline at scale {factor}");
        }
    }

    /// The keys that reach the hidden treemap (`map-key`), in place of its handler.
    fn record_map_keys(ui: &MainWindow) -> Rc<RefCell<Vec<String>>> {
        let keys = Rc::new(RefCell::new(Vec::new()));
        let log = keys.clone();
        ui.on_map_key(move |k| log.borrow_mut().push(k.to_string()));
        keys
    }

    #[test]
    #[cfg_attr(not(slint_debug_info), ignore = "needs the Slint debug info: a debug build, or SLINT_EMIT_DEBUG_INFO=1")]
    fn the_hidden_treemap_never_takes_the_focus_or_a_key_while_the_graph_shows() {
        let (ui, _dir) = graph_window();
        crate::select(&ui, node("a"));
        let map_keys = record_map_keys(&ui);
        let graph_keys = record_graph_keys(&ui);

        // The treemap had the focus when the graph came (a scan ends with Graph chosen).
        ui.set_graph_shown(false);
        ui.invoke_focus_map();
        run_change_handlers();
        ui.set_graph_shown(true);
        run_change_handlers();
        press(&ui, Key::Delete);
        assert!(map_keys.borrow().is_empty(), "Delete reached the hidden treemap: {:?}", map_keys.borrow());

        // Tab goes round the window and never stops on the hidden treemap.
        for _ in 0..40 {
            press(&ui, Key::Tab);
            press(&ui, Key::DownArrow);
        }
        assert!(map_keys.borrow().is_empty(), "a key reached the hidden treemap: {:?}", map_keys.borrow());
        assert!(graph_keys.borrow().iter().any(|k| k == "down"), "Tab reaches the graph");

        // With the graph focused, Delete goes to the graph's handler only.
        graph_keys.borrow_mut().clear();
        ui.invoke_focus_map();
        run_change_handlers();
        press(&ui, Key::Delete);
        assert_eq!(*graph_keys.borrow(), vec!["delete".to_string()]);
        assert!(map_keys.borrow().is_empty());
        assert_eq!(ui.get_delete().step, DeleteStep::Closed, "the recorder took the key");
    }
}

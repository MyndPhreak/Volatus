//! The delete dialog, the Delete keys and the Delete of the context menu, wired to `scan_apply`.
//!
//! The dialog shows `SESSION.delete.state`: `sync` copies `delete_flow::view` of it into the
//! window. It runs after every delete callback here, after each message of a running delete and
//! each try of a pending removal (`scan_apply`), and after each scan change (`show_state`,
//! `clear`).
//!
//! Shift for the menu's Delete (`menu_delete`, from the shell menu's Volatus group): a menu entry
//! has no key state, so `shift_held` asks Windows when the entry is chosen. Both the Shift state of the message being handled (`GetKeyState`) and the
//! key's state now (`GetAsyncKeyState`) must say "down"; any other answer is the Recycle Bin. The
//! keys carry their own Shift state (`Keys.event-name` in Slint).

use super::{DeleteInfo, DeleteStep, MainWindow, SESSION, start_scan};
use crate::delete_flow::{self, DialogState, DialogView, Step, TreeAction};
use crate::scan_apply::{self, RemoverFor, Spawn};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;

/// What the wiring needs from the outside: the engines and where a delete runs. `Deps::real` in
/// the app; the tests give fakes, so no test runs a real delete.
pub(crate) struct Deps {
    pub(crate) remover_for: Box<RemoverFor>,
    pub(crate) spawn: Box<Spawn>,
}

impl Deps {
    pub(crate) fn real() -> Deps {
        Deps { remover_for: Box::new(scan_apply::remover_for), spawn: Box::new(scan_apply::spawn_worker) }
    }
}

/// Shows the delete dialog of the session in the window.
pub(crate) fn sync(ui: &MainWindow) {
    let view = SESSION.with_borrow(|s| delete_flow::view(&s.delete.state));
    ui.set_delete(info(&view));
}

/// The window's form of a dialog view.
fn info(v: &DialogView) -> DeleteInfo {
    DeleteInfo {
        step: match v.step {
            Step::Closed => DeleteStep::Closed,
            Step::Refused => DeleteStep::Refused,
            Step::Confirm => DeleteStep::Confirm,
            Step::Running => DeleteStep::Running,
            Step::Done => DeleteStep::Done,
        },
        title: v.title.into(),
        permanent: v.permanent,
        name: v.name.as_str().into(),
        path: v.path.as_str().into(),
        size: v.size.as_str().into(),
        counts: v.counts.as_str().into(),
        effect: v.effect.into(),
        link_note: v.link_note.into(),
        text: v.text.as_str().into(),
        action: v.action.into(),
        cancel: v.cancel.into(),
        action_default: v.action_default,
        switch_text: v.switch.into(),
        progress_line: v.progress_line.as_str().into(),
        progress: v.progress,
        indeterminate: v.indeterminate,
        can_stop: v.can_stop,
        stopping: v.stopping,
        stop_note: v.stop_note.into(),
        result: lines(&v.result),
        skips: lines(&v.skips),
        rescan: v.rescan,
        admin: v.admin.into(),
    }
}

fn lines(lines: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(lines.iter().map(|l| SharedString::from(l.as_str())).collect::<Vec<_>>()))
}

/// Delete (`shift` false) or Shift+Delete (`shift` true) on the selection.
/// Nothing while a scan runs (`open_in` gives no dialog then).
pub(crate) fn request(ui: &MainWindow, shift: bool) {
    scan_apply::request_delete(ui, shift);
    sync(ui);
}

/// The Delete of the context menu, for the item the menu was opened on (`node`, which need not be
/// the selection: a crumb is not selected). `shift` is the Shift key when the item was chosen.
pub(crate) fn menu_delete_with(ui: &MainWindow, node: scan::NodeId, shift: bool) {
    scan_apply::request_delete_of(ui, Some(node), shift);
    sync(ui);
}

/// The Delete of the context menu (`menu::Cmd::Delete`) on `node`, with the Shift key read now.
pub(crate) fn menu_delete(ui: &MainWindow, node: scan::NodeId) {
    menu_delete_with(ui, node, shift_held());
}

/// True only when Windows says Shift is down both for the message being handled and now.
fn shift_from(queued: i16, now: i16) -> bool {
    queued < 0 && now < 0
}

/// The Shift key, read when the menu's Delete is chosen (see the module text).
fn shift_held() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, VK_SHIFT};
    // SAFETY: two reads of the keyboard state, with no pointers.
    let (queued, now) = unsafe { (GetKeyState(i32::from(VK_SHIFT)), GetAsyncKeyState(i32::from(VK_SHIFT))) };
    shift_from(queued, now)
}

/// The window's HWND, for the Recycle Bin's shell windows; `None` when the window has none.
fn owner(ui: &MainWindow) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match ui.window().window_handle().window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// "Scan again": only from a result that offers it. The dialog closes and the scan root, as
/// typed, is scanned again.
fn rescan(ui: &MainWindow) {
    let offered = |state: &DialogState| matches!(state, DialogState::Done { tree_action: TreeAction::KeepAndOfferRescan, .. });
    let Some(root) = SESSION.with_borrow(|s| offered(&s.delete.state).then(|| s.scan_root.clone())) else { return };
    scan_apply::close_delete();
    sync(ui);
    start_scan(ui, &root.to_string_lossy());
}

/// Connects the dialog to the session.
pub(crate) fn wire(ui: &MainWindow, deps: Deps) {
    let deps = Rc::new(deps);
    let with_ui = |f: Box<dyn Fn(&MainWindow)>| {
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                f(&ui);
            }
        }
    };
    ui.on_confirm_delete(with_ui({
        let deps = deps.clone();
        Box::new(move |ui| {
            scan_apply::confirm_delete(ui, owner(ui), &*deps.remover_for, &*deps.spawn);
            sync(ui);
        })
    }));
    ui.on_switch_delete_mode(with_ui(Box::new(|ui| {
        scan_apply::switch_delete_mode();
        sync(ui);
    })));
    ui.on_stop_delete(with_ui(Box::new(|ui| {
        scan_apply::stop_delete();
        sync(ui);
    })));
    ui.on_close_delete(with_ui(Box::new(|ui| {
        scan_apply::close_delete();
        sync(ui);
    })));
    ui.on_delete_rescan(with_ui(Box::new(rescan)));
    // The window does not close while a permanent delete runs (`delete_flow::may_close_window`).
    ui.window().on_close_requested(|| {
        if SESSION.with_borrow(|s| delete_flow::may_close_window(&s.delete.state)) {
            slint::CloseRequestResponse::HideWindow
        } else {
            slint::CloseRequestResponse::KeepWindowShown
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Data, select};
    use crate::delete::{DeleteMode, DeleteTarget, Outcome, Remover, SkipReason, Skipped};
    use crate::model::{Event, ScanState, Summary};
    use crate::tree_view::file_counts;
    use i_slint_backend_testing::ElementHandle;
    use scan::NodeId;
    use scan::rollup::{RawEntry, rollup};
    use slint::platform::{Key, WindowEvent};
    use slint::Model;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    /// A remover that reports one progress step and returns `OUTCOME`. Never touches the disk.
    struct FakeRemover(Outcome);

    impl Remover for FakeRemover {
        fn remove(&self, _: &DeleteTarget, progress: &dyn Fn(u64, u64), _: &AtomicBool) -> Outcome {
            progress(1, 5);
            self.0.clone()
        }
    }

    type Job = (Arc<dyn Remover>, DeleteTarget, u64, Arc<AtomicBool>);

    thread_local! {
        /// The modes the engines were asked for.
        static MODES: RefCell<Vec<DeleteMode>> = const { RefCell::new(Vec::new()) };
        /// The deletes that were started and not run yet.
        static JOBS: RefCell<Vec<Job>> = const { RefCell::new(Vec::new()) };
        static OUTCOME: RefCell<Outcome> = RefCell::new(Outcome::default());
    }

    fn fake_deps() -> Deps {
        Deps {
            remover_for: Box::new(|mode, _| {
                MODES.with_borrow_mut(|m| m.push(mode));
                Arc::new(FakeRemover(OUTCOME.with_borrow(Clone::clone)))
            }),
            spawn: Box::new(|remover, target, generation, stop, _post| {
                JOBS.with_borrow_mut(|j| j.push((remover, target, generation, stop)));
                Ok(())
            }),
        }
    }

    /// A folder the test made, removed at the end.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `<temp>` { `a` { `f.bin` 10 }, `b.bin` 5 } on the disk and as a scan.
    fn fixture() -> (TempDir, Arc<Data>) {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("volatus-ui-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a").join("f.bin"), [0u8; 10]).unwrap();
        std::fs::write(root.join("b.bin"), [0u8; 5]).unwrap();
        let e = |id: u64, parent: u64, name: &str, size: u64, is_dir: bool| RawEntry {
            id,
            parent,
            name: name.into(),
            size,
            allocated: size,
            is_dir,
        };
        let entries = vec![
            e(0, 0, &root.to_string_lossy(), 0, true),
            e(1, 0, "a", 0, true),
            e(2, 1, "f.bin", 10, false),
            e(3, 0, "b.bin", 5, false),
        ];
        let tree = rollup(0, entries).unwrap().tree;
        (TempDir(root), Arc::new(Data { counts: file_counts(&tree), tree }))
    }

    fn summary() -> Summary {
        Summary { method: scan::Method::Walk, files: 2, dirs: 1, skipped: 0, elapsed: Duration::ZERO, phases: Default::default(), total: 15 }
    }

    /// A window on the testing backend (this thread only) with a finished scan of the fixture
    /// and `a` selected.
    fn window() -> (MainWindow, TempDir) {
        i_slint_backend_testing::init_no_event_loop();
        window_on_this_platform()
    }

    /// `window` on the platform this thread already has.
    fn window_on_this_platform() -> (MainWindow, TempDir) {
        let ui = MainWindow::new().unwrap();
        crate::wire_tree_and_treemap(&ui);
        wire(&ui, fake_deps());
        let (dir, data) = fixture();
        SESSION.with_borrow_mut(|s| {
            let (g, _) = s.scan.start().expect("no scan runs");
            assert!(s.scan.apply(g, Event::Finished(summary())));
            s.scan_root = dir.0.clone();
        });
        crate::install(&ui, data);
        crate::show_state(&ui);
        select_named(&ui, "a");
        ui.show().unwrap();
        (ui, dir)
    }

    fn node(name: &str) -> NodeId {
        SESSION.with_borrow(|s| {
            let tree = &s.loaded.as_ref().unwrap().data.tree;
            let mut stack = vec![tree.root()];
            while let Some(n) = stack.pop() {
                if tree.name(n).ends_with(name) {
                    return n;
                }
                stack.extend(tree.children(n));
            }
            panic!("no node {name}")
        })
    }

    fn select_named(ui: &MainWindow, name: &str) {
        select(ui, node(name));
    }

    fn step(ui: &MainWindow) -> DeleteStep {
        ui.get_delete().step
    }

    /// The buttons (and the link) the window shows with this label.
    fn buttons(ui: &MainWindow, label: &str) -> Vec<ElementHandle> {
        ElementHandle::find_by_accessible_label(ui, label)
            .filter(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button))
            .collect()
    }

    fn has_button(ui: &MainWindow, label: &str) -> bool {
        !buttons(ui, label).is_empty()
    }

    fn click(ui: &MainWindow, label: &str) {
        // The search finds an element once per component level it belongs to.
        let b = buttons(ui, label);
        assert!(!b.is_empty(), "a {label:?} button");
        b[0].invoke_accessible_default_action();
    }

    fn key(ui: &MainWindow, k: Key) {
        // The dialog's elements exist before a key reaches them.
        let _ = buttons(ui, "");
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: k.into() });
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: k.into() });
    }

    /// Runs the started delete with the fake remover and gives its messages to the window, as the
    /// worker would. `gone`: the item is not on the disk after the run.
    fn deliver(ui: &MainWindow, gone: bool) {
        let job = JOBS.with_borrow_mut(|j| j.pop());
        assert!(job.is_some(), "a delete was started");
        let Some((remover, target, generation, stop)) = job else { return };
        let msgs = RefCell::new(Vec::new());
        delete_flow::run_delete(&*remover, &target, generation, &stop, &|_| gone, &|m| msgs.borrow_mut().push(m));
        for m in msgs.into_inner() {
            scan_apply::on_delete_msg(ui, m);
        }
    }

    fn modes() -> Vec<DeleteMode> {
        MODES.with_borrow(Clone::clone)
    }

    fn strings(m: &ModelRc<SharedString>) -> Vec<String> {
        m.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn shift_needs_both_reads_to_say_down() {
        for (queued, now, want) in [(-128, -127, true), (-128, 0, false), (0, -32768, false), (0, 0, false), (1, 1, false)] {
            assert_eq!(shift_from(queued, now), want, "{queued} {now}");
        }
    }

    #[test]
    fn delete_key_opens_the_recycle_bin_confirm() {
        let (ui, _dir) = window();
        ui.invoke_tree_key("delete".into());
        let d = ui.get_delete();
        assert_eq!(d.step, DeleteStep::Confirm);
        assert_eq!((d.action.as_str(), d.cancel.as_str(), d.action_default), ("Move to Recycle Bin", "Cancel", true));
        assert_eq!(d.name, "a");
        assert_eq!(d.counts, "1 file and 0 folders, from the scan.");
        assert!(has_button(&ui, "Move to Recycle Bin"));
        assert!(has_button(&ui, "Cancel"));
        assert!(has_button(&ui, "Delete permanently instead"), "the switch link shows in Confirm");
        assert!(!has_button(&ui, "Stop"));
    }

    #[test]
    fn shift_delete_opens_the_permanent_confirm_where_enter_cancels() {
        let (ui, _dir) = window();
        ui.invoke_map_key("shift-delete".into());
        let d = ui.get_delete();
        assert_eq!(d.step, DeleteStep::Confirm);
        assert_eq!((d.action.as_str(), d.action_default, d.permanent), ("Delete permanently", false, true));
        assert!(has_button(&ui, "Delete permanently"));
        assert!(has_button(&ui, "Move to Recycle Bin instead"));
        // Cancel is the default button: Enter closes the dialog and deletes nothing.
        key(&ui, Key::Return);
        assert_eq!(step(&ui), DeleteStep::Closed);
        assert!(modes().is_empty());
        assert!(JOBS.with_borrow(Vec::is_empty));
    }

    #[test]
    fn enter_in_the_recycle_bin_confirm_starts_the_recycle_bin() {
        let (ui, _dir) = window();
        ui.invoke_tree_key("delete".into());
        key(&ui, Key::Return);
        let d = ui.get_delete();
        assert_eq!(d.step, DeleteStep::Running);
        assert_eq!(modes(), vec![DeleteMode::RecycleBin]);
        assert!(d.indeterminate);
        assert!(!d.can_stop);
        assert!(!has_button(&ui, "Stop"), "the Recycle Bin has no Stop");
        assert!(!has_button(&ui, "Delete permanently instead"), "the switch link shows only in Confirm");
    }

    #[test]
    fn a_permanent_delete_has_stop_and_esc_does_nothing_while_it_runs() {
        let (ui, _dir) = window();
        ui.invoke_tree_key("shift-delete".into());
        click(&ui, "Delete permanently");
        assert_eq!(step(&ui), DeleteStep::Running);
        assert_eq!(modes(), vec![DeleteMode::Permanent]);
        assert!(ui.get_delete().can_stop);
        assert!(has_button(&ui, "Stop"));

        key(&ui, Key::Escape);
        assert_eq!(step(&ui), DeleteStep::Running, "Esc does nothing while a delete runs");
        ui.invoke_close_delete();
        assert_eq!(step(&ui), DeleteStep::Running, "Close does nothing while a delete runs");

        click(&ui, "Stop");
        assert!(ui.get_delete().stopping);
        assert_eq!(ui.get_delete().stop_note, delete_flow::STOP_NOTE);
        assert!(JOBS.with_borrow(|j| j[0].3.load(Ordering::Relaxed)), "the job's stop flag is set");
    }

    #[test]
    fn closing_the_window_while_a_permanent_delete_runs_keeps_it_shown_until_the_run_ends() {
        let (ui, _dir) = window();
        ui.invoke_tree_key("shift-delete".into());
        click(&ui, "Delete permanently");
        assert_eq!(step(&ui), DeleteStep::Running);

        ui.window().dispatch_event(WindowEvent::CloseRequested);
        assert!(ui.window().is_visible(), "the window stays while the permanent delete runs");
        click(&ui, "Stop");
        ui.window().dispatch_event(WindowEvent::CloseRequested);
        assert!(ui.window().is_visible(), "Stop ends the run after the item in progress: still shown");

        deliver(&ui, false);
        assert_eq!(step(&ui), DeleteStep::Done);
        ui.window().dispatch_event(WindowEvent::CloseRequested);
        assert!(!ui.window().is_visible(), "the window closes once the run ended");
    }

    #[test]
    fn the_switch_link_changes_the_mode_in_confirm() {
        let (ui, _dir) = window();
        ui.invoke_tree_key("delete".into());
        click(&ui, "Delete permanently instead");
        let d = ui.get_delete();
        assert_eq!((d.action.as_str(), d.action_default), ("Delete permanently", false));
        assert!(has_button(&ui, "Move to Recycle Bin instead"));
        // The focus moves to the new default button, Cancel: Enter deletes nothing.
        key(&ui, Key::Return);
        assert_eq!(step(&ui), DeleteStep::Closed);
        assert!(modes().is_empty());
    }

    #[test]
    fn switching_back_to_the_recycle_bin_makes_its_button_the_default_again() {
        let (ui, _dir) = window();
        ui.invoke_tree_key("shift-delete".into());
        click(&ui, "Move to Recycle Bin instead");
        key(&ui, Key::Return);
        assert_eq!(step(&ui), DeleteStep::Running);
        assert_eq!(modes(), vec![DeleteMode::RecycleBin]);
    }

    #[test]
    fn a_result_with_skips_lists_them_and_offers_a_rescan() {
        let (ui, _dir) = window();
        let skipped = vec![Skipped { path: PathBuf::from(r"C:\x\locked.bin"), reason: SkipReason::AccessDenied }];
        OUTCOME.with_borrow_mut(|o| *o = Outcome { removed_items: 1, removed_bytes: 5, skipped: skipped.clone(), stopped: false });
        ui.invoke_tree_key("shift-delete".into());
        click(&ui, "Delete permanently");
        deliver(&ui, false);
        let d = ui.get_delete();
        assert_eq!(d.step, DeleteStep::Done);
        assert_eq!(strings(&d.skips), vec![delete_flow::skip_line(&skipped[0])]);
        assert!(d.rescan);
        assert!(has_button(&ui, "Scan again"));
        assert!(has_button(&ui, "Restart as administrator"));
        assert!(has_button(&ui, "Close"));
        assert!(!has_button(&ui, "Stop"));
        assert!(!has_button(&ui, "Move to Recycle Bin instead"));
        // The tree is kept: the item is still on the disk.
        assert!(SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.tree.children(s.loaded.as_ref().unwrap().data.tree.root()).count()) == 2);
    }

    #[test]
    fn scan_again_closes_the_dialog_and_scans_the_root_again() {
        let (ui, _dir) = window();
        OUTCOME.with_borrow_mut(|o| *o = Outcome { removed_items: 1, removed_bytes: 5, ..Outcome::default() });
        ui.invoke_tree_key("shift-delete".into());
        click(&ui, "Delete permanently");
        deliver(&ui, false);
        assert!(ui.get_delete().rescan);
        click(&ui, "Scan again");
        assert_eq!(step(&ui), DeleteStep::Closed);
        assert!(ui.get_scanning());
    }

    #[test]
    fn a_recycle_bin_result_says_the_space_frees_when_the_bin_is_emptied() {
        let (ui, _dir) = window();
        OUTCOME.with_borrow_mut(|o| *o = Outcome { removed_items: 1, removed_bytes: 10, ..Outcome::default() });
        ui.invoke_tree_key("delete".into());
        key(&ui, Key::Return);
        deliver(&ui, false);
        let d = ui.get_delete();
        assert_eq!(d.step, DeleteStep::Done);
        assert!(strings(&d.result).iter().any(|l| l.contains("empty the Recycle Bin")), "{:?}", strings(&d.result));
        // Close is the default button.
        key(&ui, Key::Return);
        assert_eq!(step(&ui), DeleteStep::Closed);
    }

    #[test]
    fn the_scan_root_is_refused_with_a_reason_and_close() {
        let (ui, _dir) = window();
        let root = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.tree.root());
        select(&ui, root);
        ui.invoke_tree_key("delete".into());
        let d = ui.get_delete();
        assert_eq!(d.step, DeleteStep::Refused);
        assert_eq!(d.text, crate::delete::refusal_text(crate::delete::Refusal::NotUnderRoot));
        assert!(has_button(&ui, "Close"));
        assert!(!has_button(&ui, "Move to Recycle Bin"));
        key(&ui, Key::Escape);
        assert_eq!(step(&ui), DeleteStep::Closed);
    }

    #[test]
    fn a_delete_while_a_scan_runs_does_nothing() {
        let (ui, _dir) = window();
        SESSION.with_borrow_mut(|s| {
            assert!(s.scan.reset());
            s.scan.start().expect("a scan starts");
            assert!(matches!(s.scan.state(), ScanState::Scanning { .. }));
        });
        ui.invoke_tree_key("delete".into());
        ui.invoke_map_key("shift-delete".into());
        menu_delete_with(&ui, node("b.bin"), true);
        assert_eq!(step(&ui), DeleteStep::Closed);
        assert!(SESSION.with_borrow(|s| s.delete.state == DialogState::Closed));
    }

    #[test]
    fn the_menu_delete_is_for_the_menu_item_and_reads_shift() {
        let (ui, _dir) = window();
        // The selection is `a`; the menu was opened on `b.bin` (a crumb, say, is not selected).
        menu_delete_with(&ui, node("b.bin"), false);
        let d = ui.get_delete();
        assert_eq!((d.step, d.name.as_str(), d.permanent), (DeleteStep::Confirm, "b.bin", false));
        assert_eq!(d.action.as_str(), "Move to Recycle Bin");
        ui.invoke_close_delete();
        menu_delete_with(&ui, node("b.bin"), true);
        let d = ui.get_delete();
        assert_eq!((d.step, d.name.as_str(), d.permanent, d.action_default), (DeleteStep::Confirm, "b.bin", true, false));
        // Nothing runs before the confirm click.
        assert!(modes().is_empty());
        assert!(JOBS.with_borrow(Vec::is_empty));
    }

    #[test]
    fn the_menu_delete_of_the_scan_root_is_refused() {
        let (ui, _dir) = window();
        let root = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.tree.root());
        menu_delete_with(&ui, root, false);
        assert_eq!(step(&ui), DeleteStep::Refused);
    }

    /// The tree rows a right-click asked a menu for (`tree-menu`), in order.
    fn record_tree_menus(ui: &MainWindow) -> Rc<RefCell<Vec<i32>>> {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let log = asked.clone();
        ui.on_tree_menu(move |i, _, _| log.borrow_mut().push(i));
        asked
    }

    fn element(ui: &MainWindow, role: i_slint_backend_testing::AccessibleRole, start: &str) -> ElementHandle {
        i_slint_backend_testing::ElementQuery::from_root(ui)
            .match_descendants()
            .match_accessible_role(role)
            .find_all()
            .into_iter()
            .find(|e| e.accessible_label().is_some_and(|l| l.starts_with(start)))
            .unwrap_or_else(|| panic!("an element {start:?}"))
    }

    /// A right press and release at a window point.
    fn right_click_at(ui: &MainWindow, position: slint::LogicalPosition) {
        let button = slint::platform::PointerEventButton::Right;
        ui.window().dispatch_event(WindowEvent::PointerPressed { position, button });
        ui.window().dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    #[test]
    fn a_right_click_on_empty_tree_space_asks_for_no_menu() {
        use i_slint_backend_testing::AccessibleRole;
        let (ui, _dir) = window();
        let asked = record_tree_menus(&ui);
        let tree = element(&ui, AccessibleRole::Tree, "File tree");
        let (p, s) = (tree.absolute_position(), tree.size());
        // Below the last row, and in the 2 px margin at the bottom of the tree.
        for y in [p.y + s.height - 20.0, p.y + s.height - 1.0] {
            right_click_at(&ui, slint::LogicalPosition::new(p.x + s.width / 2.0, y));
        }
        assert_eq!(*asked.borrow(), Vec::<i32>::new());
        assert_eq!(step(&ui), DeleteStep::Closed);
    }

    #[test]
    fn a_right_click_on_a_row_asks_for_the_menu_of_that_row() {
        use i_slint_backend_testing::AccessibleRole;
        let (ui, _dir) = window();
        let asked = record_tree_menus(&ui);
        let row = element(&ui, AccessibleRole::ListItem, "b.bin");
        let (p, s) = (row.absolute_position(), row.size());
        right_click_at(&ui, slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0));
        let want = SESSION.with_borrow(|s| {
            let l = s.loaded.as_ref().unwrap();
            l.model.rows.borrow().iter().position(|r| l.data.tree.name(r.node) == "b.bin").map(|i| i as i32)
        });
        assert_eq!(asked.borrow().clone(), want.into_iter().collect::<Vec<_>>());
    }

    #[test]
    fn a_held_delete_key_opens_one_dialog() {
        let (ui, _dir) = window();
        ui.invoke_tree_key("delete".into());
        // Esc closes the dialog and gives the focus back to the tree.
        key(&ui, Key::Escape);
        assert_eq!(step(&ui), DeleteStep::Closed);
        ui.window().dispatch_event(WindowEvent::KeyPressRepeated { text: Key::Delete.into() });
        assert_eq!(step(&ui), DeleteStep::Closed, "a repeat opens no dialog");
        // A new press in the tree opens it again.
        key(&ui, Key::Delete);
        assert_eq!(step(&ui), DeleteStep::Confirm);
    }

    #[test]
    fn shift_delete_through_the_tree_key_is_permanent_and_ctrl_delete_is_nothing() {
        let (ui, _dir) = window();
        ui.invoke_tree_key("delete".into());
        key(&ui, Key::Escape);
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        key(&ui, Key::Delete);
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        assert_eq!(step(&ui), DeleteStep::Closed, "Ctrl+Delete deletes nothing");
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
        key(&ui, Key::Delete);
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
        assert_eq!((step(&ui), ui.get_delete().permanent), (DeleteStep::Confirm, true));
    }

    #[test]
    fn keys_while_the_dialog_shows_never_reach_the_tree() {
        let (ui, _dir) = window();
        let a = node("a");
        ui.invoke_tree_key("delete".into());
        // Tab goes round the dialog only: the tree, the map, the crumbs and the headers behind
        // it take no focus, so the arrows and Delete change nothing there.
        for _ in 0..12 {
            key(&ui, Key::Tab);
            key(&ui, Key::DownArrow);
            key(&ui, Key::Delete);
        }
        assert_eq!(SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().selected), Some(a));
        assert_eq!(step(&ui), DeleteStep::Confirm);
        assert!(modes().is_empty());
    }

    #[test]
    fn a_held_enter_does_not_confirm() {
        let (ui, _dir) = window();
        ui.invoke_tree_key("delete".into());
        let _ = buttons(&ui, "");
        ui.window().dispatch_event(WindowEvent::KeyPressRepeated { text: Key::Return.into() });
        ui.window().dispatch_event(WindowEvent::KeyPressRepeated { text: Key::Space.into() });
        assert_eq!(step(&ui), DeleteStep::Confirm, "a repeat starts nothing");
        assert!(modes().is_empty());
        // A new press on the default button still starts it.
        key(&ui, Key::Return);
        assert_eq!(step(&ui), DeleteStep::Running);
    }

    /// A PNG file of an RGBA picture, with no compression (stored deflate blocks).
    fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut c = 0xffff_ffffu32;
            for &b in bytes {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
            out.extend((data.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend(kind);
            out.extend(data);
            let c = crc(&out[start..]);
            out.extend(c.to_be_bytes());
        }
        let mut raw = Vec::new();
        for row in rgba.chunks(width as usize * 4) {
            raw.push(0);
            raw.extend(row);
        }
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
        for (i, b) in blocks.iter().enumerate() {
            z.push(u8::from(i + 1 == blocks.len()));
            z.extend((b.len() as u16).to_le_bytes());
            z.extend((!(b.len() as u16)).to_le_bytes());
            z.extend(*b);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + u32::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        z.extend(((b << 16) | a).to_be_bytes());
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut head = Vec::new();
        head.extend(width.to_be_bytes());
        head.extend(height.to_be_bytes());
        head.extend([8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &head);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// Pictures of each dialog state, drawn by the software renderer, for a person to look at.
    /// Run with `VOLATUS_SNAPSHOTS=<folder> cargo test -p app -- --ignored snapshots`.
    #[test]
    #[ignore = "writes pictures to the folder in VOLATUS_SNAPSHOTS"]
    fn snapshots() {
        let Some(out) = std::env::var_os("VOLATUS_SNAPSHOTS").map(PathBuf::from) else { return };
        let options = i_slint_backend_testing::TestingBackendOptions {
            mock_time: true,
            threading: false,
            renderer_name: Some("software".into()),
        };
        slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(options))).unwrap();
        let (ui, _dir) = window_on_this_platform();
        ui.window().set_size(slint::PhysicalSize::new(1100, 700));
        ui.set_mica(false);
        // The engine takes its result when the delete starts.
        let skipped = (0..6)
            .map(|i| Skipped { path: PathBuf::from(format!(r"C:\data\a\locked file number {i} with a long name.bin")), reason: SkipReason::Locked })
            .chain([Skipped { path: PathBuf::from(r"C:\data\a\x"), reason: SkipReason::AccessDenied }])
            .collect();
        OUTCOME.with_borrow_mut(|o| *o = Outcome { removed_items: 3, removed_bytes: 4, skipped, stopped: true });
        let shot = |name: &str| {
            let _ = buttons(&ui, "");
            // The buttons' colour changes are animated: let them end.
            i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(1));
            let pic = ui.window().take_snapshot().expect("snapshot");
            std::fs::write(out.join(format!("{name}.png")), png(pic.width(), pic.height(), pic.as_bytes())).unwrap();
        };
        ui.invoke_tree_key("delete".into());
        shot("1-confirm-recycle");
        click(&ui, "Delete permanently instead");
        shot("2-confirm-permanent");
        ui.invoke_close_delete();
        ui.invoke_tree_key("shift-delete".into());
        shot("2b-confirm-permanent-from-shift");
        click(&ui, "Delete permanently");
        let generation = JOBS.with_borrow(|j| j[0].2);
        scan_apply::on_delete_msg(&ui, delete_flow::DeleteMsg::Progress { generation, items: 1, bytes: 4 });
        shot("3-running-permanent");
        click(&ui, "Stop");
        shot("4-running-stopping");
        deliver(&ui, false);
        shot("5-done-skips");
        ui.invoke_close_delete();
        let root = SESSION.with_borrow(|s| s.loaded.as_ref().unwrap().data.tree.root());
        select(&ui, root);
        ui.invoke_tree_key("delete".into());
        shot("6-refused");
        ui.invoke_close_delete();
        select_named(&ui, "b.bin");
        OUTCOME.with_borrow_mut(|o| *o = Outcome { removed_items: 1, removed_bytes: 5, ..Outcome::default() });
        ui.invoke_tree_key("delete".into());
        key(&ui, Key::Return);
        shot("7-running-recycle");
        deliver(&ui, false);
        shot("8-done-recycle");
    }
}

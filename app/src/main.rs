//! Storage tool window: Mica backdrop, scan controls and the result banner.
//!
//! Mica recipe (proved by a spike, see `.claude/work/storagetool-wiztree-clone/logs/P3-mica-spike.md`):
//! the root `Window` background starts transparent; `slint::Window::window_handle()` only yields
//! a handle after the winit window exists, so `apply_mica` runs from `invoke_from_event_loop`
//! queued before the event loop starts. Calling it right after `show()` returns `NotSupported`.
//! If Mica is not available (Windows 10), the window falls back to the opaque Fluent background.

mod model;

use model::{Event, ScanState, Summary, banner_text, error_message, next};
use scan::{Progress, ScanError};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

slint::include_modules!();

/// Minimum gap between progress updates sent to the UI.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// UI-thread state. Events from worker threads are applied here via `invoke_from_event_loop`.
struct Session {
    state: ScanState,
    /// Id of the current scan. Events from an older scan are dropped.
    generation: u64,
    cancel: Arc<AtomicBool>,
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session {
        state: ScanState::Idle,
        generation: 0,
        cancel: Arc::new(AtomicBool::new(false)),
    });
}

/// Applies `event` to the session and pushes the new state into the window.
fn apply(ui: &MainWindow, event: Event) {
    let state = SESSION.with_borrow_mut(|s| {
        s.state = next(std::mem::replace(&mut s.state, ScanState::Idle), event);
        s.state.clone()
    });
    ui.set_scanning(matches!(state, ScanState::Scanning { .. }));
    ui.set_banner_is_error(matches!(state, ScanState::Error(_)));
    ui.set_banner(banner_text(&state).into());
}

/// Queues `event` for the UI thread. Dropped if a newer scan has started.
fn post(ui: &Weak<MainWindow>, generation: u64, event: Event) {
    let ui = ui.clone();
    // Err means the event loop has stopped: nothing left to update.
    let _ = slint::invoke_from_event_loop(move || {
        let current = SESSION.with_borrow(|s| s.generation);
        if let Some(ui) = ui.upgrade().filter(|_| current == generation) {
            apply(&ui, event);
        }
    });
}

fn start_scan(ui: &MainWindow, path: &str) {
    let path = path.trim();
    if SESSION.with_borrow(|s| matches!(s.state, ScanState::Scanning { .. })) {
        return;
    }
    let generation = SESSION.with_borrow_mut(|s| {
        s.generation += 1;
        s.cancel = Arc::new(AtomicBool::new(false));
        s.generation
    });
    apply(ui, Event::Start);
    if path.is_empty() {
        apply(ui, Event::Failed("Choose a folder or drive first.".into()));
        return;
    }
    let cancel = SESSION.with_borrow(|s| s.cancel.clone());
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

    std::thread::spawn(move || {
        let event = match scan::scan(&root, tx, &cancel) {
            Ok((tree, stats)) => Event::Finished(Summary::new(&stats, tree.node(tree.root()).size)),
            Err(ScanError::Cancelled) => Event::Cancelled,
            Err(e) => Event::Failed(error_message(&e)),
        };
        post(&weak, generation, event);
    });
}

/// Existing drive roots, `A:\` to `Z:\`.
fn drive_roots() -> Vec<SharedString> {
    ('A'..='Z')
        .map(|c| format!("{c}:\\"))
        .filter(|d| Path::new(d).exists())
        .map(Into::into)
        .collect()
}

fn main() {
    let ui = MainWindow::new().expect("create window");
    ui.set_drives(ModelRc::new(VecModel::from(drive_roots())));
    ui.set_banner(banner_text(&ScanState::Idle).into());
    ui.on_scan({
        let weak = ui.as_weak();
        move |path| {
            if let Some(ui) = weak.upgrade() {
                start_scan(&ui, &path);
            }
        }
    });
    ui.on_cancel(|| SESSION.with_borrow(|s| s.cancel.store(true, Ordering::Relaxed)));
    ui.on_browse({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                ui.set_path(dir.to_string_lossy().as_ref().into());
            }
        }
    });

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
    slint::run_event_loop().expect("run event loop");
}

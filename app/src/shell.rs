//! The Windows shell menu of an item: COM, `IContextMenu`/2/3, the popup and its owner window.
//! Exempt from test-first except the menu build (the tests below; the rest is manual checks M1-M17
//! of the plan); the decisions it uses are in `menu`.
//!
//! Choices from the spike (`.claude/work/storagetool-wiztree-clone/logs/CM-spike.md`, S1-S10):
//! - Owner: the Slint window, subclassed (`SetWindowSubclass`, no comctl32 v6 manifest needed). The
//!   subclass passes `WM_INITMENUPOPUP` (Open with fills itself only then), `WM_DRAWITEM`,
//!   `WM_MEASUREITEM` and `WM_MENUCHAR` to the shell menu. It does not act on `WM_CONTEXTMENU`:
//!   the Menu key and Shift+F10 come through Slint's key events (`Key.Menu`, `Key.F10` + shift),
//!   and the window gets `WM_CONTEXTMENU` for them too (a second menu). If the subclass cannot be
//!   set, a hidden helper window owned by the Slint window is the owner, with the same dispatch.
//! - The menu shows from the Slint callback itself (S8): while it shows, winit buffers every event,
//!   so no Slint code runs; the window does not repaint until it closes.
//! - After the menu, the window gets one `WM_MOUSEMOVE` at the pointer (S8): else the click that
//!   closed the menu lands where the right-click was.
//! - `CoInitializeEx` (STA) gives `S_FALSE` on the UI thread (winit called `OleInitialize`): ready.
//! - The shell gets the plain path: a `\\?\` path fails (S10) and the menu degrades.
//! - `QueryContextMenu` can block up to about 0.8 s (S9): a wait cursor shows meanwhile.
//!
//! The shell's own Delete is taken out of the menu after `QueryContextMenu` (`hide_shell_delete`):
//! Volatus has its own Delete in its group at the top (owner's decision, 2026-10-26), so the menu
//! has one Delete. An item whose verb the shell does not give stays.

use crate::menu::{Anchor, Cmd, Item, Picked, SHELL_FIRST, SHELL_LAST, is_delete_verb, picked, separators_to_remove};
use std::cell::{Cell, RefCell};
use std::path::{Component, Path, PathBuf};
use windows::Win32::Foundation::{E_ABORT, E_FAIL, E_INVALIDARG, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, GetKeyState, SetFocus, VIRTUAL_KEY};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, DefSubclassProc, GCS_VERBW, IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, RemoveWindowSubclass, SHBindToParent,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_IDLIST, SHELLEXECUTEINFOW, SHOpenFolderAndSelectItems, SHParseDisplayName, SetWindowSubclass, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DeleteMenu, DestroyMenu, GWLP_HINSTANCE, GetCursorPos, GetMenuItemCount,
    GetMenuItemID, GetMenuItemInfoW, GetWindowLongPtrW, HMENU, IDC_WAIT, InsertMenuW, LoadCursorW, MENUITEMINFOW, MF_BYPOSITION,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, MFT_SEPARATOR, MIIM_FTYPE, RegisterClassW, SW_SHOWNORMAL,
    SendMessageW, SetCursor, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_VERTICAL, TPMPARAMS, TrackPopupMenuEx, WINDOW_EX_STYLE, WM_DRAWITEM, WM_INITMENUPOPUP,
    WM_MEASUREITEM, WM_MENUCHAR, WM_MOUSEMOVE, WM_NCDESTROY, WNDCLASSW, WS_POPUP,
};
use windows::core::{HSTRING, Interface, PCSTR, PCWSTR, PSTR, w};

/// Id of the subclass of the Slint window.
const SUBCLASS_ID: usize = 1;
/// Window class of the helper owner window (used only when the subclass cannot be set).
const HELPER_CLASS: PCWSTR = w!("VolatusMenuOwner");

thread_local! {
    /// The shell menu that shows now, for the messages of its submenus. Set only during `track`.
    static ACTIVE: RefCell<Option<Handlers>> = const { RefCell::new(None) };
    /// The menu owner, once made (`menu_owner`).
    static OWNER: Cell<Option<HWND>> = const { Cell::new(None) };
    /// COM is ready on this thread; `None` before the first try.
    static COM: Cell<Option<bool>> = const { Cell::new(None) };
}

/// The interfaces of a shell menu that draw and fill its submenus.
#[derive(Clone)]
struct Handlers {
    cm2: Option<IContextMenu2>,
    cm3: Option<IContextMenu3>,
}

/// A shell item id list from `SHParseDisplayName`, freed when dropped.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        // SAFETY: the shell allocated it with the COM allocator; it is freed once.
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

/// The id list of `path` (the plain path, never `\\?\`). The shell takes no `/`, `.` or `..`, which
/// a scan root typed on the command line can have: `path` is made absolute first, as Windows does
/// for the disk check (`menu::read_disk`), so both name the same item. A name that is not
/// `menu::plain_name` is refused (`E_INVALIDARG`): the shell would name another item ("a." gives "a").
fn parse(path: &Path) -> windows::core::Result<Pidl> {
    let plain = |c: Component| !matches!(c, Component::Normal(name) if !name.to_str().is_some_and(crate::menu::plain_name));
    if !path.components().all(plain) {
        return Err(E_INVALIDARG.into());
    }
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut pidl = std::ptr::null_mut();
    // SAFETY: the name outlives the call; the result is owned by `Pidl`.
    unsafe { SHParseDisplayName(&HSTRING::from(path.as_os_str()), None, &mut pidl, 0, None)? };
    Ok(Pidl(pidl))
}

/// COM (STA) is ready on this thread. `S_FALSE` (already started, as winit does) is ready too.
fn com_ready() -> bool {
    COM.with(|c| {
        if let Some(ok) = c.get() {
            return ok;
        }
        // SAFETY: no pointers. Never undone: the UI thread uses COM until the process ends.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_err() {
            eprintln!("The shell menu cannot start COM: {hr:?}.");
        }
        c.set(Some(hr.is_ok()));
        hr.is_ok()
    })
}

/// A popup menu: the Volatus items above the shell items of one path.
pub struct Popup {
    menu: HMENU,
    shell: Option<IContextMenu>,
    /// The id list the shell menu was made from; kept while the menu lives.
    _pidl: Option<Pidl>,
}

impl Drop for Popup {
    fn drop(&mut self) {
        // SAFETY: the menu was made by `CreatePopupMenu` and is destroyed once.
        let _ = unsafe { DestroyMenu(self.menu) };
    }
}

impl Popup {
    /// The menu for `path` (`None`: no shell items) with the shell flags `flags`, owned by `owner`.
    /// `items` gets whether the shell menu was built and gives the Volatus items, put above it.
    /// A shell menu that fails is left out (and logged): never a menu for another path.
    pub fn new(owner: HWND, path: Option<&Path>, flags: u32, items: impl FnOnce(bool) -> Vec<Item>) -> windows::core::Result<Self> {
        // SAFETY: no pointers; the menu is owned by the `Popup`.
        let mut popup = Popup { menu: unsafe { CreatePopupMenu()? }, shell: None, _pidl: None };
        if let Some(path) = path.filter(|_| com_ready()) {
            match shell_menu(owner, path, popup.menu, flags) {
                Ok((cm, pidl)) => (popup.shell, popup._pidl) = (Some(cm), Some(pidl)),
                Err(e) => eprintln!("Windows has no menu for {}: {e}", path.display()),
            }
        }
        for (at, item) in items(popup.shell.is_some()).iter().enumerate() {
            insert(popup.menu, at as u32, item)?;
        }
        Ok(popup)
    }

    /// The verb of each shell item of the top level, in menu order (`None`: the shell gave none).
    #[cfg(test)]
    pub fn verbs(&self) -> Vec<Option<String>> {
        // SAFETY: the menu lives as long as `self`.
        let count = unsafe { GetMenuItemCount(Some(self.menu)) };
        (0..count)
            .filter_map(|i| match picked(unsafe { GetMenuItemID(self.menu, i) }) {
                Picked::Shell(offset) => Some(self.verb(offset)),
                _ => None,
            })
            .collect()
    }

    /// Whether each line of the top level is a separator, in menu order.
    #[cfg(test)]
    pub fn separator_lines(&self) -> Vec<bool> {
        // SAFETY: the menu lives as long as `self`.
        let count = unsafe { GetMenuItemCount(Some(self.menu)) };
        (0..count.max(0) as u32).map(|i| is_separator(self.menu, i)).collect()
    }

    /// The canonical verb of the shell item at `offset` from `SHELL_FIRST` (`GCS_VERBW`), or `None`
    /// when the shell gives none (an error or an empty verb).
    #[cfg(test)]
    pub fn verb(&self, offset: u32) -> Option<String> {
        verb_of(self.shell.as_ref()?, offset)
    }

    /// Shows the menu at `at` until the user picks an item or closes it, owned by `owner` (from
    /// `menu_owner`). Returns the id picked, 0 for none. Afterwards `window` (the Slint window) gets
    /// the pointer where it is now and the keyboard focus.
    pub fn track(&self, owner: HWND, window: HWND, at: Anchor) -> u32 {
        let handlers = self.shell.as_ref().map(|cm| Handlers { cm2: cm.cast().ok(), cm3: cm.cast().ok() });
        ACTIVE.with_borrow_mut(|a| *a = handlers);
        // A key: the menu opens under the item's box and does not cover it (spike S5).
        let (x, y, flags, params) = match at {
            Anchor::Point(x, y) => (x, y, TPM_RETURNCMD | TPM_RIGHTBUTTON, None),
            Anchor::Below { left, top, right, bottom } => {
                let exclude = RECT { left, top, right, bottom };
                let params = TPMPARAMS { cbSize: size_of::<TPMPARAMS>() as u32, rcExclude: exclude };
                (left, bottom, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_VERTICAL, Some(params))
            }
        };
        // SAFETY: the menu, the owner and the parameters live for the call.
        let id = unsafe { TrackPopupMenuEx(self.menu, flags.0, x, y, owner, params.as_ref().map(|p| p as *const _)) }.0 as u32;
        ACTIVE.with_borrow_mut(|a| *a = None);
        after_menu(window);
        id
    }
}

impl Popup {
    /// Runs the shell item at `offset` from `SHELL_FIRST` with the window `window` as the owner of
    /// its dialogs, the point `at` (screen) and the `fMask` `mask` (`menu::invoke_mask`). Gives
    /// its HRESULT (`E_FAIL` with no shell menu). It returns when the item is done: a delete only
    /// after the item is gone or the user cancelled (spike S7).
    pub fn invoke(&self, offset: u32, window: HWND, at: (i32, i32), mask: u32) -> i32 {
        let Some(cm) = self.shell.as_ref() else { return E_FAIL.0 };
        // The verb as an id (MAKEINTRESOURCE): the offset in the low word of the pointer.
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: mask,
            hwnd: window,
            lpVerb: PCSTR(offset as usize as *const u8),
            lpVerbW: PCWSTR(offset as usize as *const u16),
            nShow: SW_SHOWNORMAL.0,
            ptInvoke: POINT { x: at.0, y: at.1 },
            ..Default::default()
        };
        // SAFETY: `info` is a CMINVOKECOMMANDINFOEX (its size in cbSize), as CMIC_MASK_UNICODE asks.
        match unsafe { cm.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const CMINVOKECOMMANDINFO) } {
            Ok(()) => 0,
            Err(e) => e.code().0,
        }
    }
}

/// The canonical verb of the item at `offset` from `SHELL_FIRST` of the shell menu `cm`
/// (`GCS_VERBW`), or `None` when the shell gives none (an error or an empty verb).
fn verb_of(cm: &IContextMenu, offset: u32) -> Option<String> {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer holds `buf.len()` UTF-16 units, as GCS_VERBW writes.
    unsafe { cm.GetCommandString(offset as usize, GCS_VERBW, None, PSTR(buf.as_mut_ptr().cast()), buf.len() as u32) }.ok()?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    (len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
}

/// Takes the shell's own Delete out of `menu`, which holds only the shell items of `cm` yet: every
/// top-level item whose canonical verb is `delete` (`menu::is_delete_verb`). An item whose verb
/// the shell does not give stays (never a guess). Then the separators left at an edge or doubled
/// go (`menu::separators_to_remove`). Returns how many items went.
fn hide_shell_delete(cm: &IContextMenu, menu: HMENU) -> usize {
    // SAFETY: a live popup menu of this thread.
    let count = unsafe { GetMenuItemCount(Some(menu)) }.max(0) as u32;
    let is_delete = |at: u32| {
        // SAFETY: `at` is a position of `menu`; a submenu gives no id (`picked` is `Nothing`).
        let id = unsafe { GetMenuItemID(menu, at as i32) };
        matches!(picked(id), Picked::Shell(offset) if verb_of(cm, offset).is_some_and(|v| is_delete_verb(&v)))
    };
    let gone: Vec<u32> = (0..count).filter(|&at| is_delete(at)).collect();
    for &at in gone.iter().rev() {
        // SAFETY: a position of `menu`, removed from the last one back.
        if let Err(e) = unsafe { DeleteMenu(menu, at, MF_BYPOSITION) } {
            eprintln!("The shell's Delete could not be taken out of the menu: {e}");
        }
    }
    let lines: Vec<bool> = (0..count - gone.len() as u32).map(|at| is_separator(menu, at)).collect();
    for at in separators_to_remove(&lines).into_iter().rev() {
        // SAFETY: a position of `menu`, removed from the last one back.
        let _ = unsafe { DeleteMenu(menu, at as u32, MF_BYPOSITION) };
    }
    gone.len()
}

/// Builds the shell menu of `path` into `menu` at its top, ids `SHELL_FIRST..=SHELL_LAST`, with the
/// shell's own Delete taken out (`hide_shell_delete`).
fn shell_menu(owner: HWND, path: &Path, menu: HMENU, flags: u32) -> windows::core::Result<(IContextMenu, Pidl)> {
    // The shell extensions can take most of a second here (S9): a wait cursor shows meanwhile.
    // SAFETY: a system cursor; the old one is put back.
    let old = unsafe { LoadCursorW(None, IDC_WAIT) }.ok().map(|wait| unsafe { SetCursor(Some(wait)) });
    let built = (|| {
        let pidl = parse(path)?;
        let mut child = std::ptr::null_mut();
        // SAFETY: `child` points into `pidl`, which lives until the menu is dropped.
        let folder: IShellFolder = unsafe { SHBindToParent(pidl.0, Some(&mut child))? };
        // SAFETY: `child` is the last item of `pidl` (alive here and kept with the menu), relative
        // to `folder` as GetUIObjectOf wants; one item, so the slice length is right.
        let cm: IContextMenu = unsafe { folder.GetUIObjectOf(owner, &[child as *const _], None)? };
        // SAFETY: `menu` is a live popup menu of this thread; the id range is SHELL_FIRST..=SHELL_LAST.
        unsafe { cm.QueryContextMenu(menu, 0, SHELL_FIRST, SHELL_LAST, flags) }.ok()?;
        hide_shell_delete(&cm, menu);
        Ok((cm, pidl))
    })();
    if let Some(old) = old {
        // SAFETY: the cursor that showed before.
        unsafe { SetCursor(Some(old)) };
    }
    built
}

/// True when the line at position `at` of `menu` is a separator.
fn is_separator(menu: HMENU, at: u32) -> bool {
    let mut info = MENUITEMINFOW { cbSize: size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_FTYPE, ..Default::default() };
    // SAFETY: `info` is a MENUITEMINFOW with its size set; only the type is asked for.
    unsafe { GetMenuItemInfoW(menu, at, true, &mut info) }.is_ok() && (info.fType.0 & MFT_SEPARATOR.0) != 0
}

/// Puts the Volatus `item` at position `at` of `menu`.
fn insert(menu: HMENU, at: u32, item: &Item) -> windows::core::Result<()> {
    let (flags, id, text) = match *item {
        Item::Action(cmd, text) => (MF_STRING, cmd as usize, Some(text)),
        Item::Disabled(text) => (MF_STRING | MF_GRAYED, 0, Some(text)),
        Item::Separator => (MF_SEPARATOR, 0, None),
    };
    let text = text.map(HSTRING::from);
    let text = text.as_ref().map_or(PCWSTR::null(), |t| PCWSTR(t.as_ptr()));
    // SAFETY: the text outlives the call; the menu copies it.
    unsafe { InsertMenuW(menu, at, MF_BYPOSITION | flags, id, text) }
}

/// After a menu: the window gets the pointer where it is now (S8) and the keyboard focus.
fn after_menu(window: HWND) {
    let mut p = POINT::default();
    // SAFETY: own window, a local point.
    unsafe {
        if GetCursorPos(&mut p).is_ok() && ScreenToClient(window, &mut p).as_bool() {
            // MAKELPARAM: the low words of x and y (negative outside the window, as Windows sends them).
            let at = ((p.y as u16 as isize) << 16) | p.x as u16 as isize;
            SendMessageW(window, WM_MOUSEMOVE, Some(WPARAM(0)), Some(LPARAM(at)));
        }
        if GetFocus() != window {
            let _ = SetFocus(Some(window));
        }
    }
}

/// Runs the Volatus item `cmd` for the item at `path` (`is_dir`: a folder). Explore Folder opens
/// a file's folder with the file selected (`SHOpenFolderAndSelectItems`, D5) and a folder itself;
/// Command Prompt Here starts `cmd.exe` in the folder `path` (`menu::terminal_folder`), with no
/// path on a command line (D4); Copy Path puts the raw path on the clipboard. `Err` with what
/// failed; Windows shows no error box of its own.
pub fn act(cmd: Cmd, path: &Path, is_dir: bool) -> Result<(), String> {
    let failed = |what: &str, e: &dyn std::fmt::Display| format!("{what} {}: {e}", path.display());
    if matches!(cmd, Cmd::ExploreFolder | Cmd::CommandPrompt) && !com_ready() {
        return Err(failed("No COM for", &"CoInitializeEx failed"));
    }
    match cmd {
        Cmd::ExploreFolder => {
            let pidl = parse(path).map_err(|e| failed("Windows cannot find", &e))?;
            let opened = if is_dir {
                execute(SHELLEXECUTEINFOW { fMask: SEE_MASK_IDLIST | SEE_MASK_FLAG_NO_UI, lpVerb: w!("explore"), lpIDList: pidl.0.cast(), ..Default::default() })
            } else {
                // SAFETY: a full id list that lives for the call.
                opened(unsafe { SHOpenFolderAndSelectItems(pidl.0, None, 0) })
            };
            opened.map_err(|e| failed("Explorer cannot open", &e))
        }
        Cmd::CommandPrompt => {
            let folder = HSTRING::from(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).as_os_str());
            let info = SHELLEXECUTEINFOW {
                fMask: SEE_MASK_FLAG_NO_UI,
                lpVerb: w!("open"),
                lpFile: w!("cmd.exe"),
                lpDirectory: PCWSTR(folder.as_ptr()),
                ..Default::default()
            };
            execute(info).map_err(|e| failed("A command prompt cannot start in", &e))
        }
        Cmd::CopyPath => {
            let copied = arboard::Clipboard::new().and_then(|mut c| c.set_text(clip_text(path)));
            copied.map_err(|e| failed("The clipboard cannot take", &e))
        }
        Cmd::ShowFolder | Cmd::Rescan | Cmd::Delete => Ok(()),
    }
}

/// Explore Folder (`act`) for the item at `path` on a new thread with its own COM (STA), so a file
/// manager that blocks `SHOpenFolderAndSelectItems` does not block the window. `done` gets the
/// result on that thread. Returns at once.
pub fn explore_on_worker(path: PathBuf, is_dir: bool, done: impl FnOnce(Result<(), String>) + Send + 'static) {
    std::thread::spawn(move || {
        // `act` starts COM on this thread (`com_ready`) and parses `path` here: an id list is not sent.
        let result = act(Cmd::ExploreFolder, &path, is_dir);
        if COM.get() == Some(true) {
            // SAFETY: pairs the `CoInitializeEx` of `com_ready` on this thread, which ends here; the
            // id list was freed when `act` returned.
            unsafe { CoUninitialize() };
        }
        done(result);
    });
}

/// The text Copy Path puts on the clipboard for `path`: the absolute path, as Explore Folder and
/// Command Prompt Here use it (a scan root typed as a relative path stays relative in the tree).
fn clip_text(path: &Path) -> String {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned()
}

/// `ShellExecuteExW` of `info`, shown as a normal window.
fn execute(mut info: SHELLEXECUTEINFOW) -> windows::core::Result<()> {
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.nShow = SW_SHOWNORMAL.0;
    // SAFETY: every string and the id list in `info` outlive the call.
    unsafe { ShellExecuteExW(&mut info) }
}

/// The result of `SHOpenFolderAndSelectItems` as Explore Folder reads it. `E_ABORT` is opened: with
/// another file manager as the handler of folders (OneCommander on the owner's machine, CM-P4 live
/// check), the folder opens in it and the call gives `E_ABORT`, as it cannot select the file there.
/// This can hide a real failure that also gives `E_ABORT`: then no note shows although nothing opened.
fn opened(result: windows::core::Result<()>) -> windows::core::Result<()> {
    match result {
        Err(e) if e.code() == E_ABORT => Ok(()),
        other => other,
    }
}

/// The HWND of the Slint window, once it exists.
pub fn window_hwnd(window: &slint::Window) -> Option<HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(w) => Some(HWND(w.hwnd.get() as *mut _)),
        _ => None,
    }
}

/// The screen point of the top-left corner of the client area of `window`, in physical pixels.
pub fn client_origin(window: HWND) -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: own window, a local point.
    let _ = unsafe { ClientToScreen(window, &mut p) };
    (p.x, p.y)
}

/// The key `vk` is down now (the state of this thread's input).
pub fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: no pointers.
    unsafe { GetKeyState(i32::from(vk.0)) < 0 }
}

/// The owner of the menus of `window`: the window itself, subclassed, made once. If the subclass
/// cannot be set, a hidden helper window; if that fails too, the window alone (then submenus as
/// Open with can stay empty).
pub fn menu_owner(window: HWND) -> HWND {
    if let Some(owner) = OWNER.get() {
        return owner;
    }
    // SAFETY: own window and a proc that lives for the whole process.
    let owner = if unsafe { SetWindowSubclass(window, Some(subclass), SUBCLASS_ID, 0) }.as_bool() {
        window
    } else {
        eprintln!("The shell menu cannot watch the window: it uses a helper window.");
        helper(window).unwrap_or_else(|e| {
            eprintln!("The shell menu has no helper window ({e}): some submenus can be empty.");
            window
        })
    };
    OWNER.set(Some(owner));
    owner
}

/// A hidden window owned by `window` that passes the menu messages to the shell menu.
fn helper(window: HWND) -> windows::core::Result<HWND> {
    // SAFETY: own window; the class and the proc live for the whole process.
    unsafe {
        let instance = HINSTANCE(GetWindowLongPtrW(window, GWLP_HINSTANCE) as *mut _);
        let class = WNDCLASSW { lpfnWndProc: Some(helper_proc), hInstance: instance, lpszClassName: HELPER_CLASS, ..Default::default() };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        CreateWindowExW(WINDOW_EX_STYLE(0), HELPER_CLASS, PCWSTR::null(), WS_POPUP, 0, 0, 0, 0, Some(window), None, Some(instance), None)
    }
}

/// The answer of the shell menu that shows to a menu message, or `None` when it is not one or the
/// shell did not take it.
fn menu_message(msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if !matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
        return None;
    }
    // Cloned: no borrow is held while the shell runs.
    let handlers = ACTIVE.try_with(|a| a.borrow().clone()).ok().flatten()?;
    if let Some(cm3) = handlers.cm3 {
        let mut r = LRESULT(0);
        // SAFETY: the message as the window got it.
        return unsafe { cm3.HandleMenuMsg2(msg, wp, lp, Some(&mut r)) }.is_ok().then_some(r);
    }
    // SAFETY: the message as the window got it.
    unsafe { handlers.cm2?.HandleMenuMsg(msg, wp, lp) }.is_ok().then_some(LRESULT(if msg == WM_INITMENUPOPUP { 0 } else { 1 }))
}

/// The subclass of the Slint window. `WM_CONTEXTMENU` passes on untouched (see the module doc).
unsafe extern "system" fn subclass(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, id: usize, _data: usize) -> LRESULT {
    if let Some(r) = menu_message(msg, wp, lp) {
        return r;
    }
    if msg == WM_NCDESTROY {
        // SAFETY: the subclass set by `menu_owner`, removed as the window goes.
        let _ = unsafe { RemoveWindowSubclass(h, Some(subclass), id) };
        let _ = OWNER.try_with(|o| o.set(None));
    }
    // SAFETY: the message as the window got it.
    unsafe { DefSubclassProc(h, msg, wp, lp) }
}

/// The window proc of the helper owner window.
unsafe extern "system" fn helper_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: the message as the window got it.
    menu_message(msg, wp, lp).unwrap_or_else(|| unsafe { DefWindowProcW(h, msg, wp, lp) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::query_flags;
    use std::fs;

    /// A new empty folder under the target folder of the worktree, removed when dropped. The shell
    /// builds menus only for items made here: no verb runs.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/shell-tests").join(format!("{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The verbs of the shell menu of `path`, with Shift or not.
    fn verbs_of(path: &Path, shift: bool) -> Vec<Option<String>> {
        let popup = Popup::new(HWND::default(), Some(path), query_flags(shift), |_| Vec::new()).unwrap();
        popup.verbs()
    }

    fn has(verbs: &[Option<String>], verb: &str) -> bool {
        verbs.iter().flatten().any(|v| v.eq_ignore_ascii_case(verb))
    }

    #[test]
    fn popup_of_a_file_has_properties_and_no_delete_of_the_shell_and_no_rename() {
        let dir = Scratch::new("file");
        let file = dir.0.join("f.txt");
        fs::write(&file, b"hello").unwrap();

        let verbs = verbs_of(&file, false);

        assert_eq!([has(&verbs, "delete"), has(&verbs, "properties"), has(&verbs, "rename")], [false, true, false], "{verbs:?}");
    }

    #[test]
    fn popup_of_a_folder_has_no_delete_of_the_shell_with_or_without_shift() {
        let dir = Scratch::new("folder");
        let folder = dir.0.join("sub");
        fs::create_dir(&folder).unwrap();

        let (plain, extended) = (verbs_of(&folder, false), verbs_of(&folder, true));

        assert!(!has(&plain, "delete") && !has(&extended, "delete"), "plain {plain:?}, shift {extended:?}");
        assert!(has(&plain, "properties"), "{plain:?}");
    }

    #[test]
    fn popup_shell_part_has_no_separator_at_an_edge_or_doubled() {
        let dir = Scratch::new("separators");
        let file = dir.0.join("f.txt");
        fs::write(&file, b"hello").unwrap();
        let folder = dir.0.join("sub");
        fs::create_dir(&folder).unwrap();

        for (path, shift) in [(&file, false), (&file, true), (&folder, false), (&folder, true)] {
            let popup = Popup::new(HWND::default(), Some(path), query_flags(shift), |_| Vec::new()).unwrap();
            let lines: String = popup.separator_lines().iter().map(|&sep| if sep { '-' } else { 'x' }).collect();

            assert!(lines.contains('-'), "{path:?} shift {shift}: the shell part has separators to check: {lines}");
            assert!(!lines.starts_with('-') && !lines.ends_with('-') && !lines.contains("--"), "{path:?} shift {shift}: {lines}");
        }
    }

    #[test]
    fn opened_takes_an_abort_as_opened_by_another_file_manager() {
        use windows::Win32::Foundation::E_ABORT;

        let got = [opened(Ok(())), opened(Err(E_ABORT.into())), opened(Err(E_FAIL.into()))].map(|r| r.is_ok());

        assert_eq!(got, [true, true, false]);
    }

    #[test]
    fn act_explore_folder_of_a_missing_file_or_folder_fails() {
        let dir = Scratch::new("explore-missing");

        let got = [act(Cmd::ExploreFolder, &dir.0.join("gone.txt"), false), act(Cmd::ExploreFolder, &dir.0.join("gone"), true)];

        assert!(got.iter().all(Result::is_err), "{got:?}");
    }

    #[test]
    fn parse_of_a_trailing_dot_name_never_names_its_twin() {
        use windows::Win32::UI::Shell::SHGetPathFromIDListW;
        let dir = Scratch::new("proof-dot");
        let abs = std::path::absolute(&dir.0).unwrap();
        let verbatim = |name: &str| PathBuf::from(format!(r"\\?\{}\{name}", abs.display()));
        fs::write(verbatim("a."), b"hello").unwrap();
        fs::write(verbatim("a"), b"abc").unwrap();
        assert!(com_ready());

        let parsed = parse(&abs.join("a."));

        // Refused is safe; an id list must name "a." itself, never its twin "a".
        let got = parsed.map(|pidl| {
            let mut buf = [0u16; 260];
            // SAFETY: a full id list and a MAX_PATH buffer.
            assert!(unsafe { SHGetPathFromIDListW(pidl.0, &mut buf) }.as_bool());
            String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap()])
        });
        let _ = fs::remove_file(verbatim("a."));
        if let Ok(got) = got {
            assert_eq!(got, abs.join("a.").display().to_string());
        }
    }

    #[test]
    fn explore_on_worker_parses_the_path_on_another_thread_and_gives_its_error() {
        let dir = Scratch::new("explore-worker");
        let missing = dir.0.join("gone.txt");
        let (tx, rx) = std::sync::mpsc::channel();

        explore_on_worker(missing.clone(), false, move |result| tx.send((std::thread::current().id(), result)).unwrap());

        let (thread, result) = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        assert_ne!(thread, std::thread::current().id());
        let error = result.unwrap_err();
        assert!(error.contains(&missing.display().to_string()), "{error}");
    }

    #[test]
    fn clip_text_of_a_path_under_a_relative_root_is_absolute() {
        let got = clip_text(Path::new("scan\\sub\\f.txt"));

        assert_eq!(got, std::env::current_dir().unwrap().join("scan\\sub\\f.txt").display().to_string());
    }

    #[test]
    fn popup_of_a_folder_with_shift_has_at_least_the_items_without() {
        let dir = Scratch::new("folder-shift");
        let folder = dir.0.join("sub");
        fs::create_dir(&folder).unwrap();

        let (plain, extended) = (verbs_of(&folder, false), verbs_of(&folder, true));

        assert!(extended.len() >= plain.len() && !plain.is_empty(), "plain {plain:?}, shift {extended:?}");
    }
}

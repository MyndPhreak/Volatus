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

use crate::menu::{Anchor, Item, SHELL_FIRST, SHELL_LAST};
use std::cell::{Cell, RefCell};
use std::path::Path;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, GetKeyState, SetFocus, VIRTUAL_KEY};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    DefSubclassProc, GCS_VERBW, IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, RemoveWindowSubclass, SHBindToParent,
    SHParseDisplayName, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GWLP_HINSTANCE, GetCursorPos, GetWindowLongPtrW,
    HMENU, IDC_WAIT, InsertMenuW, LoadCursorW, MF_BYPOSITION, MF_GRAYED, MF_SEPARATOR, MF_STRING, RegisterClassW,
    SendMessageW, SetCursor, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WINDOW_EX_STYLE, WM_DRAWITEM, WM_INITMENUPOPUP,
    WM_MEASUREITEM, WM_MENUCHAR, WM_MOUSEMOVE, WM_NCDESTROY, WNDCLASSW, WS_POPUP,
};
use windows::core::{HSTRING, Interface, PCWSTR, PSTR, w};

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
/// for the disk check (`menu::read_disk`), so both name the same item.
fn parse(path: &Path) -> windows::core::Result<Pidl> {
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
        use crate::menu::{Picked, picked};
        use windows::Win32::UI::WindowsAndMessaging::{GetMenuItemCount, GetMenuItemID};
        // SAFETY: the menu lives as long as `self`.
        let count = unsafe { GetMenuItemCount(Some(self.menu)) };
        (0..count)
            .filter_map(|i| match picked(unsafe { GetMenuItemID(self.menu, i) }) {
                Picked::Shell(offset) => Some(self.verb(offset)),
                _ => None,
            })
            .collect()
    }

    /// The canonical verb of the shell item at `offset` from `SHELL_FIRST` (`GCS_VERBW`), or `None`
    /// when the shell gives none (an error or an empty verb).
    #[cfg_attr(not(test), expect(dead_code, reason = "P4.T3 reads the verb of the picked item"))]
    pub fn verb(&self, offset: u32) -> Option<String> {
        let cm = self.shell.as_ref()?;
        let mut buf = [0u16; 260];
        // SAFETY: the buffer holds `buf.len()` UTF-16 units, as GCS_VERBW writes.
        unsafe { cm.GetCommandString(offset as usize, GCS_VERBW, None, PSTR(buf.as_mut_ptr().cast()), buf.len() as u32) }.ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        (len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
    }

    /// Shows the menu at `at` until the user picks an item or closes it, owned by `owner` (from
    /// `menu_owner`). Returns the id picked, 0 for none. Afterwards `window` (the Slint window) gets
    /// the pointer where it is now and the keyboard focus.
    pub fn track(&self, owner: HWND, window: HWND, at: Anchor) -> u32 {
        let handlers = self.shell.as_ref().map(|cm| Handlers { cm2: cm.cast().ok(), cm3: cm.cast().ok() });
        ACTIVE.with_borrow_mut(|a| *a = handlers);
        let Anchor::Point(x, y) = at;
        // SAFETY: the menu and the owner live for the call.
        let id = unsafe { TrackPopupMenuEx(self.menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, x, y, owner, None) }.0 as u32;
        ACTIVE.with_borrow_mut(|a| *a = None);
        after_menu(window);
        id
    }
}

/// Builds the shell menu of `path` into `menu` at its top, ids `SHELL_FIRST..=SHELL_LAST`.
fn shell_menu(owner: HWND, path: &Path, menu: HMENU, flags: u32) -> windows::core::Result<(IContextMenu, Pidl)> {
    // The shell extensions can take most of a second here (S9): a wait cursor shows meanwhile.
    // SAFETY: a system cursor; the old one is put back.
    let old = unsafe { LoadCursorW(None, IDC_WAIT) }.ok().map(|wait| unsafe { SetCursor(Some(wait)) });
    let built = (|| {
        let pidl = parse(path)?;
        let mut child = std::ptr::null_mut();
        // SAFETY: `child` points into `pidl`, which lives until the menu is dropped.
        let folder: IShellFolder = unsafe { SHBindToParent(pidl.0, Some(&mut child))? };
        let cm: IContextMenu = unsafe { folder.GetUIObjectOf(owner, &[child as *const _], None)? };
        unsafe { cm.QueryContextMenu(menu, 0, SHELL_FIRST, SHELL_LAST, flags) }.ok()?;
        Ok((cm, pidl))
    })();
    if let Some(old) = old {
        // SAFETY: the cursor that showed before.
        unsafe { SetCursor(Some(old)) };
    }
    built
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
    use std::path::PathBuf;

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
    fn popup_of_a_file_has_delete_and_properties_and_no_rename() {
        let dir = Scratch::new("file");
        let file = dir.0.join("f.txt");
        fs::write(&file, b"hello").unwrap();

        let verbs = verbs_of(&file, false);

        assert_eq!([has(&verbs, "delete"), has(&verbs, "properties"), has(&verbs, "rename")], [true, true, false], "{verbs:?}");
    }

    #[test]
    fn popup_of_a_folder_has_delete() {
        let dir = Scratch::new("folder");
        let folder = dir.0.join("sub");
        fs::create_dir(&folder).unwrap();

        let verbs = verbs_of(&folder, false);

        assert!(has(&verbs, "delete"), "{verbs:?}");
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

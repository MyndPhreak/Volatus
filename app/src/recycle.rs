//! Send to the Recycle Bin: one `IFileOperation` delete with `FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE`
//! on the one checked path, watched by a progress sink. A folder goes into the bin as one item.
//! The shell calls the sink's `PreDeleteItem` before it deletes an item, with flags that say if
//! the item can go to the bin; when it cannot, the sink returns `E_ABORT` and nothing is deleted.
//! The item counts as recycled only when `PostDeleteItem` reports a success and the new item in
//! the bin. So plain Delete never becomes a permanent delete: an item the bin cannot take is one
//! skip whose text offers the permanent delete, and the user chooses. The shell takes only a
//! plain (`C:\...`) path, so the verbatim (`\\?\`) path from `check_target` is made plain only
//! where the plain form names the same item.

use crate::delete::{DeleteTarget, Outcome, Remover, SkipReason, Skipped, reason_from_win32};
use std::sync::atomic::AtomicU64;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{E_ABORT, HWND, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::{
    COPYENGINE_E_CANCELLED, COPYENGINE_E_RECYCLE_BIN_NOT_FOUND, COPYENGINE_E_RECYCLE_FORCE_NUKE,
    COPYENGINE_E_RECYCLE_PATH_TOO_LONG, COPYENGINE_E_RECYCLE_SIZE_TOO_BIG, COPYENGINE_E_RECYCLE_UNKNOWN_ERROR,
    COPYENGINE_E_USER_CANCELLED, FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
    FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl,
    IShellItem, SHCreateItemFromParsingName, TSF_DELETE_RECYCLE_IF_POSSIBLE,
};
use windows_core::{ComObject, HRESULT, PCWSTR, Ref};

/// The Recycle Bin delete. `owner` is the window handle (`HWND` as `isize`) that the shell's own
/// windows, if it shows any, belong to.
#[derive(Debug, Default, Clone, Copy)]
pub struct Recycle {
    owner: Option<isize>,
}

impl Recycle {
    pub fn new(owner: Option<isize>) -> Recycle {
        Recycle { owner }
    }
}

/// `SkipReason::Other` code for a path the shell cannot take as the same item (see `shell_path`).
/// Not a shell, Win32 or HRESULT code that this engine sees.
pub const PATH_REFUSED: i32 = i32::MIN;

/// `SkipReason::Other` code for a cancelled or aborted operation.
pub const ABORTED: i32 = i32::MIN + 1;

/// `SkipReason::Other` code for an item the shell would delete permanently (the sink refused it).
pub const NOT_RECYCLABLE: i32 = i32::MIN + 2;

/// `SkipReason::Other` code for an operation that reported success but did not report the item
/// in the bin. The item may or may not still be in its place.
pub const NOT_IN_BIN: i32 = i32::MIN + 3;

/// `SkipReason::Other` code for an item the sink saw go to the bin, after which the operation
/// failed or was aborted. The item is in the bin: no permanent delete is offered.
pub const IN_BIN_THEN_ERROR: i32 = i32::MIN + 4;

/// The legacy `DE_*` codes of the shell's copy engine (from the `SHFileOperation` documentation;
/// shellapi.h does not define them). The ones not named here map through `reason_from_win32` to
/// `Other(code)`, as they collide with none of its special codes.
const DE_OPCANCELLED: i32 = 0x75;
const DE_ACCESSDENIEDSRC: i32 = 0x78;
const DE_PATHTOODEEP: i32 = 0x79;
const DE_INVALIDFILES: i32 = 0x7C;
const DE_FILENAMETOOLONG: i32 = 0x81;
const DE_FILE_TOO_LARGE: i32 = 0x85;

/// Win32 `ERROR_CANCELLED`.
const ERROR_CANCELLED: i32 = 1223;

const HR_E_ABORT: i32 = E_ABORT.0;
const HR_USER_CANCELLED: i32 = COPYENGINE_E_USER_CANCELLED.0;
const HR_CANCELLED: i32 = COPYENGINE_E_CANCELLED.0;
const HR_RECYCLE_UNKNOWN: i32 = COPYENGINE_E_RECYCLE_UNKNOWN_ERROR.0;
const HR_RECYCLE_FORCE_NUKE: i32 = COPYENGINE_E_RECYCLE_FORCE_NUKE.0;
const HR_RECYCLE_SIZE_TOO_BIG: i32 = COPYENGINE_E_RECYCLE_SIZE_TOO_BIG.0;
const HR_RECYCLE_PATH_TOO_LONG: i32 = COPYENGINE_E_RECYCLE_PATH_TOO_LONG.0;
const HR_RECYCLE_BIN_NOT_FOUND: i32 = COPYENGINE_E_RECYCLE_BIN_NOT_FOUND.0;

/// Longest plain path (in UTF-16 units, no NUL) the shell is given: `MAX_PATH` less the NUL.
const MAX_PLAIN: usize = 259;

/// `FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE` make the delete a recycle. `FOF_NOCONFIRMATION`,
/// `FOF_SILENT` and `FOF_NOERRORUI` turn every shell window off. `FOF_NOCONFIRMATION` would also
/// say yes to "delete permanently?" for an item the bin cannot take: the sink's `PreDeleteItem`
/// refuses that item first, so no flag here can delete it.
const FLAGS: FILEOPERATION_FLAGS = FILEOPERATION_FLAGS(
    FOF_ALLOWUNDO.0 | FOFX_RECYCLEONDELETE.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0,
);

impl Remover for Recycle {
    /// One shell operation for the whole target. There is no progress during it: `progress(0, 0)`
    /// is called before it and `progress(1, t.size)` after a success. `stop` is checked only
    /// before the operation (Recycle Bin mode has no Stop button); when it is set nothing is done.
    fn remove(&self, t: &DeleteTarget, progress: &dyn Fn(u64, u64), stop: &AtomicBool) -> Outcome {
        let mut out = Outcome::default();
        if stop.load(Ordering::Relaxed) {
            out.stopped = true;
            return out;
        }
        let skip = |reason| Skipped { path: t.path.clone(), reason };
        let Some(from) = shell_path(&t.path) else {
            out.skipped.push(skip(SkipReason::Other(PATH_REFUSED)));
            return out;
        };
        progress(0, 0);
        let r = shell_recycle(&from, self.owner);
        match skip_for_result(r.hr, r.aborted, r.seen) {
            None => {
                out.removed_items = 1;
                out.removed_bytes = t.size;
                progress(1, t.size);
            }
            Some(reason) => out.skipped.push(skip(reason)),
        }
        out
    }
}

/// The plain, double-NUL-terminated path for a verbatim drive path, or `None` when the plain
/// form could name a different item or more than one. Win32 drops a trailing dot or space from a
/// name (and reaches a sibling), reads a `:` as a stream, reads a device name (`con`, `aux.txt`)
/// as a device, and the shell expands `*` and `?` (and `<`, `>`, `"` are wildcards to
/// `FindFirstFile`). So `None` for: a path that is not `\\?\<letter>:\<names>` (a plain path
/// too: this engine takes only `check_target`'s output, and the text of a plain path may already
/// have lost its trailing dots); a drive root; a plain form longer than `MAX_PLAIN`; and a name
/// that is empty, `.` or `..`, ends in a dot or space, holds a control character or one of
/// `: * ? < > " | /`, is not valid UTF-16, or is a reserved device name. The plain form is the
/// verbatim text with the `\\?\` taken off and nothing else changed.
pub(crate) fn shell_path(verbatim: &Path) -> Option<Vec<u16>> {
    let (colon, back) = (u16::from(b':'), u16::from(b'\\'));
    let w: Vec<u16> = verbatim.as_os_str().encode_wide().collect();
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    let plain = w.strip_prefix(prefix.as_slice())?;
    let [letter, c, b, names @ ..] = plain else { return None };
    if !(u8::try_from(*letter).is_ok_and(|l| l.is_ascii_alphabetic()) && *c == colon && *b == back) {
        return None;
    }
    if names.is_empty() || plain.len() > MAX_PLAIN || !names.split(|c| *c == back).all(plain_name) {
        return None;
    }
    Some(plain.iter().copied().chain([0, 0]).collect())
}

/// True when Win32 reads `name` (one path component) as exactly this name. See `shell_path`.
fn plain_name(name: &[u16]) -> bool {
    let Ok(s) = String::from_utf16(name) else { return false };
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.ends_with(['.', ' '])
        && !s.chars().any(|c| c < ' ' || r#":*?<>"|/"#.contains(c))
        && !is_device(&s)
}

/// A reserved device name: the part before the first dot, trailing spaces dropped, is `con`,
/// `prn`, `aux`, `nul`, `conin$`, `conout$`, or `com`/`lpt` with one digit (`0`-`9`, `¹²³`).
/// Any case. Newer Windows reads fewer of these as devices; all are refused here.
fn is_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end_matches(' ').to_lowercase();
    let numbered = stem
        .strip_prefix("com")
        .or_else(|| stem.strip_prefix("lpt"))
        .is_some_and(|d| d.chars().count() == 1 && "0123456789¹²³".contains(d));
    numbered || ["con", "prn", "aux", "nul", "conin$", "conout$"].contains(&stem.as_str())
}

/// What `PreDeleteItem` does with an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreDelete {
    Allow,
    Abort,
}

/// What `PostDeleteItem` says happened to an item. `NotRecycled` keeps the `hrDelete` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostDelete {
    Recycled,
    NotRecycled(i32),
}

/// What the sink saw over the whole operation: the worst of its calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seen {
    Nothing,
    Post(PostDelete),
    /// `PreDeleteItem` refused an item the bin cannot take.
    Refused,
}

/// Allow only an item the shell will send to the bin (`TSF_DELETE_RECYCLE_IF_POSSIBLE` set).
/// Without it the shell would delete the item permanently.
pub(crate) fn pre_delete_decision(tsf_flags: u32) -> PreDelete {
    if tsf_flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 != 0 { PreDelete::Allow } else { PreDelete::Abort }
}

/// Recycled only for a success `hrDelete` with a new item in the bin (`psiNewlyCreated`).
pub(crate) fn post_delete_decision(hr: i32, newly_created_is_null: bool) -> PostDelete {
    if hr >= 0 && !newly_created_is_null { PostDelete::Recycled } else { PostDelete::NotRecycled(hr) }
}

/// The worse of two sink observations: `Refused` over `NotRecycled` over `Recycled` over
/// `Nothing`, so a later call never hides an earlier failure.
fn worse(a: Seen, b: Seen) -> Seen {
    let rank = |s: Seen| match s {
        Seen::Nothing => 0,
        Seen::Post(PostDelete::Recycled) => 1,
        Seen::Post(PostDelete::NotRecycled(_)) => 2,
        Seen::Refused => 3,
    };
    if rank(b) > rank(a) { b } else { a }
}

/// `Seen` as one `u64` for the sink's atomic state: the kind in the high 32 bits (0 Nothing,
/// 1 Recycled, 2 NotRecycled, 3 Refused), the `NotRecycled` code in the low 32 bits.
fn encode(s: Seen) -> u64 {
    let (kind, code) = match s {
        Seen::Nothing => (0u64, 0),
        Seen::Post(PostDelete::Recycled) => (1, 0),
        Seen::Post(PostDelete::NotRecycled(code)) => (2, code),
        Seen::Refused => (3, 0),
    };
    kind << 32 | u64::from(code as u32)
}

/// The `Seen` that `encode` gave `v`. An unknown kind decodes as `Refused`: fail closed.
fn decode(v: u64) -> Seen {
    match v >> 32 {
        0 => Seen::Nothing,
        1 => Seen::Post(PostDelete::Recycled),
        2 => Seen::Post(PostDelete::NotRecycled(v as u32 as i32)),
        _ => Seen::Refused,
    }
}

/// What one operation means for the target: `None` only when the sink saw the item recycled,
/// `hr` is 0 and nothing aborted. The sink's evidence comes first, as it says what happened to
/// the item itself:
///
/// | Sink saw                         | Result                                             |
/// |----------------------------------|----------------------------------------------------|
/// | `Refused`                        | `Other(NOT_RECYCLABLE)`                            |
/// | `NotRecycled`, failure code      | that code, by `skip_for_shell_result`              |
/// | `NotRecycled`, success code      | `Other(NOT_IN_BIN)` (the item may be gone)         |
/// | `Recycled`                       | success if `hr` is 0 and not aborted, else `Other(IN_BIN_THEN_ERROR)` |
/// | nothing                          | `hr` by `skip_for_shell_result`; 0 with aborted is `Other(ABORTED)`; 0 is `Other(NOT_IN_BIN)` |
pub(crate) fn skip_for_result(hr: i32, aborted: bool, seen: Seen) -> Option<SkipReason> {
    let other = |code| Some(SkipReason::Other(code));
    match seen {
        Seen::Refused => other(NOT_RECYCLABLE),
        Seen::Post(PostDelete::NotRecycled(code)) if code < 0 => skip_for_shell_result(code, aborted),
        Seen::Post(PostDelete::NotRecycled(_)) => other(NOT_IN_BIN),
        Seen::Post(PostDelete::Recycled) if hr == 0 && !aborted => None,
        Seen::Post(PostDelete::Recycled) => other(IN_BIN_THEN_ERROR),
        Seen::Nothing if hr != 0 => skip_for_shell_result(hr, aborted),
        Seen::Nothing if aborted => other(ABORTED),
        Seen::Nothing => other(NOT_IN_BIN),
    }
}

/// What a code and the aborted flag mean: `None` for a success (0, not aborted), else the one
/// skip reason. A non-zero code or the aborted flag is never a success.
///
/// | Result                                                  | Reason                    |
/// |---------------------------------------------------------|---------------------------|
/// | 0, not aborted                                          | success (`None`)          |
/// | 0, aborted                                              | `Other(ABORTED)`          |
/// | a cancel: `ERROR_CANCELLED` 1223 (or its HRESULT 0x800704C7), `DE_OPCANCELLED` 0x75, `E_ABORT`, `COPYENGINE_E_USER_CANCELLED`, `COPYENGINE_E_CANCELLED` | `Other(ABORTED)` |
/// | an HRESULT 0x8007xxxx (a Win32 code)                    | as the Win32 code xxxx    |
/// | `DE_ACCESSDENIEDSRC` (0x78)                             | `AccessDenied`            |
/// | `DE_PATHTOODEEP` (0x79), `DE_FILENAMETOOLONG` (0x81)    | `TooLong`                 |
/// | any other code (`DE_*`, Win32, other HRESULT)           | `reason_from_win32(code)` |
///
/// `reason_from_win32` gives `Locked`, `AccessDenied` and `TooLong` for the plain Win32 codes and
/// `Other(code)` for the rest (`DE_FILE_TOO_LARGE`, `DE_INVALIDFILES`, the
/// `COPYENGINE_E_RECYCLE_*` codes, ...). `skip_text` has the words for the codes that offer the
/// permanent delete.
pub(crate) fn skip_for_shell_result(code: i32, aborted: bool) -> Option<SkipReason> {
    // An HRESULT that carries a Win32 code (`HRESULT_FROM_WIN32`): the Win32 code decides.
    if code as u32 & 0xFFFF_0000 == 0x8007_0000 {
        return Some(win32_reason(code & 0xFFFF));
    }
    match code {
        0 if !aborted => None,
        0 | DE_OPCANCELLED | HR_E_ABORT | HR_USER_CANCELLED | HR_CANCELLED => Some(SkipReason::Other(ABORTED)),
        DE_ACCESSDENIEDSRC => Some(SkipReason::AccessDenied),
        DE_PATHTOODEEP | DE_FILENAMETOOLONG => Some(SkipReason::TooLong),
        code => Some(win32_reason(code)),
    }
}

/// `reason_from_win32`, with `ERROR_CANCELLED` as the aborted reason.
fn win32_reason(code: i32) -> SkipReason {
    if code == ERROR_CANCELLED { SkipReason::Other(ABORTED) } else { reason_from_win32(code) }
}

/// The words for a Recycle Bin skip (ASD-STE100), or `None` for a reason that has the general
/// text. The ones for an item the bin did not take offer the permanent delete; there is no
/// automatic fallback: the user chooses.
pub fn skip_text(r: SkipReason) -> Option<&'static str> {
    match r {
        SkipReason::Other(PATH_REFUSED | DE_INVALIDFILES) => {
            Some("The Recycle Bin cannot take this path. You can delete the item permanently.")
        }
        SkipReason::Other(DE_FILE_TOO_LARGE | HR_RECYCLE_SIZE_TOO_BIG) => {
            Some("The item is too large for the Recycle Bin. You can delete the item permanently.")
        }
        SkipReason::Other(
            NOT_RECYCLABLE | HR_RECYCLE_FORCE_NUKE | HR_RECYCLE_UNKNOWN | HR_RECYCLE_PATH_TOO_LONG
            | HR_RECYCLE_BIN_NOT_FOUND,
        ) => Some("The Recycle Bin cannot take this item. You can delete the item permanently."),
        SkipReason::Other(ABORTED) => {
            Some("Windows did not move the item to the Recycle Bin. You can delete the item permanently.")
        }
        SkipReason::Other(IN_BIN_THEN_ERROR) => {
            Some("The item is in the Recycle Bin. Then Windows reported an error. Look in the Recycle Bin.")
        }
        SkipReason::Other(NOT_IN_BIN) => {
            Some("Windows did not show the item in the Recycle Bin. Scan the folder again to see the item's state.")
        }
        _ => None,
    }
}

/// True for a skip where the Recycle Bin did not take the item and nothing on the disk changed:
/// the shell never got the path (`PATH_REFUSED`), the sink refused the item before any delete
/// (`NOT_RECYCLABLE`), or the bin said the item is too large.
pub fn left_in_place(r: SkipReason) -> bool {
    let _ = (r, PATH_REFUSED, NOT_RECYCLABLE, DE_FILE_TOO_LARGE, HR_RECYCLE_SIZE_TOO_BIG);
    false
}

/// COM on this thread for one operation: `CoInitializeEx` (apartment-threaded), balanced by
/// `CoUninitialize` on drop only when this guard initialised it. A thread already in the other
/// mode (`RPC_E_CHANGED_MODE`) keeps its COM and is not uninitialised.
struct Com {
    uninit: bool,
}

impl Com {
    fn init() -> Result<Com, i32> {
        // SAFETY: no reserved pointer. A success (S_OK or S_FALSE) is balanced in `drop`.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_ok() {
            Ok(Com { uninit: true })
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(Com { uninit: false })
        } else {
            Err(hr.0)
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: balances the successful `CoInitializeEx` this guard made on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// The progress sink: records what the shell did to the item. Every other call is accepted.
/// The `implement` object is agile (callable from any thread), so the state is one atomic
/// (`encode`d `Seen`), updated with `worse`: sound whatever thread the shell calls it on.
#[windows_core::implement(IFileOperationProgressSink)]
struct Sink {
    seen: AtomicU64,
}

impl Sink {
    fn new() -> ComObject<Sink> {
        ComObject::new(Sink { seen: AtomicU64::new(encode(Seen::Nothing)) })
    }
    fn seen(&self) -> Seen {
        decode(self.seen.load(Ordering::SeqCst))
    }
    fn note(&self, s: Seen) {
        self.seen.update(Ordering::SeqCst, Ordering::SeqCst, |v| encode(worse(decode(v), s)));
    }
}

#[allow(non_snake_case, reason = "the COM interface's method names")]
impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> windows_core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _hr: HRESULT) -> windows_core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _f: u32, _i: Ref<'_, IShellItem>, _n: &PCWSTR) -> windows_core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _f: u32,
        _i: Ref<'_, IShellItem>,
        _n: &PCWSTR,
        _hr: HRESULT,
        _new: Ref<'_, IShellItem>,
    ) -> windows_core::Result<()> {
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _f: u32,
        _i: Ref<'_, IShellItem>,
        _d: Ref<'_, IShellItem>,
        _n: &PCWSTR,
    ) -> windows_core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(
        &self,
        _f: u32,
        _i: Ref<'_, IShellItem>,
        _d: Ref<'_, IShellItem>,
        _n: &PCWSTR,
        _hr: HRESULT,
        _new: Ref<'_, IShellItem>,
    ) -> windows_core::Result<()> {
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _f: u32,
        _i: Ref<'_, IShellItem>,
        _d: Ref<'_, IShellItem>,
        _n: &PCWSTR,
    ) -> windows_core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _f: u32,
        _i: Ref<'_, IShellItem>,
        _d: Ref<'_, IShellItem>,
        _n: &PCWSTR,
        _hr: HRESULT,
        _new: Ref<'_, IShellItem>,
    ) -> windows_core::Result<()> {
        Ok(())
    }
    /// `E_ABORT` for an item the shell would delete permanently: the shell then leaves it.
    fn PreDeleteItem(&self, flags: u32, _i: Ref<'_, IShellItem>) -> windows_core::Result<()> {
        match pre_delete_decision(flags) {
            PreDelete::Allow => Ok(()),
            PreDelete::Abort => {
                self.note(Seen::Refused);
                Err(E_ABORT.into())
            }
        }
    }
    fn PostDeleteItem(
        &self,
        _flags: u32,
        _i: Ref<'_, IShellItem>,
        hr: HRESULT,
        new: Ref<'_, IShellItem>,
    ) -> windows_core::Result<()> {
        self.note(Seen::Post(post_delete_decision(hr.0, new.is_null())));
        Ok(())
    }
    fn PreNewItem(&self, _f: u32, _d: Ref<'_, IShellItem>, _n: &PCWSTR) -> windows_core::Result<()> {
        Ok(())
    }
    fn PostNewItem(
        &self,
        _f: u32,
        _d: Ref<'_, IShellItem>,
        _n: &PCWSTR,
        _t: &PCWSTR,
        _a: u32,
        _hr: HRESULT,
        _new: Ref<'_, IShellItem>,
    ) -> windows_core::Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _total: u32, _done: u32) -> windows_core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows_core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows_core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows_core::Result<()> {
        Ok(())
    }
}

/// What one shell operation reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Report {
    /// The first failing HRESULT of the setup or of `PerformOperations`; 0 for none.
    pub hr: i32,
    pub aborted: bool,
    pub seen: Seen,
}

/// One `IFileOperation` delete of `from` (one plain path, double-NUL terminated), on this thread.
fn shell_recycle(from: &[u16], owner: Option<isize>) -> Report {
    assert!(from.ends_with(&[0, 0]) && !from[..from.len() - 2].contains(&0), "one path, double-NUL terminated");
    let nothing = |hr| Report { hr, aborted: false, seen: Seen::Nothing };
    // Declared first, dropped last: every COM object below is released before COM ends.
    let _com = match Com::init() {
        Ok(c) => c,
        Err(hr) => return nothing(hr),
    };
    let sink = Sink::new();
    let setup = || -> windows_core::Result<IFileOperation> {
        // SAFETY: COM is initialised on this thread for the life of `_com`. `from` is
        // NUL-terminated and outlives the call. The sink is a live COM object; `DeleteItem` keeps
        // its own reference.
        unsafe {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            op.SetOperationFlags(FLAGS)?;
            if let Some(h) = owner {
                op.SetOwnerWindow(HWND(h as *mut core::ffi::c_void))?;
            }
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(from.as_ptr()), None)?;
            op.DeleteItem(&item, &sink.to_interface::<IFileOperationProgressSink>())?;
            Ok(op)
        }
    };
    let op = match setup() {
        Ok(op) => op,
        Err(e) => return nothing(e.code().0),
    };
    // SAFETY: `op` is a live IFileOperation made on this thread, with COM still initialised.
    let performed = unsafe { op.PerformOperations() };
    // An unknown abort state counts as aborted: never a success on a guess.
    // SAFETY: as above.
    let aborted = unsafe { op.GetAnyOperationsAborted() }.map_or(true, |b| b.as_bool());
    drop(op);
    Report { hr: performed.err().map_or(0, |e| e.code().0), aborted, seen: sink.seen() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scan::NodeId;
    use std::fs;
    use std::path::PathBuf;
    use windows::Win32::Foundation::{E_FAIL, S_OK};
    use windows::Win32::System::Com::COINIT_MULTITHREADED;

    /// A unique directory, canonical (verbatim) form. Removed on drop; a link inside is removed
    /// first, so nothing outside is ever followed.
    struct Dir {
        base: PathBuf,
        links: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl Dir {
        fn new(tag: &str) -> Dir {
            let plain = std::env::temp_dir().join(format!("volatus-recycle-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&plain);
            fs::create_dir_all(&plain).expect("temp dir");
            Dir { base: fs::canonicalize(&plain).expect("canonical"), links: Default::default() }
        }
        /// The name an item that goes to the real bin gets, so the owner can find it there.
        fn bin_name(tag: &str) -> String {
            format!("volatus-recycle-test-{}-{tag}", std::process::id())
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            for j in self.links.borrow().iter() {
                let _ = fs::remove_dir(j);
            }
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    /// Writes to the real stderr, which the test harness does not capture, so the skip shows in
    /// every `cargo test` run.
    fn skipped(case: &str) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "SKIPPED real file system recycle test: {case}");
    }

    fn plain(p: &Path) -> PathBuf {
        PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\"))
    }

    fn target(path: &Path, is_dir: bool, size: u64) -> DeleteTarget {
        DeleteTarget { node: NodeId(0), path: path.to_path_buf(), is_dir, is_link: false, size, files: 0, dirs: 0 }
    }

    /// Runs `Recycle` and records every progress call.
    fn run(t: &DeleteTarget, stop: bool) -> (Outcome, Vec<(u64, u64)>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let o = Recycle::default().remove(t, &|i, b| calls.borrow_mut().push((i, b)), &AtomicBool::new(stop));
        (o, calls.into_inner())
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// `want` as the plain, double-NUL-terminated form.
    fn pfrom(want: &str) -> Vec<u16> {
        wide(want).into_iter().chain([0, 0]).collect()
    }

    #[test]
    fn shell_path_table() {
        let long = format!(r"C:\{}", "a".repeat(MAX_PLAIN - 3));
        let too_long = format!(r"C:\{}", "a".repeat(MAX_PLAIN - 2));
        let deep = format!(r"C:\{}\{}\{}", "L".repeat(120), "M".repeat(120), "N".repeat(40));
        let ok: Vec<String> = vec![
            r"C:\Users\dev\data".into(),
            r"c:\a\b.txt".into(),
            r"D:\x".into(),
            r"C:\a.b\c d\.hidden".into(),
            r"C:\café-日😀\x".into(),
            r"C:\console\nul2\com10\comx\lpt\conin".into(),
            long.clone(),
        ];
        for p in &ok {
            let verbatim = format!(r"\\?\{p}");
            // The plain form is the same text with the prefix off: never another item.
            assert_eq!(shell_path(Path::new(&verbatim)), Some(pfrom(p)), "{verbatim}");
        }
        assert_eq!(long.encode_utf16().count(), MAX_PLAIN);
        let refused: Vec<String> = vec![
            // Not verbatim, or not a drive path.
            r"C:\Users\dev\data".into(),
            r"\\?\UNC\server\share\x".into(),
            r"\\server\share\x".into(),
            r"\\.\C:\x".into(),
            r"\\?\Volume{0}\x".into(),
            r"\\?\C:".into(),
            r"\\?\C:\".into(),
            r"\\?\C:x".into(),
            r"\\?\1:\x".into(),
            // Too long for the shell.
            format!(r"\\?\{too_long}"),
            format!(r"\\?\{deep}"),
            // Names Win32 would change or read as something else.
            r"\\?\C:\data\x.".into(),
            r"\\?\C:\pd.\child".into(),
            r"\\?\C:\data\x ".into(),
            r"\\?\C:\x \y".into(),
            r"\\?\C:\data\ff:s".into(),
            r"\\?\C:\data\ff::$DATA".into(),
            r"\\?\C:\data\c:Docs".into(),
            r"\\?\C:\a\.\b".into(),
            r"\\?\C:\a\..\b".into(),
            r"\\?\C:\a\\b".into(),
            r"\\?\C:\a\".into(),
            r"\\?\C:\a/b".into(),
            // Devices.
            r"\\?\C:\data\con".into(),
            r"\\?\C:\data\CON".into(),
            r"\\?\C:\data\nul".into(),
            r"\\?\C:\data\com1".into(),
            r"\\?\C:\data\COM9.log".into(),
            r"\\?\C:\data\com¹".into(),
            r"\\?\C:\data\lpt0".into(),
            r"\\?\C:\data\aux.txt".into(),
            r"\\?\C:\data\prn.tar.gz".into(),
            r"\\?\C:\data\con .txt".into(),
            r"\\?\C:\data\conin$".into(),
            r"\\?\C:\data\CONOUT$".into(),
            r"\\?\C:\nul\x".into(),
            // Wildcards and other characters Win32 does not take in a name.
            r"\\?\C:\data\*".into(),
            r"\\?\C:\data\a?b".into(),
            r"\\?\C:\data\a<b".into(),
            r"\\?\C:\data\a>b".into(),
            r#"\\?\C:\data\a"b"#.into(),
            r"\\?\C:\data\a|b".into(),
            "\\\\?\\C:\\data\\a\tb".into(),
        ];
        for p in &refused {
            assert_eq!(shell_path(Path::new(p)), None, "{p}");
        }
    }

    #[test]
    fn shell_path_refuses_an_unpaired_surrogate() {
        use std::os::windows::ffi::OsStringExt;
        let mut w = wide(r"\\?\C:\data\x");
        w.push(0xD800);
        let p = PathBuf::from(std::ffi::OsString::from_wide(&w));
        assert_eq!(shell_path(&p), None);
    }

    /// `HRESULT_FROM_WIN32`.
    fn hr_win32(code: u32) -> i32 {
        (0x8007_0000 | code) as i32
    }

    #[test]
    fn shell_results_map_to_one_skip_or_success() {
        let aborted = Some(SkipReason::Other(ABORTED));
        for (code, flag, want) in [
            (0, false, None),
            (0, true, aborted),
            (DE_ACCESSDENIEDSRC, false, Some(SkipReason::AccessDenied)),
            (DE_PATHTOODEEP, false, Some(SkipReason::TooLong)),
            (DE_FILENAMETOOLONG, false, Some(SkipReason::TooLong)),
            (DE_FILE_TOO_LARGE, false, Some(SkipReason::Other(DE_FILE_TOO_LARGE))),
            (DE_INVALIDFILES, false, Some(SkipReason::Other(DE_INVALIDFILES))),
            (0x71, false, Some(SkipReason::Other(0x71))),
            (0x74, false, Some(SkipReason::Other(0x74))),
            (0xB7, false, Some(SkipReason::Other(0xB7))),
            (0x10000, false, Some(SkipReason::Other(0x10000))),
            (0x402, false, Some(SkipReason::Other(0x402))),
            // Cancels, in every form, are the aborted reason.
            (DE_OPCANCELLED, false, aborted),
            (DE_OPCANCELLED, true, aborted),
            (ERROR_CANCELLED, false, aborted),
            (hr_win32(1223), false, aborted),
            (hr_win32(1223), true, aborted),
            (HR_E_ABORT, false, aborted),
            (HR_E_ABORT, true, aborted),
            (HR_USER_CANCELLED, true, aborted),
            (HR_CANCELLED, false, aborted),
            // Plain Win32 codes, bare and as HRESULTs.
            (2, false, Some(SkipReason::Other(2))),
            (5, false, Some(SkipReason::AccessDenied)),
            (32, false, Some(SkipReason::Locked)),
            (32, true, Some(SkipReason::Locked)),
            (206, false, Some(SkipReason::TooLong)),
            (hr_win32(2), false, Some(SkipReason::Other(2))),
            (hr_win32(5), false, Some(SkipReason::AccessDenied)),
            (hr_win32(32), false, Some(SkipReason::Locked)),
            (hr_win32(206), false, Some(SkipReason::TooLong)),
            // Other HRESULTs keep their code.
            (HR_RECYCLE_SIZE_TOO_BIG, false, Some(SkipReason::Other(HR_RECYCLE_SIZE_TOO_BIG))),
            (HR_RECYCLE_FORCE_NUKE, false, Some(SkipReason::Other(HR_RECYCLE_FORCE_NUKE))),
            (E_FAIL.0, false, Some(SkipReason::Other(E_FAIL.0))),
        ] {
            assert_eq!(skip_for_shell_result(code, flag), want, "code={code:#x} aborted={flag}");
        }
        // Every DE_* code from the documentation is a skip, never a success.
        for code in (0x71..=0x88).chain([0xB7, 0x402, 0x10000, 0x10074]) {
            assert!(skip_for_shell_result(code, false).is_some(), "code={code:#x}");
        }
    }

    /// Every `TSF_*` bit (`_TRANSFER_SOURCE_FLAGS`, 0x1 to 0x2000).
    const TSF_BITS: u32 = 0x3FFF;

    #[test]
    fn pre_delete_allows_only_an_item_the_shell_will_recycle() {
        let recycle = TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32;
        assert_eq!(recycle, 0x80);
        // Every combination of the other bits, with and without the recycle bit.
        let others = TSF_BITS & !recycle;
        let mut sub = others;
        loop {
            assert_eq!(pre_delete_decision(sub), PreDelete::Abort, "flags={sub:#x}");
            assert_eq!(pre_delete_decision(sub | recycle), PreDelete::Allow, "flags={:#x}", sub | recycle);
            if sub == 0 {
                break;
            }
            sub = (sub - 1) & others;
        }
        assert_eq!(pre_delete_decision(!recycle), PreDelete::Abort);
    }

    #[test]
    fn post_delete_counts_only_a_success_with_a_new_item_in_the_bin() {
        for (hr, null, want) in [
            (S_OK.0, false, PostDelete::Recycled),
            (1, false, PostDelete::Recycled),
            (S_OK.0, true, PostDelete::NotRecycled(S_OK.0)),
            (E_FAIL.0, false, PostDelete::NotRecycled(E_FAIL.0)),
            (E_FAIL.0, true, PostDelete::NotRecycled(E_FAIL.0)),
            (HR_E_ABORT, false, PostDelete::NotRecycled(HR_E_ABORT)),
        ] {
            assert_eq!(post_delete_decision(hr, null), want, "hr={hr:#x} null={null}");
        }
    }

    #[test]
    fn results_are_a_success_only_when_the_sink_saw_the_item_recycled() {
        let other = |c| Some(SkipReason::Other(c));
        // Every class of operation result: success, a positive non-zero code, a failure, a
        // cancel, and Win32 codes in an HRESULT. With each: what the code alone means.
        let hrs = [
            (0, None),
            (1, other(1)),
            (E_FAIL.0, other(E_FAIL.0)),
            (HR_E_ABORT, other(ABORTED)),
            (hr_win32(5), Some(SkipReason::AccessDenied)),
            (hr_win32(1223), other(ABORTED)),
        ];
        let seens = [
            Seen::Nothing,
            Seen::Refused,
            Seen::Post(PostDelete::Recycled),
            Seen::Post(PostDelete::NotRecycled(0)),
            Seen::Post(PostDelete::NotRecycled(E_FAIL.0)),
        ];
        for (hr, by_code) in hrs {
            for aborted in [false, true] {
                for seen in seens {
                    let got = skip_for_result(hr, aborted, seen);
                    let want = match seen {
                        // The sink's evidence wins over the operation's result.
                        Seen::Refused => other(NOT_RECYCLABLE),
                        // A success code with no new item: the item may be gone. Scan again.
                        Seen::Post(PostDelete::NotRecycled(0)) => other(NOT_IN_BIN),
                        Seen::Post(PostDelete::NotRecycled(code)) => other(code),
                        // In the bin: a success only with no error and no abort after it.
                        Seen::Post(PostDelete::Recycled) if hr == 0 && !aborted => None,
                        Seen::Post(PostDelete::Recycled) => other(IN_BIN_THEN_ERROR),
                        // No sink report: the operation's result, never a success.
                        Seen::Nothing if hr != 0 => by_code,
                        Seen::Nothing if aborted => other(ABORTED),
                        Seen::Nothing => other(NOT_IN_BIN),
                    };
                    let case = format!("hr={hr:#x} aborted={aborted} seen={seen:?}");
                    assert_eq!(got, want, "{case}");
                    // Never a false success.
                    let success = hr == 0 && !aborted && seen == Seen::Post(PostDelete::Recycled);
                    assert_eq!(got.is_none(), success, "{case}");
                }
            }
        }
    }

    #[test]
    fn every_seen_value_round_trips_through_the_atomic_encoding() {
        let mut values = vec![Seen::Nothing, Seen::Refused, Seen::Post(PostDelete::Recycled)];
        for code in [0, 1, -1, 2, i32::MIN, i32::MAX, E_FAIL.0, HR_E_ABORT, hr_win32(5)] {
            values.push(Seen::Post(PostDelete::NotRecycled(code)));
        }
        for s in &values {
            assert_eq!(decode(encode(*s)), *s, "{s:?}");
        }
        // Distinct values have distinct encodings.
        let mut codes: Vec<u64> = values.iter().map(|s| encode(*s)).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), values.len());
        assert_eq!(decode(encode(Seen::Nothing)), Seen::Nothing);
        // An unknown kind fails closed.
        assert_eq!(decode(7 << 32), Seen::Refused);
    }

    #[test]
    fn the_sink_state_from_two_threads_ends_at_the_worst_report() {
        let sink = Sink::new();
        assert_eq!(sink.seen(), Seen::Nothing);
        let (recycle, s) = (TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32, &sink);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for _ in 0..1000 {
                    s.PostDeleteItem(recycle, Ref::default(), S_OK, Ref::default()).unwrap();
                }
            });
            scope.spawn(move || {
                for i in 0..1000 {
                    let pre = s.PreDeleteItem(if i == 500 { 0 } else { recycle }, Ref::default());
                    assert_eq!(pre.is_err(), i == 500);
                }
            });
        });
        assert_eq!(sink.seen(), Seen::Refused, "the refusal from the other thread is kept");

        let sink = Sink::new();
        let s = &sink;
        std::thread::scope(|scope| {
            scope.spawn(move || s.PostDeleteItem(recycle, Ref::default(), S_OK, Ref::default()).unwrap());
            scope.spawn(move || s.PostDeleteItem(recycle, Ref::default(), S_OK, Ref::default()).unwrap());
        });
        assert_eq!(sink.seen(), Seen::Post(PostDelete::NotRecycled(0)));
    }

    #[test]
    fn worse_keeps_the_worst_report() {
        let (r, n) = (Seen::Post(PostDelete::Recycled), Seen::Post(PostDelete::NotRecycled(5)));
        assert_eq!(worse(Seen::Nothing, r), r);
        assert_eq!(worse(r, n), n);
        assert_eq!(worse(n, r), n);
        assert_eq!(worse(Seen::Refused, r), Seen::Refused);
        assert_eq!(worse(n, Seen::Refused), Seen::Refused);
    }

    #[test]
    fn skip_text_offers_the_permanent_delete_where_the_bin_did_not_take_the_item() {
        let offers = [
            SkipReason::Other(PATH_REFUSED),
            SkipReason::Other(DE_INVALIDFILES),
            SkipReason::Other(DE_FILE_TOO_LARGE),
            SkipReason::Other(HR_RECYCLE_SIZE_TOO_BIG),
            SkipReason::Other(ABORTED),
            SkipReason::Other(NOT_RECYCLABLE),
            SkipReason::Other(HR_RECYCLE_FORCE_NUKE),
            SkipReason::Other(HR_RECYCLE_UNKNOWN),
            SkipReason::Other(HR_RECYCLE_PATH_TOO_LONG),
            SkipReason::Other(HR_RECYCLE_BIN_NOT_FOUND),
        ];
        for r in offers {
            let t = skip_text(r).unwrap_or_else(|| panic!("{r:?} has no text"));
            assert!(t.contains("Recycle Bin"), "{t}");
            assert!(t.ends_with("You can delete the item permanently."), "{t}");
        }
        assert!(skip_text(SkipReason::Other(PATH_REFUSED)).unwrap().contains("path"));
        assert!(skip_text(SkipReason::Other(DE_FILE_TOO_LARGE)).unwrap().contains("too large"));
        assert!(skip_text(SkipReason::Other(NOT_RECYCLABLE)).unwrap().contains("cannot take this item"));
        // The item is in the bin: no offer to delete it permanently.
        let in_bin = skip_text(SkipReason::Other(IN_BIN_THEN_ERROR)).expect("in-bin text");
        assert!(in_bin.contains("in the Recycle Bin") && in_bin.contains("error"), "{in_bin}");
        assert!(!in_bin.contains("permanently"), "{in_bin}");
        // The item may be gone: no offer to delete it, a request to scan again.
        let gone = skip_text(SkipReason::Other(NOT_IN_BIN)).expect("text");
        assert!(!gone.contains("permanently") && gone.contains("Scan"), "{gone}");
        for r in [SkipReason::Locked, SkipReason::AccessDenied, SkipReason::TooLong, SkipReason::Other(2)] {
            assert_eq!(skip_text(r), None, "{r:?}");
        }
    }

    #[test]
    fn the_flags_recycle_with_no_shell_window_and_no_nuke_warning() {
        for (flag, name) in [
            (FOF_ALLOWUNDO, "FOF_ALLOWUNDO"),
            (FOFX_RECYCLEONDELETE, "FOFX_RECYCLEONDELETE"),
            (FOF_NOCONFIRMATION, "FOF_NOCONFIRMATION"),
            (FOF_SILENT, "FOF_SILENT"),
            (FOF_NOERRORUI, "FOF_NOERRORUI"),
        ] {
            assert_ne!(FLAGS.0 & flag.0, 0, "{name}");
        }
        assert_eq!(FLAGS.0 & windows::Win32::UI::Shell::FOF_WANTNUKEWARNING.0, 0);
    }

    #[test]
    fn recycle_keeps_its_owner_window() {
        assert_eq!(Recycle::default().owner, None);
        assert_eq!(Recycle::new(Some(0x1234)).owner, Some(0x1234));
    }

    #[test]
    fn com_is_uninitialised_only_by_the_guard_that_initialised_it() {
        std::thread::spawn(|| {
            let first = Com::init().expect("init");
            assert!(first.uninit, "a new thread is initialised by the guard");
            // A second init on the same thread (S_FALSE) also needs its own uninit.
            let second = Com::init().expect("init again");
            assert!(second.uninit);
        })
        .join()
        .unwrap();
        std::thread::spawn(|| {
            // SAFETY: no reserved pointer; balanced below on this thread.
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().expect("MTA");
            let g = Com::init().expect("a thread in the other mode keeps its COM");
            assert!(!g.uninit, "RPC_E_CHANGED_MODE: not ours to uninitialise");
            drop(g);
            // SAFETY: balances the CoInitializeEx above.
            unsafe { CoUninitialize() };
        })
        .join()
        .unwrap();
    }

    #[test]
    fn the_sink_refuses_an_item_the_bin_cannot_take_and_records_the_result() {
        let d = Dir::new("sink");
        // COM is started here without the guard, so this test does not depend on it. The test
        // thread ends with the test, which ends its COM too.
        // SAFETY: no reserved pointer; on this thread only.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().expect("com");
        let path = pfrom(&plain(&d.base).to_string_lossy());
        // SAFETY: COM is initialised on this thread; `path` is NUL-terminated and lives across the call.
        let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None) }.expect("item");
        let recycle = TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32;

        let sink = Sink::new();
        let i: IFileOperationProgressSink = sink.to_interface();
        // SAFETY (each call below): `i` is a live sink on this thread; the items are live or null.
        unsafe { i.PreDeleteItem(recycle, None) }.expect("a recyclable item is allowed");
        assert_eq!(sink.seen(), Seen::Nothing);
        unsafe { i.PostDeleteItem(recycle, None, S_OK, &item) }.unwrap();
        assert_eq!(sink.seen(), Seen::Post(PostDelete::Recycled));
        let refused = unsafe { i.PreDeleteItem(0, None) }.expect_err("an item the bin cannot take is refused");
        assert_eq!(refused.code(), E_ABORT);
        assert_eq!(sink.seen(), Seen::Refused);
        unsafe { i.PostDeleteItem(recycle, None, S_OK, &item) }.unwrap();
        assert_eq!(sink.seen(), Seen::Refused, "a later report does not hide the refusal");

        let sink = Sink::new();
        let i: IFileOperationProgressSink = sink.to_interface();
        unsafe { i.PostDeleteItem(recycle, None, S_OK, None) }.unwrap();
        assert_eq!(sink.seen(), Seen::Post(PostDelete::NotRecycled(0)), "no new item: not in the bin");
        unsafe { i.PostDeleteItem(recycle, None, E_FAIL, &item) }.unwrap();
        assert_eq!(sink.seen(), Seen::Post(PostDelete::NotRecycled(0)), "the first failure stays");
        let sink = Sink::new();
        let i: IFileOperationProgressSink = sink.to_interface();
        unsafe { i.PostDeleteItem(recycle, None, E_FAIL, &item) }.unwrap();
        assert_eq!(sink.seen(), Seen::Post(PostDelete::NotRecycled(E_FAIL.0)));
    }

    #[test]
    fn a_missing_target_is_one_skip() {
        let d = Dir::new("missing");
        let t = target(&d.base.join("gone"), true, 10);
        let (o, _) = run(&t, false);
        assert_eq!((o.removed_items, o.removed_bytes, o.stopped), (0, 0, false));
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, t.path);
        assert_eq!(o.skipped[0].reason, reason_from_win32(2));
    }

    #[test]
    fn stop_set_before_the_call_leaves_the_item_in_place() {
        let d = Dir::new("stop");
        let item = d.base.join("t");
        fs::create_dir(&item).unwrap();
        fs::write(item.join("keep"), b"x").unwrap();
        let (o, calls) = run(&target(&item, true, 1), true);
        assert!(o.stopped);
        assert_eq!((o.removed_items, o.removed_bytes), (0, 0));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert!(calls.is_empty(), "{calls:?}");
        assert!(item.join("keep").exists());
    }

    #[test]
    fn a_plain_path_is_one_skip_and_stays() {
        let d = Dir::new("plain");
        let item = d.base.join("t");
        fs::create_dir(&item).unwrap();
        let t = target(&plain(&item), true, 0);
        let (o, _) = run(&t, false);
        assert_eq!(o.removed_items, 0);
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, t.path);
        assert_eq!(o.skipped[0].reason, SkipReason::Other(PATH_REFUSED));
        assert!(item.exists());
    }

    #[test]
    fn a_trailing_dot_is_one_skip_and_its_sibling_stays() {
        let d = Dir::new("dot");
        fs::create_dir(d.base.join("pd")).unwrap();
        fs::write(d.base.join("pd").join("plain-mark"), b"x").unwrap();
        let dot = d.base.join("pd.");
        if fs::create_dir(&dot).is_err() {
            skipped("trailing dot (the OS refused the name pd.)");
            return;
        }
        fs::write(dot.join("dot-mark"), b"x").unwrap();
        let (o, calls) = run(&target(&dot, true, 1), false);
        assert_eq!(o.removed_items, 0);
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].reason, SkipReason::Other(PATH_REFUSED));
        assert!(calls.is_empty(), "the shell is not called: {calls:?}");
        // The plain form `...\pd.` is `...\pd` to Win32: both must stay.
        assert!(d.base.join("pd").join("plain-mark").exists());
        assert!(dot.join("dot-mark").exists());
    }

    #[test]
    fn a_path_too_long_for_the_shell_is_one_skip_and_stays() {
        let d = Dir::new("long");
        let item = d.base.join("L".repeat(120)).join("M".repeat(120)).join("N".repeat(40));
        fs::create_dir_all(&item).unwrap();
        assert!(plain(&item).as_os_str().encode_wide().count() > MAX_PLAIN);
        let (o, _) = run(&target(&item, true, 0), false);
        assert_eq!(o.removed_items, 0);
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].reason, SkipReason::Other(PATH_REFUSED));
        assert!(item.exists());
    }

    /// The bin's `$I` records (one per item) whose original path is `p`, in every bin folder of
    /// `p`'s drive this user can read. Only read, never changed. `None` when no bin folder can be
    /// read. Format: version (u64), size (u64), time (u64), then for version 2 the name length
    /// (u32) and the UTF-16 path; for version 1 a fixed 260-unit path.
    fn bin_records(p: &Path) -> Option<usize> {
        let want = plain(p).to_string_lossy().to_lowercase();
        let drive = want.get(..2)?.to_string();
        let mut readable = false;
        let mut n = 0;
        for user in fs::read_dir(format!(r"{drive}\$Recycle.Bin")).ok()?.flatten() {
            let Ok(entries) = fs::read_dir(user.path()) else { continue };
            readable = true;
            for e in entries.flatten() {
                if !e.file_name().to_string_lossy().starts_with("$I") {
                    continue;
                }
                let Ok(b) = fs::read(e.path()) else { continue };
                let text = |from: usize| -> String {
                    let w: Vec<u16> =
                        b.get(from..).unwrap_or_default().as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                    String::from_utf16_lossy(&w).trim_end_matches('\0').to_lowercase()
                };
                let name = match b.first() {
                    Some(2) => text(28),
                    Some(1) => text(24),
                    _ => continue,
                };
                if name.trim_start_matches(r"\\?\") == want {
                    n += 1;
                }
            }
        }
        readable.then_some(n)
    }

    /// Checks that `p` came into the bin: one more `$I` record names it than `before`.
    fn assert_in_bin(p: &Path, before: Option<usize>) {
        match (before, bin_records(p)) {
            (Some(b), Some(a)) => assert_eq!(a, b + 1, "the bin has no new record for {p:?}"),
            _ => skipped("Recycle Bin record check (no bin folder could be read)"),
        }
    }

    #[test]
    #[ignore = "sends a temp folder to the real Recycle Bin"]
    fn a_folder_goes_to_the_recycle_bin_as_one_item() {
        let d = Dir::new("folder");
        let item = d.base.join(Dir::bin_name("folder"));
        for sub in ["a", "b"] {
            fs::create_dir_all(item.join(sub)).unwrap();
            fs::write(item.join(sub).join("f.txt"), b"12345").unwrap();
        }
        let before = bin_records(&item);
        let (o, calls) = run(&target(&item, true, 10), false);
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert!(!o.stopped);
        assert_eq!((o.removed_items, o.removed_bytes), (1, 10));
        assert_eq!(calls, vec![(0, 0), (1, 10)]);
        assert!(!item.exists(), "the folder left its place");
        assert_in_bin(&item, before);
    }

    #[test]
    #[ignore = "sends a temp folder to the real Recycle Bin"]
    fn the_shell_recycles_a_file_with_no_abort_and_reports_it_in_the_bin() {
        let d = Dir::new("file");
        let item = d.base.join(format!("{}.txt", Dir::bin_name("file")));
        fs::write(&item, b"hello").unwrap();
        let before = bin_records(&item);
        let from = shell_path(&item).expect("a plain form");
        let r = shell_recycle(&from, None);
        assert_eq!(r, Report { hr: 0, aborted: false, seen: Seen::Post(PostDelete::Recycled) });
        assert!(!item.exists(), "the file left its place");
        assert_in_bin(&item, before);
    }

    #[test]
    #[ignore = "sends a temp folder to the real Recycle Bin"]
    fn a_junction_goes_to_the_bin_as_itself() {
        let d = Dir::new("junction");
        let other = d.base.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("keep"), b"x").unwrap();
        let link = d.base.join(Dir::bin_name("junction"));
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(plain(&link))
            .arg(plain(&other))
            .output()
            .is_ok_and(|o| o.status.success());
        if !made {
            skipped("junction item (mklink /J was refused)");
            return;
        }
        d.links.borrow_mut().push(link.clone());
        let before = bin_records(&link);
        let mut t = target(&link, false, 0);
        t.is_link = true;
        let (o, _) = run(&t, false);
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!(o.removed_items, 1);
        assert!(fs::symlink_metadata(&link).is_err(), "the junction left its place");
        assert!(other.join("keep").exists(), "the junction was not followed");
        assert_in_bin(&link, before);
    }

    /// A network path has no Recycle Bin: the shell would delete it permanently. The item is a
    /// temp folder reached through this machine's own admin share (`\\localhost\C$`), so the test
    /// changes no setting. `shell_recycle` is called directly, as `shell_path` refuses UNC paths.
    #[test]
    #[ignore = "sends a temp folder to the real Recycle Bin"]
    fn an_item_the_bin_cannot_take_is_refused_and_stays() {
        let d = Dir::new("network");
        let item = d.base.join("must-stay");
        fs::create_dir(&item).unwrap();
        fs::write(item.join("keep"), b"x").unwrap();
        let local = plain(&item).to_string_lossy().into_owned();
        let unc = format!(r"\\localhost\{}${}", &local[..1], &local[2..]);
        if let Err(e) = fs::metadata(&unc) {
            skipped(&format!("item the bin cannot take (the admin share path {unc} is not reachable: {e})"));
            return;
        }
        let r = shell_recycle(&pfrom(&unc), None);
        assert_eq!(r.seen, Seen::Refused, "{r:?}");
        assert_eq!(skip_for_result(r.hr, r.aborted, r.seen), Some(SkipReason::Other(NOT_RECYCLABLE)));
        assert!(item.join("keep").exists(), "the item the bin cannot take is not deleted");
    }
}

//! Fast permanent delete, relative to open handles. The target is opened once by its verbatim
//! (`\\?\`) path. Everything below it is opened by name relative to its parent folder's open
//! handle (`NtCreateFile` with `RootDirectory`), checked through its own handle, and removed
//! through that handle (`SetFileInformationByHandle`). No path below the target is ever resolved
//! again, so a folder swapped for a link during the run cannot lead the delete out of the target.
//! Work runs in parallel batches; progress and `stop` are seen between batches.

#![cfg_attr(not(test), expect(dead_code, reason = "wired in the UI task"))]
#![cfg_attr(test, allow(dead_code, reason = "tests do not use every item; wired in the UI task"))]

use crate::delete::{DeleteTarget, Outcome, Remover, Skipped, reason_from_win32};
use rayon::prelude::*;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY, ERROR_FILE_NOT_FOUND, ERROR_FILENAME_EXCED_RANGE,
    ERROR_INVALID_DATA, ERROR_INVALID_FUNCTION, ERROR_INVALID_HANDLE, ERROR_INVALID_NAME, ERROR_INVALID_PARAMETER,
    ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED, ERROR_SHARING_VIOLATION, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SYSTEM, FILE_ATTRIBUTE_TAG_INFO, FILE_ATTRIBUTE_TEMPORARY,
    FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FULL_DIR_INFO, FILE_INFO_BY_HANDLE_CLASS,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
    FILE_WRITE_ATTRIBUTES, FileAttributeTagInfo, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx,
    FileFullDirectoryInfo, FileStandardInfo, GetFileInformationByHandleEx, OPEN_EXISTING, SYNCHRONIZE,
    SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

/// Items run in one parallel batch: the bound on the progress rate and on the work done after
/// `stop` is set.
const BATCH: usize = 256;

/// Bytes asked for in one folder-listing call.
const LIST_BUF: usize = 64 * 1024;

/// The reparse-tag bit of a name surrogate (junction, symlink): the same test as the scan's.
const NAME_SURROGATE: u32 = 0x2000_0000;

/// The attributes `FileBasicInfo` sets here; the others are left out.
const SETTABLE: u32 = FILE_ATTRIBUTE_ARCHIVE
    | FILE_ATTRIBUTE_HIDDEN
    | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
    | FILE_ATTRIBUTE_OFFLINE
    | FILE_ATTRIBUTE_SYSTEM
    | FILE_ATTRIBUTE_TEMPORARY;

/// Access for an item that is removed: delete, read its kind, and clear read-only for the
/// classic delete. No data access, so opening a file does not start an antivirus scan of it.
const ITEM_ACCESS: u32 = DELETE | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE;

/// Access for a folder that is listed and then removed.
const FOLDER_ACCESS: u32 = ITEM_ACCESS | FILE_LIST_DIRECTORY;

/// Access for a folder that another process holds open without share delete: listed and
/// emptied, never removed (no delete access).
const LIST_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// Access for the first open of the target by its path: its attributes only.
const PROBE_ACCESS: u32 = FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// No `FILE_SHARE_DELETE`: while the run holds a folder open, no one can rename or move it out
/// of the target (that needs delete access), so the folder the handle names stays where the run
/// found it.
const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;

/// The fast permanent delete.
pub struct Purge;

/// How a run works. `Purge` uses `Options::default()`; tests and the benchmark change it.
pub struct Options<'a> {
    /// Try the POSIX delete first (falls back to the classic delete when the volume refuses it).
    pub posix: bool,
    /// Test seam: called on the caller's thread with a folder's path after the folder is listed
    /// and before anything in it is opened.
    pub hook: Option<&'a dyn Fn(&Path)>,
    /// Items per parallel batch (at least 1).
    pub batch: usize,
    /// Bytes per listing call.
    pub list_buf: usize,
    /// Worker threads; 0 is rayon's global pool (one per logical CPU).
    pub threads: usize,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options { posix: true, hook: None, batch: BATCH, list_buf: LIST_BUF, threads: 0 }
    }
}

impl Remover for Purge {
    fn remove(&self, t: &DeleteTarget, progress: &dyn Fn(u64, u64), stop: &AtomicBool) -> Outcome {
        remove_with(t, progress, stop, &Options::default())
    }
}

/// What an opened item is, from its own handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    File,
    /// A name-surrogate reparse point (junction, symlink, mount point): removed as itself.
    Link,
    /// A folder to list: plain, or a reparse folder that is not a link (a cloud folder).
    Folder,
}

fn kind_of(attrs: u32, tag: u32) -> Kind {
    if is_name_surrogate_link(attrs, tag) {
        Kind::Link
    } else if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Folder
    } else {
        Kind::File
    }
}

/// An open handle, closed once: by `close` where the result is reported (the close is what
/// completes a delete), otherwise on drop.
struct Handle(HANDLE);

// SAFETY: a kernel handle is a process-wide value that any thread may use. The calls made on
// one here (relative opens, information queries and sets) are synchronous, and a folder handle
// is listed only by the one worker that opened it.
unsafe impl Send for Handle {}
// SAFETY: as above; shared use is only relative opens against a parent folder's handle.
unsafe impl Sync for Handle {}

impl Handle {
    fn close(self) -> Result<(), u32> {
        let h = std::mem::ManuallyDrop::new(self).0;
        // SAFETY: `h` is open and owned by this value; `ManuallyDrop` keeps drop from closing it again.
        if unsafe { CloseHandle(h) } == 0 { Err(last_error()) } else { Ok(()) }
    }
}

impl Drop for Handle {
    /// Drop closes a handle only where nothing is removed through it: the item is a skip
    /// already, the run stopped, or the handle is opened again. A failed close there means a
    /// bad handle value, a bug that a debug build stops on.
    fn drop(&mut self) {
        // SAFETY: `self.0` is open and owned by this value, and is closed only here.
        let closed = unsafe { CloseHandle(self.0) };
        debug_assert!(closed != 0, "CloseHandle failed: {}", last_error());
    }
}

/// One entry of a folder listing. The listing only gives the name, the size for the byte count,
/// and a hint of the access to ask for; what is done to the item comes from its own handle.
struct Listed {
    name: Vec<u16>,
    size: u64,
    folder_hint: bool,
}

/// An open folder the run works in. `h` is taken out when the folder's turn to be removed comes.
struct DirNode {
    h: Option<Handle>,
    attrs: u32,
    /// False when `h` is the list-only open of a folder held open elsewhere.
    deletable: bool,
    path: Vec<u16>,
    parent: Option<usize>,
    /// Entries not finished yet.
    pending: usize,
}

/// One item of a batch.
enum Work {
    /// An entry of the open folder `parent`.
    Child { parent: usize, entry: Listed },
    /// The folder `dir`, with every entry finished: removed through `h` when `deletable`.
    Rmdir { dir: usize, h: Handle, attrs: u32, deletable: bool },
}

/// What a worker did with one item of a batch.
enum Finished {
    /// `stop` was set before the item started: nothing was done to it.
    NotRun,
    Child { parent: usize, name: Vec<u16>, done: Done },
    Rmdir { dir: usize, result: Result<(), u32> },
}

/// What happened to one entry.
enum Done {
    /// Removed, with the bytes it freed.
    Removed(u64),
    Failed(u32),
    /// A folder, open and listed (or not, with the listing's error).
    Folder { h: Handle, attrs: u32, deletable: bool, listed: Result<Vec<Listed>, u32> },
}

/// The caller-thread state of a folder run.
struct Run<'a> {
    dirs: Vec<DirNode>,
    work: Vec<Work>,
    out: Outcome,
    hook: Option<&'a dyn Fn(&Path)>,
}

/// The target must be a verbatim (`\\?\`) path, as `check_target` gives it; any other path is
/// one skip and nothing is touched. The target is opened without following a link. A file or a
/// link target is removed through its handle. A folder is listed; each entry is opened relative
/// to the folder's handle and checked through its own: a link (a name-surrogate reparse point)
/// is removed as itself and never listed, a file is removed, a folder (plain, or a reparse
/// folder that is not a link, like a cloud folder) is listed in turn. A folder is removed when
/// all its entries are finished. A folder that will not empty stays: it gets its own skip unless
/// a skip already recorded lies in it, so a target that stays always has a skip (or `stopped`).
/// A folder (the target too) that another process holds open without share delete is opened for
/// listing only: it is emptied and stays, with one Locked skip. Counts are as `Remover` says; `removed_bytes` takes each file's size from its folder listing.
pub(crate) fn remove_with(t: &DeleteTarget, progress: &dyn Fn(u64, u64), stop: &AtomicBool, opts: &Options) -> Outcome {
    let mut out = Outcome::default();
    let root: Vec<u16> = t.path.as_os_str().encode_wide().collect();
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    if !root.starts_with(&prefix) {
        out.skipped.push(skip(&root, ERROR_INVALID_NAME));
        return out;
    }
    let posix = AtomicBool::new(opts.posix);
    // The attribute-only open names the object; the access the run needs comes from opening
    // that same object again, by its kind (a file target asks for no list access).
    let opened = open_path(&root).and_then(|probe| {
        let (attrs, tag) = tag_info(&probe)?;
        open_entry(&|a| reopen_info(&probe, a), kind_of(attrs, tag) == Kind::Folder)
    });
    let Opened { h, attrs, tag, deletable } = match opened {
        Ok(o) => o,
        Err(code) => {
            out.skipped.push(skip(&root, code));
            return out;
        }
    };
    let kind = kind_of(attrs, tag);
    if kind != Kind::Folder {
        let size = if kind == Kind::File { standard_size(&h) } else { Ok(0) };
        match size.and_then(|size| dispose(h, attrs, &posix).map(|()| size)) {
            Ok(size) => {
                out.removed_items = 1;
                out.removed_bytes = size;
            }
            Err(code) => out.skipped.push(skip(&root, code)),
        }
        progress(out.removed_items, out.removed_bytes);
        return out;
    }
    let listed = match list(&h, opts.list_buf) {
        Ok(l) => l,
        Err(code) => {
            out.skipped.push(skip(&root, code));
            progress(0, 0);
            return out;
        }
    };
    if let Some(f) = opts.hook {
        f(&to_path(&root));
    }
    let mut run = Run { dirs: Vec::new(), work: Vec::new(), out, hook: opts.hook };
    run.add_folder(h, attrs, deletable, root, None, listed);
    // Only the benchmark asks for a thread count. If that pool cannot be made, the global pool
    // does the same work.
    let pool = match opts.threads {
        0 => None,
        n => rayon::ThreadPoolBuilder::new().num_threads(n).build().ok(),
    };
    let (batch, list_buf) = (opts.batch.max(1), opts.list_buf);
    while !run.work.is_empty() {
        if stop.load(Ordering::Relaxed) {
            run.out.stopped = true;
            break;
        }
        let items = run.work.split_off(run.work.len().saturating_sub(batch));
        let dirs = &run.dirs;
        let go = || -> Vec<Finished> {
            items
                .into_par_iter()
                .map(|w| if stop.load(Ordering::Relaxed) { Finished::NotRun } else { w.run(dirs, &posix, list_buf) })
                .collect()
        };
        let finished = match &pool {
            Some(p) => p.install(go),
            None => go(),
        };
        for f in finished {
            run.apply(f);
        }
        progress(run.out.removed_items, run.out.removed_bytes);
    }
    progress(run.out.removed_items, run.out.removed_bytes);
    // Folders still open (the run stopped) close here, with nothing removed through them.
    drop(run.dirs);
    run.out
}

impl Work {
    /// Runs on a worker thread.
    fn run(self, dirs: &[DirNode], posix: &AtomicBool, list_buf: usize) -> Finished {
        match self {
            Work::Rmdir { dir, h, attrs, deletable: true } => Finished::Rmdir { dir, result: dispose(h, attrs, posix) },
            // Opened for listing only: held open elsewhere without share delete. The handle
            // has no delete access, so no delete is tried; the folder stays as a Locked skip.
            Work::Rmdir { dir, h, deletable: false, .. } => {
                drop(h);
                Finished::Rmdir { dir, result: Err(ERROR_SHARING_VIOLATION) }
            }
            Work::Child { parent, entry } => {
                let done = match dirs.get(parent).and_then(|d| d.h.as_ref()) {
                    Some(ph) => child(ph, &entry, posix, list_buf),
                    None => Done::Failed(ERROR_INVALID_HANDLE),
                };
                Finished::Child { parent, name: entry.name, done }
            }
        }
    }
}

impl Run<'_> {
    /// Adds an open, listed folder and queues its entries (or its own removal when it is empty).
    fn add_folder(
        &mut self,
        h: Handle,
        attrs: u32,
        deletable: bool,
        path: Vec<u16>,
        parent: Option<usize>,
        listed: Vec<Listed>,
    ) {
        let dir = self.dirs.len();
        let pending = listed.len();
        self.dirs.push(DirNode { h: Some(h), attrs, deletable, path, parent, pending });
        if pending == 0 {
            self.queue_rmdir(dir);
        } else {
            self.work.extend(listed.into_iter().map(|entry| Work::Child { parent: dir, entry }));
        }
    }

    fn queue_rmdir(&mut self, dir: usize) {
        let node = &mut self.dirs[dir];
        match node.h.take() {
            Some(h) => self.work.push(Work::Rmdir { dir, h, attrs: node.attrs, deletable: node.deletable }),
            // Not reached: a folder's removal is queued once. Reported, never silent.
            None => self.out.skipped.push(skip(&node.path, ERROR_INVALID_HANDLE)),
        }
    }

    /// One entry of `parent` is finished; the folder's removal is queued after its last one.
    fn finish(&mut self, parent: usize) {
        let node = &mut self.dirs[parent];
        node.pending -= 1;
        if node.pending == 0 {
            self.queue_rmdir(parent);
        }
    }

    fn child_path(&self, parent: usize, name: &[u16]) -> Vec<u16> {
        let mut p = self.dirs[parent].path.clone();
        p.push(u16::from(b'\\'));
        p.extend_from_slice(name);
        p
    }

    /// Takes one worker result into the outcome, on the caller's thread.
    fn apply(&mut self, f: Finished) {
        match f {
            // Nothing was done to it; its folder stays.
            Finished::NotRun => self.out.stopped = true,
            Finished::Child { parent, name, done } => match done {
                Done::Removed(bytes) => {
                    self.out.removed_items += 1;
                    self.out.removed_bytes += bytes;
                    self.finish(parent);
                }
                Done::Failed(code) => {
                    let path = self.child_path(parent, &name);
                    self.out.skipped.push(skip(&path, code));
                    self.finish(parent);
                }
                Done::Folder { h, attrs, deletable, listed } => {
                    let path = self.child_path(parent, &name);
                    match listed {
                        Ok(listed) => {
                            if let Some(f) = self.hook {
                                f(&to_path(&path));
                            }
                            self.add_folder(h, attrs, deletable, path, Some(parent), listed);
                        }
                        Err(code) => {
                            // Not listed, so not removed: the skip says why the folder stays.
                            self.out.skipped.push(skip(&path, code));
                            drop(h);
                            self.finish(parent);
                        }
                    }
                }
            },
            Finished::Rmdir { dir, result } => {
                let path = std::mem::take(&mut self.dirs[dir].path);
                match result {
                    Ok(()) => self.out.removed_items += 1,
                    Err(ERROR_DIR_NOT_EMPTY) => {
                        // A skipped item inside is listed already; anything else (a file added
                        // during the run, a delete still pending) gets a skip for the folder.
                        let s = skip(&path, ERROR_DIR_NOT_EMPTY);
                        if needs_skip_for_folder(&s.path, &self.out.skipped) {
                            self.out.skipped.push(s);
                        }
                    }
                    Err(code) => self.out.skipped.push(skip(&path, code)),
                }
                if let Some(p) = self.dirs[dir].parent {
                    self.finish(p);
                }
            }
        }
    }
}

/// Opens one listed entry relative to its folder's handle and acts on what its own handle says
/// it is: a link or a file is removed through the handle, a folder is listed and handed back
/// open. The listing's kind only picks the access asked for; a folder where the listing saw
/// something else is opened again, still relative to the same folder handle.
fn child(parent: &Handle, e: &Listed, posix: &AtomicBool, list_buf: usize) -> Done {
    let Opened { h, attrs, tag, deletable } = match open_entry(&|a| open_child_info(parent, &e.name, a), e.folder_hint) {
        Ok(o) => o,
        Err(code) => return Done::Failed(code),
    };
    match kind_of(attrs, tag) {
        Kind::Folder => Done::Folder { listed: list(&h, list_buf), h, attrs, deletable },
        Kind::Link => match dispose(h, attrs, posix) {
            Ok(()) => Done::Removed(0),
            Err(code) => Done::Failed(code),
        },
        Kind::File => match dispose(h, attrs, posix) {
            Ok(()) => Done::Removed(e.size),
            Err(code) => Done::Failed(code),
        },
    }
}

/// An item open for the run, with its kind read from its own handle. `deletable` is false for a
/// folder opened with list access only, because another process holds it open without share
/// delete: it is listed and emptied, and then gets a Locked skip instead of a delete.
struct Opened {
    h: Handle,
    attrs: u32,
    tag: u32,
    deletable: bool,
}

/// Opens an item through `open` (one access mask in, the open handle and its attributes and tag
/// out; every call opens the same name relative to the same handle). `folder_hint` picks the
/// first access. A folder where the hint said otherwise is opened again with folder access. A
/// folder that cannot be opened for delete because of a sharing violation is opened with list
/// access only.
fn open_entry(open: &dyn Fn(u32) -> Result<(Handle, u32, u32), u32>, folder_hint: bool) -> Result<Opened, u32> {
    let first = open(if folder_hint { FOLDER_ACCESS } else { ITEM_ACCESS });
    match first {
        Ok((h, attrs, tag)) if kind_of(attrs, tag) == Kind::Folder && !folder_hint => {
            drop(h);
            match open(FOLDER_ACCESS) {
                Ok((h, attrs, tag)) => Ok(Opened { h, attrs, tag, deletable: true }),
                Err(ERROR_SHARING_VIOLATION) => open_listing_only(open),
                Err(code) => Err(code),
            }
        }
        Ok((h, attrs, tag)) => Ok(Opened { h, attrs, tag, deletable: true }),
        Err(ERROR_SHARING_VIOLATION) if folder_hint => open_listing_only(open),
        Err(code) => Err(code),
    }
}

/// The list-only open of a folder held open elsewhere without share delete. It still shares
/// no delete itself, so the folder cannot be moved away while it is open. Anything that is not
/// a folder on this open stays as a Locked skip: this handle cannot delete it.
fn open_listing_only(open: &dyn Fn(u32) -> Result<(Handle, u32, u32), u32>) -> Result<Opened, u32> {
    let (h, attrs, tag) = open(LIST_ACCESS)?;
    if kind_of(attrs, tag) != Kind::Folder {
        return Err(ERROR_SHARING_VIOLATION);
    }
    Ok(Opened { h, attrs, tag, deletable: false })
}

/// True when a find-data entry is a link: a reparse point with a name-surrogate tag (junction,
/// symlink), as the scan counts links. `tag` is the find data's `dwReserved0`.
fn is_name_surrogate_link(attrs: u32, tag: u32) -> bool {
    attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 && tag & NAME_SURROGATE != 0
}

/// True when a folder that will not empty needs its own skip: no skip recorded so far lies in it
/// (or is it). Compared by whole names, so `a` does not hold `ab\x`.
fn needs_skip_for_folder(folder: &Path, skips: &[Skipped]) -> bool {
    !skips.iter().any(|s| s.path.starts_with(folder))
}

/// Opens the target itself by its verbatim path, without following a final link, for its
/// attributes only. This is the only path the run resolves. No data or delete access, so the
/// open takes part in no sharing check and starts no antivirus scan; the access the run needs is
/// asked for by opening this same object again (`reopen_info`).
fn open_path(path: &[u16]) -> Result<Handle, u32> {
    let z = nul(path);
    // SAFETY: `z` is NUL-terminated and lives across the call; no security attributes and no
    // template handle are passed.
    let h = unsafe {
        CreateFileW(
            z.as_ptr(),
            PROBE_ACCESS,
            SHARE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE { Err(last_error()) } else { Ok(Handle(h)) }
}

/// A plain entry name: not empty, not `.` or `..`, and no separator, stream colon or NUL.
fn is_plain_name(name: &[u16]) -> bool {
    let dot = u16::from(b'.');
    !name.is_empty()
        && name != [dot]
        && name != [dot, dot]
        && !name.iter().any(|&c| c == u16::from(b'\\') || c == u16::from(b'/') || c == u16::from(b':') || c == 0)
}

/// Opens `name` relative to the open folder `parent`, without following a link, and reads the
/// opened item's attributes and reparse tag from its own handle.
fn open_child_info(parent: &Handle, name: &[u16], access: u32) -> Result<(Handle, u32, u32), u32> {
    if !is_plain_name(name) {
        return Err(ERROR_INVALID_NAME);
    }
    let h = nt_open(parent, name, access)?;
    let (attrs, tag) = tag_info(&h)?;
    Ok((h, attrs, tag))
}

/// Opens the object `h` is open on again, with `access` (an empty name relative to `h`, as
/// `ReOpenFile` does): no name is looked up, so it is the same object.
fn reopen_info(h: &Handle, access: u32) -> Result<(Handle, u32, u32), u32> {
    let h = nt_open(h, &[], access)?;
    let (attrs, tag) = tag_info(&h)?;
    Ok((h, attrs, tag))
}

/// `NtCreateFile` of `name` relative to `root`, without following a link. The lookup is exact
/// (no `OBJ_CASE_INSENSITIVE`): every name comes from the folder's own listing.
fn nt_open(root: &Handle, name: &[u16], access: u32) -> Result<Handle, u32> {
    let bytes = u16::try_from(name.len() * 2).map_err(|_| ERROR_FILENAME_EXCED_RANGE)?;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: name.as_ptr().cast_mut() };
    let oa = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: root.0,
        ObjectName: &us,
        Attributes: 0,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut h: HANDLE = std::ptr::null_mut();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: `oa` points at `us`, whose counted buffer is `name` (NT names need no NUL; an
    // empty name has length 0, so its buffer is not read); both outlive the call, which only
    // reads them. `root` is an open handle. `h` and `iosb` are valid out locations. No
    // allocation size and no extended attributes.
    let status = unsafe {
        NtCreateFile(
            &mut h,
            access,
            &oa,
            &mut iosb,
            std::ptr::null(),
            0,
            SHARE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: a pure conversion of a status value.
        return Err(unsafe { RtlNtStatusToDosError(status) });
    }
    Ok(Handle(h))
}

/// The opened item's attributes and reparse tag (the tag means something only with
/// `FILE_ATTRIBUTE_REPARSE_POINT`).
fn tag_info(h: &Handle) -> Result<(u32, u32), u32> {
    let mut i = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `i` is the FILE_ATTRIBUTE_TAG_INFO this class fills, passed with its exact size.
    let ok = unsafe {
        GetFileInformationByHandleEx(h.0, FileAttributeTagInfo, (&raw mut i).cast(), size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32)
    };
    if ok == 0 { Err(last_error()) } else { Ok((i.FileAttributes, i.ReparseTag)) }
}

/// The logical size of the open file.
fn standard_size(h: &Handle) -> Result<u64, u32> {
    let mut i = FILE_STANDARD_INFO::default();
    // SAFETY: `i` is the FILE_STANDARD_INFO this class fills, passed with its exact size.
    let ok = unsafe {
        GetFileInformationByHandleEx(h.0, FileStandardInfo, (&raw mut i).cast(), size_of::<FILE_STANDARD_INFO>() as u32)
    };
    if ok == 0 { Err(last_error()) } else { Ok(u64::try_from(i.EndOfFile).unwrap_or(0)) }
}

/// Lists the open folder `h`, `.` and `..` left out.
fn list(h: &Handle, buf_bytes: usize) -> Result<Vec<Listed>, u32> {
    const NAME_AT: usize = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
    let mut buf = vec![0u64; buf_bytes.max(4096).div_ceil(8)];
    let len = buf.len() * 8;
    let len_u32 = u32::try_from(len).map_err(|_| ERROR_INVALID_PARAMETER)?;
    let mut out = Vec::new();
    loop {
        // SAFETY: `buf` is writable for `len` bytes and 8-byte aligned; the class fills it with
        // FILE_FULL_DIR_INFO records.
        let ok = unsafe { GetFileInformationByHandleEx(h.0, FileFullDirectoryInfo, buf.as_mut_ptr().cast(), len_u32) };
        if ok == 0 {
            return match last_error() {
                ERROR_NO_MORE_FILES => Ok(out),
                // No entry at all, not even `.`: an empty listing.
                ERROR_FILE_NOT_FOUND if out.is_empty() => Ok(out),
                code => Err(code),
            };
        }
        let base = buf.as_ptr().cast::<u8>();
        let mut off = 0;
        loop {
            if off + NAME_AT > len {
                return Err(ERROR_INVALID_DATA);
            }
            // SAFETY: the record's fixed part, `off..off + NAME_AT`, lies inside `buf` (checked
            // above). Each field is read unaligned through a raw place, with no reference made.
            let (next, attrs, name_len, tag, size) = unsafe {
                let p = base.add(off).cast::<FILE_FULL_DIR_INFO>();
                (
                    (&raw const (*p).NextEntryOffset).read_unaligned() as usize,
                    (&raw const (*p).FileAttributes).read_unaligned(),
                    (&raw const (*p).FileNameLength).read_unaligned() as usize,
                    // With FILE_ATTRIBUTE_REPARSE_POINT, EaSize holds the reparse tag (MS-FSCC, FileFullDirectoryInformation).
                    (&raw const (*p).EaSize).read_unaligned(),
                    (&raw const (*p).EndOfFile).read_unaligned(),
                )
            };
            if name_len % 2 != 0 || off + NAME_AT + name_len > len {
                return Err(ERROR_INVALID_DATA);
            }
            let mut name = vec![0u16; name_len / 2];
            // SAFETY: the name's `name_len` bytes lie inside `buf` (checked above) and `name` has
            // room for exactly that many bytes; the two do not overlap.
            unsafe { std::ptr::copy_nonoverlapping(base.add(off + NAME_AT), name.as_mut_ptr().cast::<u8>(), name_len) };
            let dot = u16::from(b'.');
            if name != [dot] && name != [dot, dot] {
                out.push(Listed {
                    name,
                    size: u64::try_from(size).unwrap_or(0),
                    folder_hint: kind_of(attrs, tag) == Kind::Folder,
                });
            }
            if next == 0 {
                break;
            }
            off += next;
        }
    }
}

/// Removes the item `h` is open on, through `h`, and closes it. The POSIX delete runs first: the
/// name goes at once, even while the item is open elsewhere, and read-only is ignored. When the
/// volume refuses it (FAT32, exFAT, some network shares), `posix` is cleared for the rest of the
/// run and the classic delete runs; a read-only item then loses the flag through the handle
/// first, and gets it back if the delete still fails.
fn dispose(h: Handle, attrs: u32, posix: &AtomicBool) -> Result<(), u32> {
    if posix.load(Ordering::Relaxed) {
        let flags = FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE;
        match set_info(&h, FileDispositionInfoEx, &FILE_DISPOSITION_INFO_EX { Flags: flags }) {
            Ok(()) => return h.close(),
            Err(ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION) => {
                posix.store(false, Ordering::Relaxed);
            }
            Err(code) => return Err(code),
        }
    }
    let delete = FILE_DISPOSITION_INFO { DeleteFile: true };
    match set_info(&h, FileDispositionInfo, &delete) {
        Ok(()) => return h.close(),
        Err(ERROR_ACCESS_DENIED) if attrs & FILE_ATTRIBUTE_READONLY != 0 => {}
        Err(code) => return Err(code),
    }
    set_attrs(&h, match attrs & SETTABLE {
        0 => FILE_ATTRIBUTE_NORMAL,
        a => a,
    })?;
    match set_info(&h, FileDispositionInfo, &delete) {
        Ok(()) => h.close(),
        Err(code) => {
            // The item stays: give the flag back. If that fails, the skip names the restore's
            // error, because the item has lost its read-only flag.
            set_attrs(&h, (attrs & SETTABLE) | FILE_ATTRIBUTE_READONLY)?;
            Err(code)
        }
    }
}

/// Sets the open item's attributes; times are left as they are (0).
fn set_attrs(h: &Handle, attrs: u32) -> Result<(), u32> {
    set_info(h, FileBasicInfo, &FILE_BASIC_INFO { FileAttributes: attrs, ..FILE_BASIC_INFO::default() })
}

/// `SetFileInformationByHandle` with `info`, which must be the structure `class` takes.
fn set_info<T>(h: &Handle, class: FILE_INFO_BY_HANDLE_CLASS, info: &T) -> Result<(), u32> {
    // SAFETY: `info` is valid for `size_of::<T>()` bytes across the call, which only reads it.
    let ok = unsafe { SetFileInformationByHandle(h.0, class, (&raw const *info).cast(), size_of::<T>() as u32) };
    if ok == 0 { Err(last_error()) } else { Ok(()) }
}

fn to_path(p: &[u16]) -> PathBuf {
    PathBuf::from(std::ffi::OsString::from_wide(p))
}

fn nul(p: &[u16]) -> Vec<u16> {
    p.iter().copied().chain(Some(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: reads the calling thread's last-error value; no arguments.
    unsafe { GetLastError() }
}

fn skip(path: &[u16], code: u32) -> Skipped {
    Skipped { path: to_path(path), reason: reason_from_win32(code as i32) }
}

/// Real file system tests. Every path is under one unique directory in the temp folder that the
/// test creates and removes. A case runs only when the OS creates its fixture; a case it cannot
/// run says so on the real stderr (`skipped`), so a skip is never a silent pass.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::delete::SkipReason;
    use scan::NodeId;
    use std::fs;

    /// A unique directory, canonical (verbatim) form. Removed on drop; a link inside is removed
    /// first, so nothing outside is ever followed.
    struct Dir {
        base: PathBuf,
        links: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl Dir {
        fn new(tag: &str) -> Dir {
            let plain = std::env::temp_dir().join(format!("volatus-purge-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&plain);
            fs::create_dir_all(&plain).expect("temp dir");
            Dir { base: fs::canonicalize(&plain).expect("canonical"), links: Default::default() }
        }
        /// `mklink /J`. `None` (skip) if the OS refuses.
        fn junction(&self, link: &Path, to: &Path) -> Option<()> {
            let ok = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(plain(link))
                .arg(plain(to))
                .output()
                .is_ok_and(|o| o.status.success());
            if ok {
                self.links.borrow_mut().push(link.to_path_buf());
                Some(())
            } else {
                skipped("junction inside the target (mklink /J was refused)");
                None
            }
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
        let _ = writeln!(std::io::stderr(), "SKIPPED real file system purge test: {case}");
    }

    /// `mklink` does not take the `\\?\` form.
    fn plain(p: &Path) -> PathBuf {
        PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\"))
    }

    fn target(path: &Path, is_dir: bool) -> DeleteTarget {
        DeleteTarget { node: NodeId(0), path: path.to_path_buf(), is_dir, is_link: false, size: 0, files: 0, dirs: 0 }
    }

    fn run(t: &DeleteTarget) -> Outcome {
        Purge.remove(t, &|_, _| {}, &AtomicBool::new(false))
    }

    /// Files and folders under `p`, `p` itself included.
    fn count(p: &Path) -> u64 {
        let mut n = 1;
        if fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
            for e in fs::read_dir(p).unwrap() {
                n += count(&e.unwrap().path());
            }
        }
        n
    }

    /// 10 folders of 10 folders of 10 files: 1000 files, 110 folders, sizes 0 to 16 bytes.
    /// Returns (items with the target folder, bytes).
    fn make_tree(t: &Path) -> (u64, u64) {
        let (mut items, mut bytes) = (1, 0);
        for a in 0..10 {
            for b in 0..10 {
                let d = t.join(format!("a{a}")).join(format!("b{b}"));
                fs::create_dir_all(&d).unwrap();
                for f in 0..10 {
                    let size = (a * 100 + b * 10 + f) % 17;
                    fs::write(d.join(format!("f{f}.txt")), vec![b'x'; size]).unwrap();
                    items += 1;
                    bytes += size as u64;
                }
            }
        }
        (items + 110, bytes)
    }

    #[test]
    fn removes_a_tree_of_1000_small_files_with_exact_counts() {
        let d = Dir::new("tree");
        let t = d.base.join("t");
        let (items, bytes) = make_tree(&t);
        let last = std::cell::Cell::new((0, 0));
        let o = Purge.remove(&target(&t, true), &|i, b| last.set((i, b)), &AtomicBool::new(false));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert!(!o.stopped);
        assert_eq!((o.removed_items, o.removed_bytes), (items, bytes));
        assert_eq!(last.get(), (items, bytes), "the last progress call has the totals");
        assert!(!t.exists());
    }

    #[test]
    fn a_read_only_file_is_removed() {
        let d = Dir::new("readonly");
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        let f = t.join("ro.txt");
        fs::write(&f, b"12345").unwrap();
        let mut perm = fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&f, perm).unwrap();
        let o = run(&target(&t, true));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (2, 5));
        assert!(!t.exists());
    }

    #[test]
    fn a_file_held_open_without_share_delete_is_skipped_as_locked() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let d = Dir::new("locked");
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        let held = t.join("held.txt");
        fs::write(&held, b"held").unwrap();
        fs::write(t.join("free.txt"), b"free").unwrap();
        let handle = fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&held).unwrap();
        let o = run(&target(&t, true));
        drop(handle);
        // One skip only: the folder that stays holds it, so the folder gets no skip of its own.
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, held);
        assert_eq!(o.skipped[0].reason, SkipReason::Locked);
        assert_eq!((o.removed_items, o.removed_bytes), (1, 4));
        assert!(held.exists() && !t.join("free.txt").exists());
        assert!(t.is_dir(), "the folder that holds the skipped file stays");
    }

    #[test]
    fn a_junction_inside_is_removed_and_its_target_stays() {
        let d = Dir::new("junction");
        let other = d.base.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("keep"), b"keep me").unwrap();
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        fs::write(t.join("own.txt"), b"abc").unwrap();
        if d.junction(&t.join("j"), &other).is_none() {
            return;
        }
        let o = run(&target(&t, true));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        // own.txt, the junction and the folder; the junction's target is not counted.
        assert_eq!((o.removed_items, o.removed_bytes), (3, 3));
        assert!(!t.exists());
        assert_eq!(fs::read(other.join("keep")).unwrap(), b"keep me");
    }

    #[test]
    fn a_path_longer_than_260_characters_is_removed() {
        let d = Dir::new("long");
        let t = d.base.join("t");
        let deep = t.join("a".repeat(100)).join("b".repeat(100)).join("c".repeat(100));
        fs::create_dir_all(&deep).unwrap();
        let f = deep.join("deep.txt");
        fs::write(&f, b"deep").unwrap();
        assert!(f.as_os_str().len() > 260, "{}", f.as_os_str().len());
        let o = run(&target(&t, true));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (5, 4));
        assert!(!t.exists());
    }

    #[test]
    fn stop_mid_run_returns_stopped_and_removed_items_stay_removed() {
        let d = Dir::new("stop");
        let t = d.base.join("t");
        let (items, _) = make_tree(&t);
        let stop = AtomicBool::new(false);
        let calls = std::cell::Cell::new(0);
        // Set from the progress callback after the first report: no clock involved.
        let progress = |i: u64, _: u64| {
            calls.set(calls.get() + 1);
            if i > 0 {
                stop.store(true, Ordering::SeqCst);
            }
        };
        let o = Purge.remove(&target(&t, true), &progress, &stop);
        assert!(o.stopped);
        assert!(calls.get() > 0);
        assert!(o.removed_items > 0 && o.removed_items < items, "{}", o.removed_items);
        assert!(t.is_dir(), "the target stays when the run stops early");
        assert_eq!(count(&t), items - o.removed_items, "what the outcome counts is gone from the disk");
    }

    #[test]
    fn a_target_that_no_longer_exists_gives_one_skip() {
        let d = Dir::new("gone");
        let t = d.base.join("gone");
        let o = run(&target(&t, true));
        assert_eq!((o.removed_items, o.removed_bytes), (0, 0));
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, t);
        assert!(!o.stopped);
    }

    #[test]
    fn a_target_that_is_a_junction_is_removed_as_the_link() {
        let d = Dir::new("rootjunction");
        let other = d.base.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("keep"), b"keep me").unwrap();
        let link = d.base.join("link");
        if d.junction(&link, &other).is_none() {
            return;
        }
        let o = run(&target(&link, true));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (1, 0));
        assert!(fs::symlink_metadata(&link).is_err(), "the junction is gone");
        assert_eq!(fs::read(other.join("keep")).unwrap(), b"keep me");
    }

    #[test]
    fn a_folder_that_fills_during_the_run_stays_with_a_skip() {
        let d = Dir::new("fills");
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        fs::write(t.join("a.txt"), b"a").unwrap();
        // The progress call after the files pass comes before the folders pass: a file written
        // then makes the target not empty, with no clock involved.
        let late = t.join("late.txt");
        let o = Purge.remove(&target(&t, true), &|_, _| fs::write(&late, b"late").unwrap(), &AtomicBool::new(false));
        assert!(t.is_dir() && late.exists());
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, t);
        assert_eq!((o.removed_items, o.removed_bytes), (1, 1));
    }

    #[test]
    fn only_a_name_surrogate_reparse_point_is_a_link() {
        let (reparse, dir) = (FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_DIRECTORY);
        for (attrs, tag, want) in [
            (0, 0, false),
            (dir, 0, false),
            // Junction and symlink: name surrogates.
            (reparse | dir, 0xA000_0003, true),
            (reparse, 0xA000_000C, true),
            (reparse | dir, 0xA000_000C, true),
            // Cloud file, dedup, app execution alias: not links.
            (reparse | dir, 0x9000_001A, false),
            (reparse, 0x9000_001A, false),
            (reparse, 0x8000_0013, false),
            (reparse, 0x8000_001B, false),
            // The tag bits mean nothing without the reparse attribute.
            (dir, 0xA000_0003, false),
            (0, 0xA000_000C, false),
            (0, 0x2000_0000, false),
        ] {
            assert_eq!(is_name_surrogate_link(attrs, tag), want, "attrs={attrs:#x} tag={tag:#x}");
        }
    }

    #[test]
    fn a_folder_needs_its_own_skip_only_when_no_skip_lies_in_it() {
        let s = |p: &str| Skipped { path: PathBuf::from(p), reason: SkipReason::Locked };
        let folder = Path::new(r"\\?\C:\t\a");
        for (skips, want) in [
            (vec![], true),
            (vec![s(r"\\?\C:\t\a\x.txt")], false),
            (vec![s(r"\\?\C:\t\a\b\deep.txt")], false),
            (vec![s(r"\\?\C:\t\a")], false),
            // A sibling whose name only starts like the folder is not inside it.
            (vec![s(r"\\?\C:\t\ab\x.txt")], true),
            (vec![s(r"\\?\C:\t\x.txt")], true),
            (vec![s(r"\\?\C:\t\b\x.txt"), s(r"\\?\C:\t\a\y.txt")], false),
        ] {
            assert_eq!(needs_skip_for_folder(folder, &skips), want, "{skips:?}");
        }
    }

    fn run_with(t: &DeleteTarget, opts: &Options) -> Outcome {
        remove_with(t, &|_, _| {}, &AtomicBool::new(false), opts)
    }

    /// The target `t` holds `sub\inner.txt` ("real"); `decoy\inner.txt` ("decoy") lies beside
    /// it, so a delete that went through a junction at `t\sub` would find the same name there.
    fn swap_fixture(d: &Dir) -> (PathBuf, PathBuf, PathBuf) {
        let decoy = d.base.join("decoy");
        fs::create_dir(&decoy).unwrap();
        fs::write(decoy.join("inner.txt"), b"decoy").unwrap();
        let t = d.base.join("t");
        let sub = t.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("inner.txt"), b"real").unwrap();
        (t, sub, decoy)
    }

    #[test]
    fn a_folder_swapped_for_a_junction_after_the_listing_is_removed_as_the_link() {
        let d = Dir::new("swap");
        let (t, sub, decoy) = swap_fixture(&d);
        let swapped = std::cell::Cell::new(None::<bool>);
        // The target is listed (`sub` is in the listing as a real folder) and nothing in it is
        // open yet: replace `sub` with a junction to the decoy.
        let hook = |p: &Path| {
            if p == t.as_path() && swapped.get().is_none() {
                fs::remove_dir_all(&sub).unwrap();
                swapped.set(Some(d.junction(&sub, &decoy).is_some()));
            }
        };
        let o = run_with(&target(&t, true), &Options { hook: Some(&hook), ..Options::default() });
        match swapped.get() {
            None => panic!("the hook was not called for the target"),
            Some(false) => return,
            Some(true) => {}
        }
        assert!(decoy.join("inner.txt").exists(), "nothing behind the junction is touched");
        assert_eq!(fs::read(decoy.join("inner.txt")).unwrap(), b"decoy");
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (2, 0), "the junction and the target");
        assert!(fs::symlink_metadata(&sub).is_err() && !t.exists());
    }

    #[test]
    fn a_folder_the_run_works_in_cannot_be_moved_out_of_the_target() {
        let d = Dir::new("pin");
        let (t, sub, decoy) = swap_fixture(&d);
        let moved = d.base.join("moved");
        let (tried, moved_away) = (std::cell::Cell::new(false), std::cell::Cell::new(false));
        // `sub` is listed and its file not yet removed: move `sub` out of the target and put a
        // junction to the decoy in its place.
        let hook = |p: &Path| {
            if p == sub.as_path() {
                tried.set(true);
                if fs::rename(&sub, &moved).is_ok() {
                    moved_away.set(true);
                    let _ = d.junction(&sub, &decoy);
                }
            }
        };
        let o = run_with(&target(&t, true), &Options { hook: Some(&hook), ..Options::default() });
        assert!(tried.get(), "the hook was called for the subfolder");
        assert!(decoy.join("inner.txt").exists(), "nothing behind a junction is touched");
        assert!(!moved_away.get(), "a folder the run holds open cannot be moved away");
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (3, 4));
        assert!(!t.exists());
    }

    #[test]
    fn a_directory_symlink_inside_is_removed_and_its_target_stays() {
        let d = Dir::new("symlink");
        let other = d.base.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("keep"), b"keep me").unwrap();
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        fs::write(t.join("own.txt"), b"abc").unwrap();
        let link = t.join("s");
        if std::os::windows::fs::symlink_dir(&other, &link).is_err() {
            skipped("directory symlink inside the target (needs Developer Mode or administrator)");
            return;
        }
        d.links.borrow_mut().push(link);
        let o = run(&target(&t, true));
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (3, 3));
        assert!(!t.exists());
        assert_eq!(fs::read(other.join("keep")).unwrap(), b"keep me");
    }

    #[test]
    fn a_read_only_folder_and_file_are_removed_with_and_without_posix_semantics() {
        for posix in [true, false] {
            let d = Dir::new(if posix { "ro-posix" } else { "ro-classic" });
            let t = d.base.join("t");
            let sub = t.join("ro-dir");
            fs::create_dir_all(&sub).unwrap();
            let f = sub.join("ro.txt");
            fs::write(&f, b"12345").unwrap();
            for p in [&f, &sub] {
                let mut perm = fs::metadata(p).unwrap().permissions();
                perm.set_readonly(true);
                fs::set_permissions(p, perm).unwrap();
            }
            let o = run_with(&target(&t, true), &Options { posix, ..Options::default() });
            assert!(o.skipped.is_empty(), "posix={posix}: {:?}", o.skipped);
            assert_eq!((o.removed_items, o.removed_bytes), (3, 5), "posix={posix}");
            assert!(!t.exists(), "posix={posix}");
        }
    }

    #[test]
    fn a_file_held_open_with_share_delete_goes_at_once_with_its_folder() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE};
        let d = Dir::new("sharedelete");
        let t = d.base.join("t");
        fs::create_dir(&t).unwrap();
        let f = t.join("open.txt");
        fs::write(&f, b"open").unwrap();
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        let handle = fs::OpenOptions::new().read(true).share_mode(share).open(&f).unwrap();
        let o = run(&target(&t, true));
        // The POSIX delete takes the name away while the file is still open elsewhere.
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!((o.removed_items, o.removed_bytes), (2, 4));
        assert!(!t.exists());
        drop(handle);
    }

    /// Opens the folder `p` as a terminal holds its working folder: list access, shared for
    /// read and write only, so no one else can open it for delete.
    fn hold_folder(p: &Path) -> fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)
            .unwrap()
    }

    #[test]
    fn a_folder_held_open_without_share_delete_is_emptied_and_stays_locked() {
        let d = Dir::new("heldfolder");
        let t = d.base.join("t");
        let held = t.join("held");
        fs::create_dir_all(&held).unwrap();
        fs::write(held.join("x.txt"), b"xx").unwrap();
        fs::write(held.join("y.txt"), b"yyy").unwrap();
        fs::write(t.join("a.txt"), b"a").unwrap();
        let h = hold_folder(&held);
        let o = run(&target(&t, true));
        drop(h);
        assert!(!held.join("x.txt").exists() && !held.join("y.txt").exists(), "the files inside are gone");
        assert!(held.is_dir(), "the held folder stays");
        assert!(!t.join("a.txt").exists());
        // One skip: the held folder. The target stays because of it and gets no skip of its own.
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, held);
        assert_eq!(o.skipped[0].reason, SkipReason::Locked);
        assert_eq!((o.removed_items, o.removed_bytes), (3, 6));
    }

    #[test]
    fn a_target_held_open_without_share_delete_is_emptied_and_stays_locked() {
        let d = Dir::new("heldroot");
        let t = d.base.join("t");
        fs::create_dir_all(t.join("sub")).unwrap();
        fs::write(t.join("sub").join("x.txt"), b"xx").unwrap();
        fs::write(t.join("y.txt"), b"yyy").unwrap();
        let h = hold_folder(&t);
        let o = run(&target(&t, true));
        drop(h);
        assert!(!t.join("sub").exists() && !t.join("y.txt").exists(), "the contents are gone");
        assert!(t.is_dir(), "the held target stays");
        assert_eq!(o.skipped.len(), 1, "{:?}", o.skipped);
        assert_eq!(o.skipped[0].path, t);
        assert_eq!(o.skipped[0].reason, SkipReason::Locked);
        assert_eq!((o.removed_items, o.removed_bytes), (3, 5));
    }

    // ---- Benchmark (ignored; docs/BENCHMARK.md) ----
    // cargo test -p app -j 1 --release purge_bench -- --ignored --nocapture --test-threads 1

    /// 10 x 10 x 10 folders with 100 files each: 100,000 files of 1 to 64 bytes in 1,110
    /// folders. Returns the items with the target folder.
    fn make_bench_tree(t: &Path) -> u64 {
        let mut leaves = Vec::new();
        for a in 0..10 {
            for b in 0..10 {
                for c in 0..10 {
                    leaves.push(t.join(format!("a{a}")).join(format!("b{b}")).join(format!("c{c}")));
                }
            }
        }
        leaves.par_iter().enumerate().for_each(|(i, d)| {
            fs::create_dir_all(d).unwrap();
            for f in 0..100 {
                fs::write(d.join(format!("f{f:03}.dat")), vec![b'x'; (i * 100 + f) % 64 + 1]).unwrap();
            }
        });
        1 + 1110 + 100_000
    }

    fn best(times: &[std::time::Duration]) -> f64 {
        times.iter().map(|d| d.as_secs_f64() * 1000.0).fold(f64::INFINITY, f64::min)
    }

    fn time_purge(t: &Path, items: u64, opts: &Options) -> std::time::Duration {
        let start = std::time::Instant::now();
        let o = remove_with(&target(t, true), &|_, _| {}, &AtomicBool::new(false), opts);
        let took = start.elapsed();
        assert!(o.skipped.is_empty() && !o.stopped, "{:?}", o.skipped);
        assert_eq!(o.removed_items, items);
        assert!(!t.exists());
        took
    }

    /// `cmd /c rd /s /q`, wall time of the child process (its start-up is printed apart).
    fn time_rd(t: &Path) -> std::time::Duration {
        let start = std::time::Instant::now();
        let s = std::process::Command::new("cmd").args(["/c", "rd", "/s", "/q"]).arg(plain(t)).status().unwrap();
        let took = start.elapsed();
        assert!(s.success() && !t.exists());
        took
    }

    /// Windows PowerShell 5.1 `Remove-Item -Recurse -Force`, timed inside PowerShell with
    /// `Measure-Command`, so PowerShell's start-up is not in the number.
    fn time_remove_item(t: &Path) -> std::time::Duration {
        let p = plain(t).to_string_lossy().replace('\'', "''");
        let script = format!("(Measure-Command {{ Remove-Item -LiteralPath '{p}' -Recurse -Force }}).TotalMilliseconds");
        let o = std::process::Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]).output().unwrap();
        assert!(o.status.success() && !t.exists(), "{}", String::from_utf8_lossy(&o.stderr));
        let ms: f64 = String::from_utf8_lossy(&o.stdout).trim().replace(',', ".").parse().unwrap();
        std::time::Duration::from_secs_f64(ms / 1000.0)
    }

    /// The shell's permanent delete: `SHFileOperationW` `FO_DELETE` with no UI and no
    /// `FOF_ALLOWUNDO`, in this process.
    fn time_shell(t: &Path) -> std::time::Duration {
        use windows_sys::Win32::UI::Shell::{FO_DELETE, FOF_NO_UI, SHFILEOPSTRUCTW, SHFileOperationW};
        let from: Vec<u16> = plain(t).as_os_str().encode_wide().chain([0, 0]).collect();
        let mut op = SHFILEOPSTRUCTW {
            hwnd: std::ptr::null_mut(),
            wFunc: FO_DELETE,
            pFrom: from.as_ptr(),
            pTo: std::ptr::null(),
            fFlags: FOF_NO_UI as u16,
            fAnyOperationsAborted: 0,
            hNameMappings: std::ptr::null_mut(),
            lpszProgressTitle: std::ptr::null(),
        };
        let start = std::time::Instant::now();
        // SAFETY: `op` is a filled SHFILEOPSTRUCTW; `from` is double-NUL terminated and outlives the call.
        let r = unsafe { SHFileOperationW(&mut op) };
        let took = start.elapsed();
        assert!(r == 0 && op.fAnyOperationsAborted == 0 && !t.exists(), "SHFileOperationW {r}");
        took
    }

    #[test]
    #[ignore = "benchmark: 12 trees of 100,000 files; run by hand"]
    fn purge_bench_against_other_deletes() {
        let d = Dir::new("bench");
        let t = d.base.join("t");
        let names = ["Purge", "rd /s /q", "Remove-Item", "SHFileOperationW"];
        let mut times = vec![Vec::new(); names.len()];
        for round in 0..3 {
            // Each round starts with a different method, so none always runs first.
            for m in (0..names.len()).map(|i| (round + i) % names.len()) {
                let name = names[m];
                let items = make_bench_tree(&t);
                let took = match m {
                    0 => time_purge(&t, items, &Options::default()),
                    1 => time_rd(&t),
                    2 => time_remove_item(&t),
                    _ => time_shell(&t),
                };
                eprintln!("round {round} {name}: {:.0} ms", took.as_secs_f64() * 1000.0);
                times[m].push(took);
            }
        }
        let cmd: Vec<_> = (0..3)
            .map(|_| {
                let start = std::time::Instant::now();
                std::process::Command::new("cmd").args(["/c", "rem"]).status().unwrap();
                start.elapsed()
            })
            .collect();
        for (m, name) in names.iter().enumerate() {
            eprintln!("BEST {name}: {:.0} ms", best(&times[m]));
        }
        eprintln!("BEST cmd /c rem (start-up in the rd number): {:.0} ms", best(&cmd));
    }

    #[test]
    #[ignore = "benchmark: tuning sweep of the fast delete; run by hand"]
    fn purge_bench_tuning() {
        let d = Dir::new("bench-tune");
        let t = d.base.join("t");
        let configs: [(&str, Options); 9] = [
            ("default (global pool, batch 256, list 64 KiB, POSIX)", Options::default()),
            ("threads 4", Options { threads: 4, ..Options::default() }),
            ("threads 8", Options { threads: 8, ..Options::default() }),
            ("threads 16", Options { threads: 16, ..Options::default() }),
            ("threads 64", Options { threads: 64, ..Options::default() }),
            ("batch 1024", Options { batch: 1024, ..Options::default() }),
            ("batch 4096", Options { batch: 4096, ..Options::default() }),
            ("list 4 KiB", Options { list_buf: 4096, ..Options::default() }),
            ("classic delete (no POSIX)", Options { posix: false, ..Options::default() }),
        ];
        let mut times = vec![Vec::new(); configs.len()];
        for _ in 0..3 {
            for (i, (_, opts)) in configs.iter().enumerate() {
                let items = make_bench_tree(&t);
                times[i].push(time_purge(&t, items, opts));
            }
        }
        for (i, (name, _)) in configs.iter().enumerate() {
            eprintln!("BEST {name}: {:.0} ms", best(&times[i]));
        }
    }
}

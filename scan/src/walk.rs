//! Fallback parallel directory walker.
//!
//! `FindFirstFileExW` with a large fetch buffer lists one directory; rayon fans out over
//! sub-directories. Reparse points (junctions, symlinks) are recorded but never followed.
//!
//! ponytail: Windows-only, no `\\?\` long-path prefix (paths over MAX_PATH are skipped
//! and counted), and `allocated` equals `size` because the find data has no cluster size.
//! Recursion runs on rayon worker stacks; extremely deep trees need a bigger stack size.

use crate::rollup::{RawEntry, rollup};
use crate::{Method, Progress, ScanError, ScanStats, Tree};
use rayon::prelude::*;
use std::ffi::{OsString, c_void};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FIND_FIRST_EX_LARGE_FETCH, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
    WIN32_FIND_DATAW,
};

const ROOT_ID: u64 = 0;

struct Item {
    name: OsString,
    is_dir: bool,
    reparse: bool,
    size: u64,
}

/// Lists one directory (without `.` and `..`). An error means the listing is incomplete.
fn list_dir(dir: &Path) -> io::Result<Vec<Item>> {
    let pattern: Vec<u16> = dir.join("*").as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: WIN32_FIND_DATAW is plain data; all-zero is a valid value.
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    // SAFETY: `pattern` is NUL-terminated and `data` is a valid out-pointer for this info level.
    let handle = unsafe {
        FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            (&mut data as *mut WIN32_FIND_DATAW).cast::<c_void>(),
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut items = Vec::new();
    let result = loop {
        let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
        let name = OsString::from_wide(&data.cFileName[..len]);
        if name != "." && name != ".." {
            let reparse = data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            items.push(Item {
                is_dir: data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 && !reparse,
                reparse,
                size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
                name,
            });
        }
        // SAFETY: `handle` is a live search handle and `data` a valid out-pointer.
        if unsafe { FindNextFileW(handle, &mut data) } == 0 {
            // SAFETY: reads the calling thread's last-error value.
            break match unsafe { GetLastError() } {
                ERROR_NO_MORE_FILES => Ok(items),
                _ => Err(io::Error::last_os_error()),
            };
        }
    };
    // SAFETY: `handle` came from FindFirstFileExW and is closed exactly once.
    unsafe { FindClose(handle) };
    result
}

struct Ctx<'a> {
    next_id: AtomicU64,
    files: AtomicU64,
    dirs: AtomicU64,
    progress: &'a Sender<Progress>,
    cancel: &'a AtomicBool,
}

#[derive(Default)]
struct Out {
    entries: Vec<RawEntry>,
    skipped: u64,
}

fn walk_dir(ctx: &Ctx, dir: &Path, dir_id: u64) -> Result<Out, ScanError> {
    if ctx.cancel.load(Ordering::Relaxed) {
        return Err(ScanError::Cancelled);
    }
    let items = match list_dir(dir) {
        Ok(items) => items,
        // An unreadable root is a scan failure, never an empty result.
        Err(e) if dir_id == ROOT_ID => return Err(ScanError::Io(e)),
        // Unreadable directory: keep the node, count it, carry on.
        Err(_) => return Ok(Out { entries: Vec::new(), skipped: 1 }),
    };
    let mut out = Out::default();
    let mut subdirs: Vec<(PathBuf, u64)> = Vec::new();
    let (mut files, mut dirs) = (0, 0);
    for item in items {
        let id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
        // A reparse point is kept as a zero-size file entry: its target is not ours to count.
        let size = if item.is_dir || item.reparse { 0 } else { item.size };
        if item.is_dir {
            dirs += 1;
            subdirs.push((dir.join(&item.name), id));
        } else {
            files += 1;
        }
        out.entries.push(RawEntry {
            id,
            parent: dir_id,
            name: item.name.to_string_lossy().into_owned(),
            size,
            allocated: size,
            is_dir: item.is_dir,
        });
    }
    let files = ctx.files.fetch_add(files, Ordering::Relaxed) + files;
    let dirs = ctx.dirs.fetch_add(dirs, Ordering::Relaxed) + dirs;
    let _ = ctx.progress.send(Progress { files, dirs: dirs + 1 }); // +1: the root

    let children: Vec<Out> = subdirs
        .par_iter()
        .map(|(path, id)| walk_dir(ctx, path, *id))
        .collect::<Result<_, _>>()?;
    for c in children {
        out.entries.extend(c.entries);
        out.skipped += c.skipped;
    }
    Ok(out)
}

/// Scans `root` with the directory walker only (no MFT attempt).
pub fn walk(
    root: &Path,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
) -> Result<(Tree, ScanStats), ScanError> {
    let started = Instant::now();
    if !std::fs::metadata(root).map_err(ScanError::Io)?.is_dir() {
        return Err(ScanError::Io(io::Error::new(io::ErrorKind::InvalidInput, "root is not a directory")));
    }
    let ctx = Ctx {
        next_id: AtomicU64::new(ROOT_ID + 1),
        files: AtomicU64::new(0),
        dirs: AtomicU64::new(0),
        progress,
        cancel,
    };
    let mut out = walk_dir(&ctx, root, ROOT_ID)?;
    out.entries.push(RawEntry {
        id: ROOT_ID,
        parent: ROOT_ID,
        name: root.display().to_string(),
        size: 0,
        allocated: 0,
        is_dir: true,
    });
    let rolled = rollup(ROOT_ID, out.entries)?;
    let stats = ScanStats {
        files: rolled.files,
        dirs: rolled.dirs,
        skipped: out.skipped + rolled.skipped,
        elapsed: started.elapsed(),
        method: Method::Walk,
    };
    Ok((rolled.tree, stats))
}

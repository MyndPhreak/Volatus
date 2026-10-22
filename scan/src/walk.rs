//! Fallback parallel directory walker.
//!
//! `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)` lists one directory with each
//! entry's file ID; rayon fans out over sub-directories. Name-surrogate reparse points
//! (junctions, symlinks) are recorded but never followed; other reparse directories (cloud
//! folders) are descended, as on the MFT path. A file with several hard links counts once,
//! under the first name in walk order.
//!
//! Directories open through `std::fs::OpenOptions`, so paths over MAX_PATH are listed too.
//!
//! ponytail: Windows-only, and `allocated` equals `size` (the listing's allocation size is
//! not used yet).
//! Recursion runs on rayon worker stacks; extremely deep trees need a bigger stack size.

use crate::mft::{NAME_SURROGATE, u32_at, u64_at};
use crate::rollup::{IdHasher, RawEntry, rollup};
use crate::{Method, Phases, Progress, ScanError, ScanStats, Tree};
use rayon::prelude::*;
use std::collections::HashSet;
use std::hash::BuildHasherDefault;
use std::ffi::OsString;
use std::io;
use std::mem::offset_of;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, GetLastError};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_BOTH_DIR_INFO,
    FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdBothDirectoryInfo,
    GetFileInformationByHandleEx,
};

const ROOT_ID: u64 = 0;
/// Bytes per directory listing call; one call returns as many entries as fit.
const LIST_BUF: usize = 64 * 1024;

struct Item {
    name: OsString,
    is_dir: bool,
    size: u64,
    file_id: u64,
}

/// Appends the entries of one `FILE_ID_BOTH_DIR_INFO` buffer, without `.` and `..`.
/// `None` when an entry runs past the buffer.
fn parse_listing(b: &[u8], items: &mut Vec<Item>) -> Option<()> {
    type Info = FILE_ID_BOTH_DIR_INFO;
    let mut off = 0usize;
    loop {
        let attrs = u32_at(b, off + offset_of!(Info, FileAttributes))?;
        let name_len = u32_at(b, off + offset_of!(Info, FileNameLength))? as usize;
        let name_at = off + offset_of!(Info, FileName);
        let raw = b.get(name_at..name_at.checked_add(name_len)?)?;
        let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
        let name = OsString::from_wide(&units);
        if name != "." && name != ".." {
            // With the reparse attribute set, EaSize holds the reparse tag.
            let link = attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
                && u32_at(b, off + offset_of!(Info, EaSize))? & NAME_SURROGATE != 0;
            items.push(Item {
                is_dir: attrs & FILE_ATTRIBUTE_DIRECTORY != 0 && !link,
                size: u64_at(b, off + offset_of!(Info, EndOfFile))?,
                file_id: u64_at(b, off + offset_of!(Info, FileId))?,
                name,
            });
        }
        match u32_at(b, off + offset_of!(Info, NextEntryOffset))? {
            0 => return Some(()),
            next => off = off.checked_add(next as usize)?,
        }
    }
}

/// Lists one directory (without `.` and `..`). An error means the listing is incomplete:
/// `Cancelled` when `cancel` is set, `Io` when the directory cannot be read.
/// The flag is checked before each listing call: one huge directory (10k entries in
/// `C:\Windows\servicing\Packages`) takes over 1 s to list in full.
fn list_dir(dir: &Path, cancel: &AtomicBool) -> Result<Vec<Item>, ScanError> {
    let handle = std::fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS) // required to open a directory
        .open(dir)
        .map_err(ScanError::Io)?;
    // u64 elements keep the buffer 8-byte aligned, as the listing entries require.
    let mut buf = vec![0u64; LIST_BUF / 8];
    let mut items = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ScanError::Cancelled);
        }
        // SAFETY: `handle` is an open directory and `buf` is writable for LIST_BUF bytes.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle.as_raw_handle(),
                FileIdBothDirectoryInfo,
                buf.as_mut_ptr().cast(),
                LIST_BUF as u32,
            )
        };
        if ok == 0 {
            // SAFETY: reads the calling thread's last-error value.
            return match unsafe { GetLastError() } {
                ERROR_NO_MORE_FILES => Ok(items),
                _ => Err(ScanError::Io(io::Error::last_os_error())),
            };
        }
        // SAFETY: `buf` is initialised and LIST_BUF bytes long; u8 has no alignment needs.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), LIST_BUF) };
        parse_listing(bytes, &mut items).ok_or_else(|| {
            ScanError::Io(io::Error::new(io::ErrorKind::InvalidData, "malformed directory listing"))
        })?;
    }
}

/// Keeps the first entry of each file ID, so a hard-linked file counts once (as on the MFT
/// path). ID 0 and `u64::MAX` (no 64-bit ID, as on some ReFS files) are never merged.
/// ponytail: one set slot per file; memory grows with the file count.
fn drop_extra_links(entries: Vec<(RawEntry, u64)>) -> Vec<RawEntry> {
    let mut seen: HashSet<u64, BuildHasherDefault<IdHasher>> = HashSet::default();
    entries
        .into_iter()
        .filter(|&(_, id)| id == 0 || id == u64::MAX || seen.insert(id))
        .map(|(e, _)| e)
        .collect()
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
    /// Entries with their file ID (0 for directories).
    entries: Vec<(RawEntry, u64)>,
    skipped: u64,
}

fn walk_dir(ctx: &Ctx, dir: &Path, dir_id: u64) -> Result<Out, ScanError> {
    if ctx.cancel.load(Ordering::Relaxed) {
        return Err(ScanError::Cancelled);
    }
    let items = match list_dir(dir, ctx.cancel) {
        Ok(items) => items,
        Err(ScanError::Cancelled) => return Err(ScanError::Cancelled),
        // An unreadable root is a scan failure, never an empty result.
        Err(e) if dir_id == ROOT_ID => return Err(e),
        // Unreadable directory: keep the node, count it, carry on.
        Err(_) => return Ok(Out { entries: Vec::new(), skipped: 1 }),
    };
    let mut out = Out::default();
    let mut subdirs: Vec<(PathBuf, u64)> = Vec::new();
    let (mut files, mut dirs) = (0, 0);
    for item in items {
        let id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
        // A junction or symlink is a file entry and never followed. A file's reported size is
        // its own logical size: 0 for junctions and symlinks, the full size for placeholders.
        let size = if item.is_dir { 0 } else { item.size };
        if item.is_dir {
            dirs += 1;
            subdirs.push((dir.join(&item.name), id));
        } else {
            files += 1;
        }
        let entry = RawEntry {
            id,
            parent: dir_id,
            name: item.name.to_string_lossy().into_owned(),
            size,
            allocated: size,
            is_dir: item.is_dir,
        };
        out.entries.push((entry, if item.is_dir { 0 } else { item.file_id }));
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
    let out = walk_dir(&ctx, root, ROOT_ID)?;
    let read = started.elapsed();
    let mut entries = drop_extra_links(out.entries);
    entries.push(RawEntry {
        id: ROOT_ID,
        parent: ROOT_ID,
        name: root.display().to_string(),
        size: 0,
        allocated: 0,
        is_dir: true,
    });
    let rolled = rollup(ROOT_ID, entries)?;
    let phases = Phases { read, rollup: started.elapsed() - read, ..Phases::default() };
    let stats = ScanStats {
        files: rolled.files,
        dirs: rolled.dirs,
        skipped: out.skipped + rolled.skipped,
        elapsed: started.elapsed(),
        method: Method::Walk,
        phases,
        threads: rayon::current_num_threads(),
    };
    Ok((rolled.tree, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: u64) -> RawEntry {
        RawEntry { id, parent: ROOT_ID, name: format!("f{id}"), size: 10, allocated: 10, is_dir: false }
    }

    #[test]
    fn drop_extra_links_unknown_file_ids_are_never_merged() {
        let entries = vec![(file(1), 0), (file(2), 0), (file(3), u64::MAX), (file(4), u64::MAX), (file(5), 7), (file(6), 7)];

        let kept: Vec<u64> = drop_extra_links(entries).iter().map(|e| e.id).collect();

        assert_eq!(kept, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn list_dir_cancel_set_returns_cancelled() {
        // One huge directory (C:\Windows\servicing\Packages, 10k entries) takes 1-2 s to list;
        // the listing loop must stop on cancel, not only the walk between directories.
        let cancel = AtomicBool::new(true);

        let got = list_dir(Path::new(env!("CARGO_MANIFEST_DIR")), &cancel);

        assert!(matches!(got, Err(ScanError::Cancelled)), "{:?}", got.map(|v| v.len()));
    }
}

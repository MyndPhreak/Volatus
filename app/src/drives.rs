//! The drives the start screen offers: ready fixed and removable drives, with their size.
//! The Win32 calls sit behind `DriveApi`, so the rules are unit tested with a fake.

use crate::model::format_size;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode};
use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};

/// The note under the drive list while the app runs without administrator rights.
const ADMIN_NOTE: &str = "Scans of a whole NTFS drive are faster as administrator.";
/// Longest volume label, in UTF-16 units, plus the end mark (`MAX_PATH + 1`).
const LABEL_UNITS: usize = 261;

/// The kinds of drive the start screen offers. Network, CD and RAM drives are left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
}

/// One ready drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveInfo {
    pub letter: char,
    /// Volume label, "" if the volume has none.
    pub label: String,
    pub kind: DriveKind,
    /// (total bytes, free bytes), or `None` when the query failed.
    pub space: Option<(u64, u64)>,
    /// The file system is NTFS: the fast MFT scan is possible (with administrator rights).
    pub is_ntfs: bool,
}

/// What `GetVolumeInformationW` tells about a ready volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Volume label, "" if the volume has none.
    pub label: String,
    pub is_ntfs: bool,
}

/// One glass button of the start screen. Text is ready to show.
#[derive(Debug, Clone, PartialEq)]
pub struct DriveButton {
    /// Root to scan: `C:\`.
    pub root: String,
    /// `C: Local Disk`.
    pub title: String,
    /// `412.0 GB free of 1.8 TB`, or `Size unknown.`.
    pub detail: String,
    /// Share of the drive in use, 0..=1. 0 when the size is unknown.
    pub used: f32,
    /// False when the size query failed: the button has no used/free bar.
    pub size_known: bool,
}

/// What the app asks Windows about drives. One method per call, so a fake can answer.
pub trait DriveApi {
    /// Bit `n` set when drive `A` + `n` exists (`GetLogicalDrives`). 0 when the call fails.
    fn logical_drives(&self) -> u32;
    /// `GetDriveTypeW` for a root such as `C:\`.
    fn drive_type(&self, root: &str) -> u32;
    /// Label and file system, or `None` when the drive is not ready (no disk, locked, asleep and failing).
    fn volume(&self, root: &str) -> Option<Volume>;
    /// `QueryDosDeviceW` target of a root such as `C:`, or "" when the call fails.
    fn dos_target(&self, root: &str) -> String;
    /// (total, free) bytes, or `None` when the query fails.
    fn space(&self, root: &str) -> Option<(u64, u64)>;
}

/// The kind of drive to offer, or `None` to leave it out. Only a ready fixed or removable drive is offered.
pub fn keep(drive_type: u32, ready: bool) -> Option<DriveKind> {
    match drive_type {
        DRIVE_FIXED if ready => Some(DriveKind::Fixed),
        DRIVE_REMOVABLE if ready => Some(DriveKind::Removable),
        _ => None,
    }
}

/// True when a DOS device target is a `subst` drive: `??C:Users`. A physical volume is `DeviceHarddiskVolume3`.
pub fn is_subst(target: &str) -> bool {
    let _ = target;
    false
}

/// The note under the drive list: shown when the app is not elevated and a listed drive is NTFS, else "".
pub fn admin_note(drives: &[DriveInfo], is_admin: bool) -> String {
    let _ = (drives, is_admin, ADMIN_NOTE);
    String::new()
}

/// The ready fixed and removable drives `api` reports, in letter order.
pub fn list_with(api: &impl DriveApi) -> Vec<DriveInfo> {
    let mask = api.logical_drives();
    ('A'..='Z')
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .filter_map(|(_, letter)| {
            let root = format!("{letter}:\\");
            // Only fixed and removable drives are asked for a label: a network drive can take long to answer.
            let drive_type = api.drive_type(&root);
            if !matches!(drive_type, DRIVE_FIXED | DRIVE_REMOVABLE) {
                return None;
            }
            let volume = api.volume(&root);
            let kind = keep(drive_type, volume.is_some())?;
            let (label, is_ntfs) = volume.map(|v| (v.label, v.is_ntfs)).unwrap_or_default();
            Some(DriveInfo { letter, label, kind, space: api.space(&root), is_ntfs })
        })
        .collect()
}

/// The ready fixed and removable drives of this PC. Can be slow (a sleeping disk): call it on a worker.
pub fn list() -> Vec<DriveInfo> {
    // No system dialog ("There is no disk in the drive") for an empty card reader.
    let mut old = 0;
    // SAFETY: `old` is a valid pointer for the call.
    let changed = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, &mut old) } != 0;
    let drives = list_with(&Win32Drives);
    if changed {
        // SAFETY: no pointers; `null_mut` means the old mode is not wanted.
        unsafe { SetThreadErrorMode(old, null_mut()) };
    }
    drives
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// `DriveApi` over the Win32 calls.
struct Win32Drives;

impl DriveApi for Win32Drives {
    fn logical_drives(&self) -> u32 {
        // SAFETY: no arguments.
        unsafe { GetLogicalDrives() }
    }

    fn drive_type(&self, root: &str) -> u32 {
        let root = wide(root);
        // SAFETY: `root` is NUL terminated and outlives the call.
        unsafe { GetDriveTypeW(root.as_ptr()) }
    }

    fn dos_target(&self, _root: &str) -> String {
        String::new()
    }

    fn volume(&self, root: &str) -> Option<Volume> {
        let root = wide(root);
        let mut label = [0u16; LABEL_UNITS];
        // SAFETY: `root` is NUL terminated; `label` has the length given; the other outputs are not wanted.
        let ok = unsafe {
            GetVolumeInformationW(root.as_ptr(), label.as_mut_ptr(), LABEL_UNITS as u32, null_mut(), null_mut(), null_mut(), null_mut(), 0)
        };
        let end = label.iter().position(|&u| u == 0).unwrap_or(LABEL_UNITS);
        (ok != 0).then(|| Volume { label: String::from_utf16_lossy(&label[..end]), is_ntfs: false })
    }

    fn space(&self, root: &str) -> Option<(u64, u64)> {
        let root = wide(root);
        let (mut total, mut free) = (0u64, 0u64);
        // SAFETY: `root` is NUL terminated; both outputs are valid for the call; the caller-quota output is not wanted.
        let ok = unsafe { GetDiskFreeSpaceExW(root.as_ptr(), null_mut(), &mut total, &mut free) };
        (ok != 0).then_some((total, free))
    }
}

/// The buttons for `drives`, sorted by letter.
pub fn drive_buttons(drives: &[DriveInfo]) -> Vec<DriveButton> {
    let mut sorted: Vec<&DriveInfo> = drives.iter().collect();
    sorted.sort_by_key(|d| d.letter);
    sorted
        .into_iter()
        .map(|d| {
            let name = match (d.label.as_str(), d.kind) {
                ("", DriveKind::Fixed) => "Local Disk",
                ("", DriveKind::Removable) => "USB drive",
                (label, _) => label,
            };
            let (detail, used) = match d.space {
                Some((total, free)) => {
                    let used = if total == 0 || free > total { 0.0 } else { ((total - free) as f64 / total as f64) as f32 };
                    (format!("{} free of {}", format_size(free), format_size(total)), used)
                }
                None => ("Size unknown.".to_string(), 0.0),
            };
            DriveButton {
                root: format!("{}:\\", d.letter),
                title: format!("{}: {name}", d.letter),
                detail,
                used,
                size_known: d.space.is_some(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ---- keep ----
    #[test]
    fn keep_every_drive_type_and_readiness_keeps_only_ready_fixed_and_removable() {
        // DRIVE_UNKNOWN 0, NO_ROOT_DIR 1, REMOVABLE 2, FIXED 3, REMOTE 4, CDROM 5, RAMDISK 6.
        for drive_type in 0..=6u32 {
            for ready in [false, true] {
                let want = match (drive_type, ready) {
                    (2, true) => Some(DriveKind::Removable),
                    (3, true) => Some(DriveKind::Fixed),
                    _ => None,
                };
                assert_eq!(keep(drive_type, ready), want, "type {drive_type}, ready {ready}");
            }
        }
        assert_eq!((DRIVE_REMOVABLE, DRIVE_FIXED), (2, 3));
    }

    // ---- list_with ----
    #[derive(Default)]
    struct Fake {
        mask: u32,
        types: HashMap<&'static str, u32>,
        labels: HashMap<&'static str, &'static str>,
        ntfs: Vec<&'static str>,
        dos: HashMap<&'static str, &'static str>,
        spaces: HashMap<&'static str, (u64, u64)>,
    }

    impl DriveApi for Fake {
        fn logical_drives(&self) -> u32 {
            self.mask
        }
        fn drive_type(&self, root: &str) -> u32 {
            self.types.get(root).copied().unwrap_or(0)
        }
        fn volume(&self, root: &str) -> Option<Volume> {
            self.labels.get(root).map(|l| Volume { label: (*l).to_string(), is_ntfs: self.ntfs.contains(&root) })
        }
        fn dos_target(&self, root: &str) -> String {
            self.dos.get(root).copied().unwrap_or_default().to_string()
        }
        fn space(&self, root: &str) -> Option<(u64, u64)> {
            self.spaces.get(root).copied()
        }
    }

    const A: u32 = 1;
    const C: u32 = 1 << 2;
    const D: u32 = 1 << 3;
    const E: u32 = 1 << 4;
    const F: u32 = 1 << 5;
    const Z: u32 = 1 << 25;

    #[test]
    fn list_with_no_drives_is_empty() {
        assert!(list_with(&Fake::default()).is_empty());
    }

    #[test]
    fn list_with_offers_ready_fixed_and_removable_drives_in_letter_order() {
        let fake = Fake {
            mask: Z | E | C,
            types: HashMap::from([("C:\\", 3), ("E:\\", 2), ("Z:\\", 3)]),
            labels: HashMap::from([("C:\\", "Windows"), ("E:\\", ""), ("Z:\\", "Last")]),
            spaces: HashMap::from([("C:\\", (100, 40)), ("E:\\", (10, 10)), ("Z:\\", (5, 1))]),
            ..Default::default()
        };
        let got = list_with(&fake);
        let letters: Vec<char> = got.iter().map(|d| d.letter).collect();
        assert_eq!(letters, ['C', 'E', 'Z']);
        assert_eq!(
            got[0],
            DriveInfo { letter: 'C', label: "Windows".into(), kind: DriveKind::Fixed, space: Some((100, 40)), is_ntfs: false }
        );
        assert_eq!(got[1].kind, DriveKind::Removable);
        assert_eq!(got[1].label, "");
    }

    #[test]
    fn list_with_leaves_out_network_cd_not_ready_and_unknown_drives() {
        let fake = Fake {
            mask: A | C | D | E | F,
            // A: network (ready), C: fixed but not ready (no label answer), D: CD (ready),
            // E: type unknown, F: a ready USB stick.
            types: HashMap::from([("A:\\", 4), ("C:\\", 3), ("D:\\", 5), ("E:\\", 0), ("F:\\", 2)]),
            labels: HashMap::from([("A:\\", "Net"), ("D:\\", "Disc"), ("E:\\", "x"), ("F:\\", "")]),
            spaces: HashMap::new(),
            ..Default::default()
        };
        let letters: Vec<char> = list_with(&fake).iter().map(|d| d.letter).collect();
        assert_eq!(letters, ['F']);
    }

    #[test]
    fn list_with_failed_space_query_keeps_the_drive_without_a_size() {
        let fake = Fake {
            mask: C,
            types: HashMap::from([("C:\\", 3)]),
            labels: HashMap::from([("C:\\", "")]),
            spaces: HashMap::new(),
            ..Default::default()
        };
        let got = list_with(&fake);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].space, None);
    }

    #[cfg(windows)]
    #[test]
    fn list_includes_the_system_drive() {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let letter = system.chars().next().unwrap().to_ascii_uppercase();
        assert!(list().iter().any(|d| d.letter == letter), "no {letter}: in the list");
    }

    // ---- list_with asks only fixed and removable drives (a network drive can take long to answer) ----
    struct Asked {
        types: HashMap<&'static str, u32>,
        asked: std::cell::RefCell<Vec<String>>,
    }

    impl DriveApi for Asked {
        fn logical_drives(&self) -> u32 {
            A | C | D | E
        }
        fn drive_type(&self, root: &str) -> u32 {
            self.types.get(root).copied().unwrap_or(0)
        }
        fn volume(&self, root: &str) -> Option<Volume> {
            self.asked.borrow_mut().push(root.to_string());
            Some(Volume { label: String::new(), is_ntfs: false })
        }
        fn dos_target(&self, _root: &str) -> String {
            String::new()
        }
        fn space(&self, _root: &str) -> Option<(u64, u64)> {
            None
        }
    }

    #[test]
    fn list_with_asks_only_fixed_and_removable_drives_for_a_label() {
        // A: network, C: fixed, D: CD, E: removable.
        let api = Asked { types: HashMap::from([("A:\\", 4), ("C:\\", 3), ("D:\\", 5), ("E:\\", 2)]), asked: Default::default() };
        list_with(&api);
        assert_eq!(*api.asked.borrow(), ["C:\\", "E:\\"]);
    }

    // ---- is_subst ----
    #[test]
    fn is_subst_every_dos_target_form_is_classified() {
        let cases = [
            (r"\??\C:\Users", true),
            (r"\??\C:\", true),
            (r"\??\C:", true),
            (r"\Device\HarddiskVolume3", false),
            (r"\Device\Harddisk\Volume1", false),
            (r"\Device\LanmanRedirector\;Z:000000000000abcd\srv\share", false),
            (r"\??\Volume{01234567-89ab-cdef-0123-456789abcdef}", false),
            ("", false),
        ];
        for (target, want) in cases {
            assert_eq!(is_subst(target), want, "target {target:?}");
        }
    }

    #[test]
    fn list_with_leaves_out_a_subst_drive() {
        // C: physical, X: a subst of C:Users. Both report DRIVE_FIXED.
        let fake = Fake {
            mask: C | (1 << 23),
            types: HashMap::from([("C:\\", 3), ("X:\\", 3)]),
            labels: HashMap::from([("C:\\", ""), ("X:\\", "")]),
            dos: HashMap::from([("C:\\", r"\Device\HarddiskVolume3"), ("X:\\", r"\??\C:\Users")]),
            ..Default::default()
        };
        let letters: Vec<char> = list_with(&fake).iter().map(|d| d.letter).collect();
        assert_eq!(letters, ['C']);
    }

    #[test]
    fn list_with_carries_the_ntfs_flag_of_each_volume() {
        let fake = Fake {
            mask: C | D,
            types: HashMap::from([("C:\\", 3), ("D:\\", 3)]),
            labels: HashMap::from([("C:\\", ""), ("D:\\", "")]),
            ntfs: vec!["C:\\"],
            ..Default::default()
        };
        let flags: Vec<bool> = list_with(&fake).iter().map(|d| d.is_ntfs).collect();
        assert_eq!(flags, [true, false]);
    }

    // ---- admin_note ----
    #[test]
    fn admin_note_is_shown_only_when_not_admin_and_a_listed_drive_is_ntfs() {
        let ntfs = DriveInfo { is_ntfs: true, ..info('C', "", DriveKind::Fixed, None) };
        let fat = info('E', "", DriveKind::Removable, None);
        let note = "Scans of a whole NTFS drive are faster as administrator.";
        let cases: [(&[DriveInfo], bool, &str); 7] = [
            (&[], false, ""),
            (&[], true, ""),
            (&[fat.clone()], false, ""),
            (&[ntfs.clone()], true, ""),
            (&[ntfs.clone()], false, note),
            (&[fat.clone(), ntfs.clone()], false, note),
            (&[fat.clone(), ntfs.clone()], true, ""),
        ];
        for (drives, is_admin, want) in cases {
            assert_eq!(admin_note(drives, is_admin), want, "admin {is_admin}, {} drives", drives.len());
        }
    }

    // ---- drive_buttons ----
    fn info(letter: char, label: &str, kind: DriveKind, space: Option<(u64, u64)>) -> DriveInfo {
        DriveInfo { letter, label: label.into(), kind, space, is_ntfs: false }
    }

    fn one(d: DriveInfo) -> DriveButton {
        let mut v = drive_buttons(&[d]);
        assert_eq!(v.len(), 1);
        v.remove(0)
    }

    #[test]
    fn drive_buttons_title_uses_the_label_or_a_kind_name() {
        let cases = [
            (info('C', "", DriveKind::Fixed, None), "C: Local Disk"),
            (info('E', "", DriveKind::Removable, None), "E: USB drive"),
            (info('D', "Data", DriveKind::Fixed, None), "D: Data"),
            (info('F', "Photos", DriveKind::Removable, None), "F: Photos"),
        ];
        for (d, want) in cases {
            assert_eq!(one(d).title, want);
        }
    }

    #[test]
    fn drive_buttons_root_is_the_drive_root() {
        assert_eq!(one(info('D', "", DriveKind::Fixed, None)).root, "D:\\");
    }

    #[test]
    fn drive_buttons_detail_and_used_show_free_of_total() {
        let gb = 1024u64.pow(3);
        let tb = 1024u64.pow(4);
        let total = tb * 18 / 10;
        let free = gb * 412;
        let b = one(info('C', "", DriveKind::Fixed, Some((total, free))));
        assert_eq!(b.detail, format!("{} free of {}", format_size(free), format_size(total)));
        assert!(b.detail.starts_with("412.0 GB free of 1.8 TB"), "{}", b.detail);
        assert!(b.size_known);
        let want = ((total - free) as f64 / total as f64) as f32;
        assert!((b.used - want).abs() < 1e-6, "used {}", b.used);
    }

    #[test]
    fn drive_buttons_unknown_size_says_so_and_has_no_bar() {
        let b = one(info('C', "", DriveKind::Fixed, None));
        assert_eq!((b.detail.as_str(), b.used, b.size_known), ("Size unknown.", 0.0, false));
    }

    #[test]
    fn drive_buttons_total_zero_has_used_zero() {
        let b = one(info('C', "", DriveKind::Fixed, Some((0, 0))));
        assert_eq!(b.used, 0.0);
    }

    #[test]
    fn drive_buttons_free_above_total_has_used_zero() {
        let b = one(info('C', "", DriveKind::Fixed, Some((10, 20))));
        assert_eq!(b.used, 0.0);
    }

    #[test]
    fn drive_buttons_full_drive_has_used_one() {
        let b = one(info('C', "", DriveKind::Fixed, Some((10, 0))));
        assert_eq!(b.used, 1.0);
    }

    #[test]
    fn drive_buttons_sorts_by_letter() {
        let v = drive_buttons(
            &[info('E', "", DriveKind::Removable, None), info('C', "", DriveKind::Fixed, None), info('D', "", DriveKind::Fixed, None)],
        );
        let roots: Vec<&str> = v.iter().map(|b| b.root.as_str()).collect();
        assert_eq!(roots, ["C:\\", "D:\\", "E:\\"]);
    }

    #[test]
    fn drive_buttons_no_drives_gives_no_buttons() {
        assert!(drive_buttons(&[]).is_empty());
    }
}

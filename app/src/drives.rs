//! The drives the start screen offers: ready fixed and removable drives, with their size.
//! The Win32 calls sit behind `DriveApi`, so the rules are unit tested with a fake.

use crate::model::format_size;

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
    /// `Faster as administrator.` when the app is not elevated, else "".
    pub note: String,
}

/// What the app asks Windows about drives. One method per call, so a fake can answer.
pub trait DriveApi {
    /// Bit `n` set when drive `A` + `n` exists (`GetLogicalDrives`). 0 when the call fails.
    fn logical_drives(&self) -> u32;
    /// `GetDriveTypeW` for a root such as `C:\`.
    fn drive_type(&self, root: &str) -> u32;
    /// Volume label, or `None` when the drive is not ready (no disk, locked, asleep and failing).
    fn volume_label(&self, root: &str) -> Option<String>;
    /// (total, free) bytes, or `None` when the query fails.
    fn space(&self, root: &str) -> Option<(u64, u64)>;
}

/// The kind of drive to offer, or `None` to leave it out. Only a ready fixed or removable drive is offered.
pub fn keep(_drive_type: u32, _ready: bool) -> Option<DriveKind> {
    None
}

/// The ready fixed and removable drives `api` reports, in letter order.
pub fn list_with(_api: &impl DriveApi) -> Vec<DriveInfo> {
    Vec::new()
}

/// The ready fixed and removable drives of this PC. Can be slow (a sleeping disk): call it on a worker.
pub fn list() -> Vec<DriveInfo> {
    list_with(&Win32Drives)
}

/// `DriveApi` over the Win32 calls.
struct Win32Drives;

impl DriveApi for Win32Drives {
    fn logical_drives(&self) -> u32 {
        todo!()
    }
    fn drive_type(&self, _root: &str) -> u32 {
        todo!()
    }
    fn volume_label(&self, _root: &str) -> Option<String> {
        todo!()
    }
    fn space(&self, _root: &str) -> Option<(u64, u64)> {
        todo!()
    }
}

/// The buttons for `drives`, sorted by letter. `is_admin` false adds the administrator note.
pub fn drive_buttons(_drives: &[DriveInfo], _is_admin: bool) -> Vec<DriveButton> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};

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
        spaces: HashMap<&'static str, (u64, u64)>,
    }

    impl DriveApi for Fake {
        fn logical_drives(&self) -> u32 {
            self.mask
        }
        fn drive_type(&self, root: &str) -> u32 {
            self.types.get(root).copied().unwrap_or(0)
        }
        fn volume_label(&self, root: &str) -> Option<String> {
            self.labels.get(root).map(|l| (*l).to_string())
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
        };
        let got = list_with(&fake);
        let letters: Vec<char> = got.iter().map(|d| d.letter).collect();
        assert_eq!(letters, ['C', 'E', 'Z']);
        assert_eq!(
            got[0],
            DriveInfo { letter: 'C', label: "Windows".into(), kind: DriveKind::Fixed, space: Some((100, 40)) }
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

    // ---- drive_buttons ----
    fn info(letter: char, label: &str, kind: DriveKind, space: Option<(u64, u64)>) -> DriveInfo {
        DriveInfo { letter, label: label.into(), kind, space }
    }

    fn one(d: DriveInfo, is_admin: bool) -> DriveButton {
        let mut v = drive_buttons(&[d], is_admin);
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
            assert_eq!(one(d, true).title, want);
        }
    }

    #[test]
    fn drive_buttons_root_is_the_drive_root() {
        assert_eq!(one(info('D', "", DriveKind::Fixed, None), true).root, "D:\\");
    }

    #[test]
    fn drive_buttons_detail_and_used_show_free_of_total() {
        let gb = 1024u64.pow(3);
        let tb = 1024u64.pow(4);
        let total = tb * 18 / 10;
        let free = gb * 412;
        let b = one(info('C', "", DriveKind::Fixed, Some((total, free))), true);
        assert_eq!(b.detail, format!("{} free of {}", format_size(free), format_size(total)));
        assert!(b.detail.starts_with("412.0 GB free of 1.8 TB"), "{}", b.detail);
        assert!(b.size_known);
        let want = ((total - free) as f64 / total as f64) as f32;
        assert!((b.used - want).abs() < 1e-6, "used {}", b.used);
    }

    #[test]
    fn drive_buttons_unknown_size_says_so_and_has_no_bar() {
        let b = one(info('C', "", DriveKind::Fixed, None), true);
        assert_eq!((b.detail.as_str(), b.used, b.size_known), ("Size unknown.", 0.0, false));
    }

    #[test]
    fn drive_buttons_total_zero_has_used_zero() {
        let b = one(info('C', "", DriveKind::Fixed, Some((0, 0))), true);
        assert_eq!(b.used, 0.0);
    }

    #[test]
    fn drive_buttons_free_above_total_has_used_zero() {
        let b = one(info('C', "", DriveKind::Fixed, Some((10, 20))), true);
        assert_eq!(b.used, 0.0);
    }

    #[test]
    fn drive_buttons_full_drive_has_used_one() {
        let b = one(info('C', "", DriveKind::Fixed, Some((10, 0))), true);
        assert_eq!(b.used, 1.0);
    }

    #[test]
    fn drive_buttons_note_shows_only_when_not_admin() {
        let d = info('C', "", DriveKind::Fixed, None);
        assert_eq!(one(d.clone(), false).note, "Faster as administrator.");
        assert_eq!(one(d, true).note, "");
    }

    #[test]
    fn drive_buttons_sorts_by_letter() {
        let v = drive_buttons(
            &[info('E', "", DriveKind::Removable, None), info('C', "", DriveKind::Fixed, None), info('D', "", DriveKind::Fixed, None)],
            true,
        );
        let roots: Vec<&str> = v.iter().map(|b| b.root.as_str()).collect();
        assert_eq!(roots, ["C:\\", "D:\\", "E:\\"]);
    }

    #[test]
    fn drive_buttons_no_drives_gives_no_buttons() {
        assert!(drive_buttons(&[], false).is_empty());
    }
}

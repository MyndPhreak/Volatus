//! Scanner choice: only a whole drive reads the MFT; a sub-folder is walked.

use scan::{Method, Route, is_volume_root, route};
use std::path::Path;

#[test]
fn is_volume_root_accepts_drive_roots() {
    for p in [r"C:\", r"c:\", "\\\\?\\C:\\", r"D:\"] {
        assert!(is_volume_root(Path::new(p)), "{p}");
    }
}

#[test]
fn is_volume_root_rejects_folders_and_other_paths() {
    for p in [r"C:\Users", r"C:\Users\", "\\\\?\\C:\\Users", r"C:\a\b", r"C:", "\\\\server\\share\\", "\\\\?\\UNC\\server\\share\\", r"Users", ""] {
        assert!(!is_volume_root(Path::new(p)), "{p}");
    }
}

#[test]
fn route_enumerates_every_root_and_method_case() {
    let mft = |fallback_to_walk| Route { method: Method::Mft, fallback_to_walk };
    let walk = Route { method: Method::Walk, fallback_to_walk: false };
    // (whole volume, requested method, expected)
    let cases = [
        (true, None, mft(true)),
        (true, Some(Method::Mft), mft(false)),
        (true, Some(Method::Walk), walk),
        (false, None, walk),
        (false, Some(Method::Mft), mft(false)),
        (false, Some(Method::Walk), walk),
    ];
    for (whole, requested, want) in cases {
        assert_eq!(route(whole, requested), want, "whole_volume={whole} method={requested:?}");
    }
}

//! File-type footer: the chips for the biggest file types. Plain functions, no Slint types.

use treemap::filetype::{Category, TypeTotal};

/// One chip of the footer: a file type, its share of the scan and its tile category.
#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    /// ".dll", "No extension" or "Other".
    pub label: String,
    /// Share of `total` as `format_percent` gives it: "10.3%".
    pub percent: String,
    /// Share of `total`, 0..=1: the width of the chip's part of the strip.
    pub share: f32,
    /// Gives the colour, the same as the tiles of this type.
    pub category: Category,
}

/// Label of the chip of the files with no extension.
pub const NO_EXTENSION: &str = "No extension";
/// Label of the chip that holds every type after the first `max`.
pub const OTHER: &str = "Other";

/// The chips of `totals` (as `type_totals` sorts them) out of `total` bytes, the size of the
/// scanned folder: the first `max` types with bytes, then one "Other" chip for what is left of
/// `total`, if anything. So the shares add up to 1. Empty when `total` is 0.
pub fn chips(totals: &[TypeTotal], total: u64, max: usize) -> Vec<Chip> {
    let _ = (totals, total, max);
    Vec::new()
}

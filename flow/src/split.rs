//! One group's split: which children of a folder get their own band and which merge into
//! "other (N)" (plan decisions 7 to 9).
//!
//! Only children with a size count. Folders are ordered by size descending, ties by `NodeId`
//! ascending. A folder, and the "files" band (all direct files summed), merges when it is under
//! `MERGE_PERCENT` of the parent (skipped with `keep_small`) or under `min_px` tall. Both rules
//! are monotonic in size, so the merged folders are always the smallest-N suffix of the order:
//! `Part::Other { parent, folders: N, files }` names them exactly, and `members` re-derives them.

use std::cmp::Ordering;

use scan::{NodeId, Tree};

use crate::{MERGE_PERCENT, Part, Split};

/// Band order: size descending, then `NodeId` ascending.
fn band_order(tree: &Tree, a: NodeId, b: NodeId) -> Ordering {
    tree.node(b).size.cmp(&tree.node(a).size).then(a.cmp(&b))
}

/// The merge rule. Integer share test (exact at the 3 % line); `u128` cannot overflow.
pub(crate) fn is_merged(size: u64, parent: u64, px_per_byte: f64, min_px: f64, keep_small: bool) -> bool {
    let share = !keep_small && u128::from(size) * 100 < u128::from(parent) * u128::from(MERGE_PERCENT);
    share || (size as f64) * px_per_byte < min_px
}

pub fn split(tree: &Tree, parent: NodeId, px_per_byte: f64, min_px: f64, keep_small: bool) -> Split {
    let parent_size = tree.node(parent).size;
    // Only the shown folders are collected; merged ones are only counted, so a folder with
    // 200,000 tiny siblings allocates nothing for them.
    let mut folders = Vec::new();
    let (mut merged_n, mut merged_bytes, mut files) = (0u32, 0u64, 0u64);
    for c in tree.children(parent) {
        let node = tree.node(c);
        if node.size == 0 {
            continue;
        }
        if !node.is_dir {
            files += node.size;
        } else if is_merged(node.size, parent_size, px_per_byte, min_px, keep_small) {
            merged_n += 1;
            merged_bytes += node.size;
        } else {
            folders.push(c);
        }
    }
    folders.sort_unstable_by(|&a, &b| band_order(tree, a, b));

    let files_merged = files > 0 && is_merged(files, parent_size, px_per_byte, min_px, keep_small);
    if files_merged {
        merged_bytes += files;
    }
    Split {
        folders,
        files: (files > 0 && !files_merged).then_some(files),
        other: (merged_n > 0 || files_merged).then_some((merged_n, files_merged, merged_bytes)),
    }
}

/// The nodes a band stands for: the folder itself; a folder's direct files; or the merged
/// folders of "other" (in band order) followed by the parent's direct files when they merged.
pub fn members(tree: &Tree, part: &Part) -> Vec<NodeId> {
    match *part {
        Part::Folder(n) => vec![n],
        Part::Files(f) => direct_files(tree, f).collect(),
        Part::Other { parent, folders, files } => {
            let mut v: Vec<NodeId> =
                tree.children(parent).filter(|&c| tree.node(c).is_dir && tree.node(c).size > 0).collect();
            let keep = v.len().saturating_sub(folders as usize);
            if keep > 0 {
                // Move the smallest `folders` to the tail without sorting the shown ones.
                v.select_nth_unstable_by(keep - 1, |&a, &b| band_order(tree, a, b));
                v.drain(..keep);
            }
            v.sort_unstable_by(|&a, &b| band_order(tree, a, b));
            if files {
                v.extend(direct_files(tree, parent));
            }
            v
        }
    }
}

fn direct_files(tree: &Tree, folder: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    tree.children(folder).filter(|&c| !tree.node(c).is_dir)
}

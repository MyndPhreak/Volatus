//! The dominant file type of a folder.

use scan::{NodeId, Tree};
use treemap::filetype::{Category, category_of};

/// The category holding the most bytes in the subtrees of `roots` (a file root counts only
/// itself); ties go to the earlier category in `Category::ALL`; `None` when no bytes.
///
/// One iterative walk, so a very deep tree cannot overflow the stack. To read a "files" band
/// pass its direct file children as roots: a folder root counts everything below it.
pub fn dominant(tree: &Tree, roots: &[NodeId]) -> Option<Category> {
    let mut sums = [0u64; Category::ALL.len()];
    let mut stack: Vec<NodeId> = roots.to_vec();
    while let Some(n) = stack.pop() {
        let node = tree.node(n);
        if node.is_dir {
            stack.extend(tree.children(n));
        } else if node.size > 0 {
            let c = category_of(tree.name(n));
            if let Some(i) = Category::ALL.iter().position(|&a| a == c) {
                sums[i] += node.size;
            }
        }
    }
    let mut best: Option<(usize, u64)> = None;
    for (i, &s) in sums.iter().enumerate() {
        if s > 0 && best.is_none_or(|(_, b)| s > b) {
            best = Some((i, s));
        }
    }
    best.map(|(i, _)| Category::ALL[i])
}

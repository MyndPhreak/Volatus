//! TP2.4: the time budget of the graph at 5 million nodes.
//!
//! Run in release: `cargo test -p flow --release --test budget -- --ignored`.
//! It times `layout` plus `render`. Both take the picture size in physical pixels; `scale` only
//! converts the logical constants.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use flow::{Input, layout, render};
use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};
use treemap::Theme;
use treemap::filetype::Category;

/// Nodes in the tree. Lower it to try the test on a small machine.
const NODES: usize = 5_000_000;
/// Subfolders of the biggest expanded folder.
const BIG_SUBFOLDERS: usize = 200_000;
const RUNS: usize = 5;
const BUDGET: Duration = Duration::from_millis(100);

fn entry(entries: &mut Vec<RawEntry>, parent: u64, name: String, size: u64, is_dir: bool) -> u64 {
    let id = entries.len() as u64 + 1;
    entries.push(RawEntry { id, parent, name, size, allocated: size, is_dir, modified: 1_700_000_000 });
    id
}

/// A view root with a few folders; three of them expanded, the first with `BIG_SUBFOLDERS`
/// subfolders; files fill the rest up to `NODES`. Returns the tree and the expanded folders.
fn build() -> (Tree, Vec<NodeId>) {
    let mut e = Vec::with_capacity(NODES);
    let root = entry(&mut e, 0, "root".into(), 0, true);
    let big = entry(&mut e, root, "big".into(), 0, true);
    let mid = entry(&mut e, root, "mid".into(), 0, true);
    let small = entry(&mut e, root, "small".into(), 0, true);
    let _plain = entry(&mut e, root, "plain".into(), 0, true);
    // The big folder: subfolders of four files each, as many as fit in a fifth of the nodes.
    let subs = BIG_SUBFOLDERS.min(NODES / 25);
    for i in 0..subs {
        let d = entry(&mut e, big, format!("d{i}"), 0, true);
        for j in 0..4 {
            entry(&mut e, d, format!("f{j}.mp4"), 1 + ((i + j) % 997) as u64, false);
        }
    }
    // The rest: subfolders of fifty files under the other folders.
    let parents = [mid, small, root];
    let mut k = 0usize;
    while e.len() < NODES {
        let d = entry(&mut e, parents[k % 3], format!("g{k}"), 0, true);
        for j in 0..50.min(NODES - e.len()) {
            entry(&mut e, d, format!("f{j}.png"), 1 + ((k + j) % 4001) as u64, false);
        }
        k += 1;
    }
    let r = rollup(root, e).unwrap();
    assert_eq!((r.orphans, r.skipped), (0, 0));
    // Raw id `i` (1-based, in input order) is `NodeId(i - 1)`.
    let ids = [big, mid, small].map(|raw| NodeId((raw - 1) as u32)).to_vec();
    (r.tree, ids)
}

fn median(mut runs: Vec<Duration>) -> Duration {
    runs.sort();
    runs[runs.len() / 2]
}

#[test]
#[ignore = "release timing: cargo test -p flow --release --test budget -- --ignored"]
fn budget_5m_nodes_layout_and_render_under_100_ms() {
    let (tree, expanded) = build();
    let view = NodeId(0);
    let expanded: HashSet<NodeId> = expanded.into_iter().collect();
    let open_other = HashSet::new();
    let colours = vec![[0u8; 3]; Category::ALL.len()];
    for (px_w, px_h, scale) in [(1920u32, 1080u32, 1.0f64), (3840, 2160, 2.0)] {
        let mut runs = Vec::new();
        for _ in 0..RUNS {
            let t = Instant::now();
            let graph = layout(&Input {
                tree: &tree,
                view,
                expanded: &expanded,
                open_other: &open_other,
                w: f64::from(px_w),
                h: f64::from(px_h),
                scale,
                first_column: 0,
            });
            let pixels = render(&graph, &colours, px_w, px_h, Theme::Dark);
            runs.push(t.elapsed());
            assert_eq!(pixels.len(), px_w as usize * px_h as usize * 4);
        }
        let med = median(runs);
        eprintln!("{NODES} nodes, {px_w}x{px_h} scale {scale}: median {med:?}");
        assert!(med < BUDGET, "{px_w}x{px_h} scale {scale}: median {med:?} is over {BUDGET:?}");
    }
}

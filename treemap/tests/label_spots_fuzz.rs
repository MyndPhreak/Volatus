//! label_spots invariants over random framed layouts (from the P8.P3 verifier's fuzz proof).
use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};
use treemap::{Chrome, Rect, file_label_spots, label_spots, layout_framed};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Random tree: up to 400 nodes, folders and files, sizes 0..huge, some zero-size folders.
fn random_tree(rng: &mut Lcg) -> Tree {
    let n = 2 + rng.below(400);
    let mut dirs = vec![0u64];
    let mut e = vec![RawEntry { id: 0, parent: 0, name: "root".into(), size: 0, allocated: 0, is_dir: true, modified: scan::time::UNKNOWN }];
    for id in 1..n {
        let parent = dirs[rng.below(dirs.len() as u64) as usize];
        let is_dir = rng.below(3) == 0;
        let size = if is_dir { 0 } else { match rng.below(4) { 0 => 0, 1 => rng.below(10), 2 => rng.below(1 << 20), _ => rng.below(1 << 40) } };
        if is_dir {
            dirs.push(id);
        }
        e.push(RawEntry { id, parent, name: format!("n{id}"), size, allocated: size, is_dir, modified: scan::time::UNKNOWN });
    }
    rollup(0, e).unwrap().tree
}

fn is_ancestor(tree: &Tree, anc: NodeId, mut n: NodeId) -> bool {
    while let Some(p) = tree.node(n).parent {
        if p == anc {
            return true;
        }
        n = p;
    }
    false
}

fn overlap(a: &Rect, b: &Rect) -> bool {
    const E: f64 = 1e-6;
    a.x + E < b.x + b.w && b.x + E < a.x + a.w && a.y + E < b.y + b.h && b.y + E < a.y + a.h
}

/// Display scales; every seed uses `SCALES[seed % 5]`, so each scale (1.5 too) gets 60 seeds.
const SCALES: [f64; 5] = [1.0, 1.25, 1.5, 2.0, 3.0];

#[test]
fn label_spots_invariants_hold_over_300_random_layouts() {
    let mut checked = 0usize;
    let mut capped_cases = 0usize;
    let mut per_scale = [0usize; 5];
    for seed in 0..300u64 {
        let mut rng = Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xDEAD_BEEF);
        let tree = random_tree(&mut rng);
        let scale = SCALES[(seed % 5) as usize];
        let (w, h) = (rng.below(2000) as u32, rng.below(1400) as u32);
        let chrome = Chrome::scaled(scale);
        let rects = layout_framed(&tree, tree.root(), w, h, chrome);
        let min_w = 24.0 * scale;
        let cap = [0usize, 1, 3, 10, 750, usize::MAX][rng.below(6) as usize];
        let spots = label_spots(&tree, &rects, chrome, min_w, cap);
        let p = chrome.pad;
        let eligible: Vec<(usize, &Rect)> =
            rects.iter().enumerate().filter(|(_, r)| tree.node(r.node).is_dir && chrome.has_header(r) && r.w - 2.0 * p >= min_w).collect();
        // Cap enforced, and nothing dropped below the cap.
        assert_eq!(spots.len(), eligible.len().min(cap), "seed {seed}");
        if eligible.len() > cap {
            capped_cases += 1;
        }
        // Each spot is the band of an eligible frame, in rects order (depth first).
        let mut last = None;
        let mut kept_area = f64::MAX;
        for s in &spots {
            let (i, r) = *eligible.iter().find(|(_, r)| r.node == s.node).unwrap_or_else(|| panic!("seed {seed}: spot for non-eligible node"));
            assert!(last.is_none_or(|l| l < i), "seed {seed}: order");
            last = Some(i);
            assert_eq!(*s, Rect { node: r.node, x: r.x + p, y: r.y, w: r.w - 2.0 * p, h: chrome.header }, "seed {seed}");
            assert!(s.x >= r.x && s.x + s.w <= r.x + r.w + 1e-9 && s.y >= r.y && s.y + s.h <= r.y + r.h + 1e-9, "seed {seed}: spot outside frame");
            assert!(s.w >= min_w && s.h > 0.0 && s.x.is_finite() && s.y.is_finite(), "seed {seed}");
            assert!(s.x + s.w <= f64::from(w) + 1e-9 && s.y + s.h <= f64::from(h) + 1e-9, "seed {seed}: off map");
            kept_area = kept_area.min(r.w * r.h);
        }
        // Largest first: every dropped frame is no bigger than every kept one.
        for (_, r) in &eligible {
            if !spots.iter().any(|s| s.node == r.node) {
                assert!(r.w * r.h <= kept_area + 1e-9, "seed {seed}: dropped a larger frame");
            }
        }
        // A label band overlaps no other rect except its own frame and its ancestors.
        for s in &spots {
            for r in &rects {
                if r.node != s.node && !is_ancestor(&tree, r.node, s.node) {
                    assert!(!overlap(s, r), "seed {seed}: band of {} overlaps {}", tree.name(s.node), tree.name(r.node));
                }
            }
        }
        checked += spots.len();
        per_scale[(seed % 5) as usize] += spots.len();
    }
    eprintln!("FUZZ 300 seeds: {checked} spots checked, {capped_cases} capped cases, per scale {per_scale:?}");
    assert!(checked > 1000 && capped_cases > 20 && per_scale.iter().all(|&n| n > 50), "{checked} {capped_cases} {per_scale:?}");
}

#[test]
fn label_spots_degenerate_inputs_are_empty_and_do_not_panic() {
    let mut rng = Lcg(7);
    let tree = random_tree(&mut rng);
    for (w, h) in [(0, 0), (1, 1), (0, 500), (500, 0), (27, 23), (28, 24)] {
        for s in [1.0, 1.5, 2.0] {
            let c = Chrome::scaled(s);
            let rects = layout_framed(&tree, tree.root(), w, h, c);
            let _ = label_spots(&tree, &rects, c, 24.0 * s, 750);
        }
    }
    // Negative and zero sizes in hand-made rects: no spot, no panic.
    let root = tree.root();
    let bad = [
        Rect { node: root, x: 0.0, y: 0.0, w: -100.0, h: 50.0 },
        Rect { node: root, x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
        Rect { node: root, x: 0.0, y: 0.0, w: f64::NAN, h: 50.0 },
    ];
    assert!(label_spots(&tree, &bad, Chrome::scaled(1.0), 24.0, 750).is_empty());
    assert!(label_spots(&tree, &bad, Chrome::NONE, 0.0, 750).is_empty());
}

/// The whole label layer under one cap, as the app builds it: frame bands first (`label_spots`),
/// then file tiles (`file_label_spots`) in the room left.
#[test]
fn file_label_spots_invariants_hold_over_300_random_layouts() {
    let (mut checked, mut capped_cases, mut frames_full) = (0usize, 0usize, 0usize);
    let mut per_scale = [0usize; 5];
    for seed in 0..300u64 {
        let mut rng = Lcg(seed.wrapping_mul(0xA24B_AED4_963E_E407) ^ 0xF11E);
        let tree = random_tree(&mut rng);
        let scale = SCALES[(seed % 5) as usize];
        let (w, h) = (rng.below(2000) as u32, rng.below(1400) as u32);
        let chrome = Chrome::scaled(scale);
        let rects = layout_framed(&tree, tree.root(), w, h, chrome);
        let (min_w, min_h) = (40.0 * scale, 14.0 * scale);
        let cap = [0usize, 1, 3, 10, 40, 750, usize::MAX][rng.below(7) as usize];
        let bands = label_spots(&tree, &rects, chrome, 24.0 * scale, cap);
        let room = cap.saturating_sub(bands.len());

        let spots = file_label_spots(&tree, &rects, min_w, min_h, room);

        let eligible: Vec<(usize, &Rect)> =
            rects.iter().enumerate().filter(|(_, r)| !tree.node(r.node).is_dir && r.w >= min_w && r.h >= min_h).collect();
        // One cap for the layer; frames first, files fill what is left and nothing below it is dropped.
        assert!(bands.len() + spots.len() <= cap, "seed {seed}: over the cap");
        assert_eq!(spots.len(), eligible.len().min(room), "seed {seed}");
        if eligible.len() > room {
            capped_cases += 1;
        }
        if room == 0 && cap > 0 && !eligible.is_empty() {
            frames_full += 1;
        }
        let mut last = None;
        let mut kept_area = f64::MAX;
        for s in &spots {
            // Each spot is the tile of an eligible file, in rects order, on the map.
            let (i, r) = *eligible.iter().find(|(_, r)| r.node == s.node).unwrap_or_else(|| panic!("seed {seed}: spot for non-eligible node"));
            assert!(last.is_none_or(|l| l < i), "seed {seed}: order");
            last = Some(i);
            assert_eq!(s, r, "seed {seed}: spot is not its tile");
            assert!(s.x + s.w <= f64::from(w) + 1e-9 && s.y + s.h <= f64::from(h) + 1e-9, "seed {seed}: off map");
            kept_area = kept_area.min(r.w * r.h);
        }
        // Largest first: every dropped tile is no bigger than every kept one.
        for (_, r) in &eligible {
            if !spots.iter().any(|s| s.node == r.node) {
                assert!(r.w * r.h <= kept_area + 1e-9, "seed {seed}: dropped a larger tile");
            }
        }
        // A file label overlaps no frame band and no rect other than its tile and its folders.
        let all_bands = label_spots(&tree, &rects, chrome, 0.0, usize::MAX);
        for s in &spots {
            for b in &all_bands {
                assert!(!overlap(s, b), "seed {seed}: file {} overlaps the band of {}", tree.name(s.node), tree.name(b.node));
            }
            for r in &rects {
                if r.node != s.node && !is_ancestor(&tree, r.node, s.node) {
                    assert!(!overlap(s, r), "seed {seed}: file {} overlaps {}", tree.name(s.node), tree.name(r.node));
                }
            }
        }
        checked += spots.len();
        per_scale[(seed % 5) as usize] += spots.len();
    }
    eprintln!("FUZZ files 300 seeds: {checked} spots, {capped_cases} capped cases, {frames_full} with no room left, per scale {per_scale:?}");
    assert!(checked > 1000 && capped_cases > 20 && frames_full > 5 && per_scale.iter().all(|&n| n > 50), "{checked} {capped_cases} {frames_full} {per_scale:?}");
}

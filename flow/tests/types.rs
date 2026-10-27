//! TP2.4: the dominant file type of a folder.

use flow::{Part, dominant, members};
use scan::rollup::{RawEntry, rollup};
use scan::{NodeId, Tree};
use treemap::filetype::{Category, category_of, type_totals};

/// Builds a tree with `rollup`. A raw id `i` (1-based, in input order) is `NodeId(i - 1)`.
struct Build {
    entries: Vec<RawEntry>,
}

impl Build {
    fn new() -> Self {
        let mut b = Build { entries: Vec::new() };
        b.push(1, "root", 0, true);
        b
    }

    fn push(&mut self, parent: u64, name: &str, size: u64, is_dir: bool) -> u64 {
        let id = self.entries.len() as u64 + 1;
        self.entries.push(RawEntry { id, parent, name: name.into(), size, allocated: size, is_dir, modified: 0 });
        id
    }

    fn dir(&mut self, parent: u64, name: &str) -> u64 {
        self.push(parent, name, 0, true)
    }

    fn file(&mut self, parent: u64, name: &str, size: u64) -> u64 {
        self.push(parent, name, size, false)
    }

    fn build(self) -> Tree {
        let r = rollup(1, self.entries).unwrap();
        assert_eq!((r.orphans, r.skipped), (0, 0));
        r.tree
    }
}

fn nid(raw: u64) -> NodeId {
    NodeId((raw - 1) as u32)
}

const ROOT: u64 = 1;

/// The category with the most bytes in `type_totals`; ties go to the lower index in `ALL`.
fn footer_argmax(tree: &Tree, root: NodeId) -> Option<Category> {
    let mut sums = [0u64; Category::ALL.len()];
    for t in type_totals(tree, root) {
        let c = category_of(&format!("x.{}", t.ext));
        sums[Category::ALL.iter().position(|&a| a == c).unwrap()] += t.bytes;
    }
    let mut best: Option<(usize, u64)> = None;
    for (i, &s) in sums.iter().enumerate() {
        if s > 0 && best.is_none_or(|(_, b)| s > b) {
            best = Some((i, s));
        }
    }
    best.map(|(i, _)| Category::ALL[i])
}

#[test]
fn video_100_beats_image_50() {
    let mut b = Build::new();
    b.file(ROOT, "a.mp4", 100);
    b.file(ROOT, "b.png", 50);
    let tree = b.build();
    assert_eq!(dominant(&tree, &[nid(ROOT)]), Some(Category::Video));
}

#[test]
fn bytes_decide_not_file_count() {
    let mut b = Build::new();
    b.file(ROOT, "big.mp4", 100);
    for i in 0..5 {
        b.file(ROOT, &format!("s{i}.png"), 10);
    }
    let tree = b.build();
    assert_eq!(dominant(&tree, &[nid(ROOT)]), Some(Category::Video));
}

#[test]
fn a_tie_goes_to_the_earlier_category() {
    let mut b = Build::new();
    // Added in the opposite order to `ALL`: Code is later than Audio.
    b.file(ROOT, "a.rs", 70);
    b.file(ROOT, "b.mp3", 70);
    let tree = b.build();
    assert_eq!(dominant(&tree, &[nid(ROOT)]), Some(Category::Audio));
}

#[test]
fn zero_bytes_is_none() {
    let mut b = Build::new();
    b.file(ROOT, "a.mp4", 0);
    let d = b.dir(ROOT, "d");
    b.file(d, "b.png", 0);
    let tree = b.build();
    assert_eq!(dominant(&tree, &[nid(ROOT)]), None);
    assert_eq!(dominant(&tree, &[]), None);
}

#[test]
fn a_folder_root_counts_its_whole_subtree() {
    let mut b = Build::new();
    b.file(ROOT, "a.png", 10);
    let d = b.dir(ROOT, "d");
    let e = b.dir(d, "e");
    b.file(e, "deep.mp4", 500);
    let tree = b.build();
    assert_eq!(dominant(&tree, &[nid(ROOT)]), Some(Category::Video));
}

#[test]
fn files_roots_ignore_files_in_subfolders() {
    let mut b = Build::new();
    b.file(ROOT, "own.png", 10);
    b.file(ROOT, "own2.txt", 5);
    let d = b.dir(ROOT, "sub");
    b.file(d, "huge.mp4", 1000);
    let tree = b.build();
    let roots = members(&tree, &Part::Files(nid(ROOT)));
    assert_eq!(roots.len(), 2, "the direct files only");
    assert_eq!(dominant(&tree, &roots), Some(Category::Image));
}

#[test]
fn a_file_root_counts_only_itself() {
    let mut b = Build::new();
    let f = b.file(ROOT, "a.mp3", 7);
    b.file(ROOT, "b.mp4", 900);
    let tree = b.build();
    assert_eq!(dominant(&tree, &[nid(f)]), Some(Category::Audio));
}

#[test]
fn other_roots_union_their_members() {
    let mut b = Build::new();
    let x = b.dir(ROOT, "x");
    b.file(x, "a.png", 60);
    let y = b.dir(ROOT, "y");
    b.file(y, "b.png", 60);
    let z = b.dir(ROOT, "z");
    b.file(z, "c.mp4", 100);
    let tree = b.build();
    // Alone, z's video beats either image folder; together the images win 120 to 100.
    assert_eq!(dominant(&tree, &[nid(z)]), Some(Category::Video));
    assert_eq!(dominant(&tree, &[nid(x), nid(y), nid(z)]), Some(Category::Image));
}

#[test]
fn dominant_agrees_with_the_footer_totals() {
    let exts = ["mp4", "png", "mp3", "zip", "exe", "pdf", "rs", "iso", "pak", "json", "", "xyz", "RS", "Mp4"];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..20 {
        let mut b = Build::new();
        let mut dirs = vec![ROOT];
        for i in 0..300 {
            let parent = dirs[(next() % dirs.len() as u64) as usize];
            if next() % 5 == 0 {
                dirs.push(b.dir(parent, &format!("d{i}")));
            } else {
                let ext = exts[(next() % exts.len() as u64) as usize];
                let name = if ext.is_empty() { format!("f{i}") } else { format!("f{i}.{ext}") };
                // Small sizes make ties likely in some rounds.
                let size = if round % 2 == 0 { next() % 4 * 10 } else { next() % 100_000 };
                b.file(parent, &name, size);
            }
        }
        let tree = b.build();
        for &d in &dirs {
            assert_eq!(dominant(&tree, &[nid(d)]), footer_argmax(&tree, nid(d)), "round {round}, folder {d}");
        }
    }
}

#[test]
fn a_chain_of_100000_folders_does_not_overflow_the_stack() {
    let mut b = Build::new();
    let mut parent = ROOT;
    for i in 0..100_000 {
        parent = b.dir(parent, &format!("d{i}"));
    }
    b.file(parent, "bottom.mp4", 42);
    let tree = b.build();
    assert_eq!(dominant(&tree, &[nid(ROOT)]), Some(Category::Video));
}

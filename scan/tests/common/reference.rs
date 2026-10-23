//! Test-only oracle: the `entries` function exactly as it was at 81181b2, and the seeded
//! generator both fuzz suites share. The library's own `entries` is compared against it.

use scan::mft::Record;
use scan::rollup::RawEntry;
use std::collections::HashMap;

/// Seeded xorshift64 (the generator of mft_records.rs). No new dependency, fully deterministic.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform-enough value in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// True with probability `percent` / 100.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// `scan::mft::entries` at 81181b2, verbatim. Extensions merge into the LAST base with a number
/// (the index collect overwrites); an extension whose base is itself an extension is dropped.
pub fn entries_reference(records: Vec<Record>) -> (Vec<RawEntry>, u64) {
    let (mut bases, extensions): (Vec<Record>, Vec<Record>) = records.into_iter().partition(|r| r.base.is_none());
    let index: HashMap<u64, usize> = bases.iter().enumerate().map(|(i, r)| (r.number, i)).collect();
    for e in extensions {
        if let Some(&i) = e.base.and_then(|b| index.get(&b)) {
            let b = &mut bases[i];
            b.names.extend(e.names);
            b.data = b.data.or(e.data);
            b.surrogate |= e.surrogate;
        }
    }
    let mut skipped = 0;
    let mut out = Vec::with_capacity(bases.len());
    for r in bases {
        let data = r.data.unwrap_or_default();
        let Some(first) = r.names.into_iter().next() else {
            if data != scan::mft::DataSize::default() {
                skipped += 1;
            }
            continue;
        };
        let is_dir = r.is_dir && !r.surrogate;
        let (size, allocated) = if is_dir { (0, 0) } else { (data.size, data.allocated) };
        out.push(RawEntry { id: r.number, parent: first.parent, name: first.name, size, allocated, is_dir });
    }
    (out, skipped)
}

type EntryFields = (u64, u64, String, u64, u64, bool);

fn fields(list: &[RawEntry]) -> Vec<EntryFields> {
    list.iter().map(|e| (e.id, e.parent, e.name.clone(), e.size, e.allocated, e.is_dir)).collect()
}

/// Panics at the first differing entry (field tuples, in order) or count.
pub fn assert_same_entries(got: &(Vec<RawEntry>, u64), want: &(Vec<RawEntry>, u64)) {
    assert_eq!(got.1, want.1, "skipped count");
    assert_eq!(got.0.len(), want.0.len(), "entry count");
    let (g, w) = (fields(&got.0), fields(&want.0));
    for (i, (a, b)) in g.iter().zip(&w).enumerate() {
        assert_eq!(a, b, "entry {i} (id, parent, name, size, allocated, is_dir)");
    }
}

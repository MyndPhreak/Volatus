//! Read buffer sizing: the chunk is rounded down to whole units and capped by the `$MFT` size.

use scan::DEFAULT_CHUNK;
use scan::mft::buffer_size;

const MIB: u64 = 1 << 20;

#[test]
fn buffer_size_table_rounds_down_to_units_and_caps_at_total() {
    // (chunk, unit, total, want)
    let cases: [(usize, u64, u64, usize); 10] = [
        (DEFAULT_CHUNK, 4096, 1 << 40, DEFAULT_CHUNK),            // big volume: the 32 MiB default
        (DEFAULT_CHUNK, 4096, 100 * MIB, DEFAULT_CHUNK),          // still above the chunk
        (DEFAULT_CHUNK + 5000, 4096, 1 << 40, DEFAULT_CHUNK + 4096), // rounds down to a unit multiple
        (DEFAULT_CHUNK, 4096, 64 * 1024, 64 * 1024),              // small $MFT: capped to its size
        (DEFAULT_CHUNK, 4096, 5000, 8192),                        // cap rounds up to whole units
        (DEFAULT_CHUNK, 4096, 1, 4096),                           // one byte still needs one unit
        (1, 4096, 1 << 40, 4096),                                 // tiny chunk: at least one unit
        (10_000, 4096, 1 << 40, 8192),                            // 2.44 units: 2
        (4096, 4096, 1 << 40, 4096),                              // exactly one unit
        (DEFAULT_CHUNK, 65536, 1 << 40, DEFAULT_CHUNK),           // 64 KiB cluster divides 32 MiB
    ];
    for (chunk, unit, total, want) in cases {
        assert_eq!(buffer_size(chunk, unit, total), want, "chunk {chunk} unit {unit} total {total}");
    }
}

#[test]
fn buffer_size_is_a_multiple_of_the_unit_for_awkward_chunks() {
    for unit in [512u64, 1024, 4096, 65536] {
        for chunk in [1usize, 511, 513, 4097, 99_999, DEFAULT_CHUNK - 1, DEFAULT_CHUNK, DEFAULT_CHUNK + 1] {
            let got = buffer_size(chunk, unit, 1 << 40) as u64;
            assert_eq!(got % unit, 0, "chunk {chunk} unit {unit}");
            assert!(got >= unit && got <= (chunk as u64).max(unit), "chunk {chunk} unit {unit} got {got}");
        }
    }
}

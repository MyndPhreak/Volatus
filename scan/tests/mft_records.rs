//! MFT record parsing and volume reading against synthetic byte fixtures (common/ntfs.rs).

mod common;

use common::ntfs::*;
use common::reference::{Rng, assert_same_entries, entries_reference};
use scan::mft::{DataSize, FileName, Malformed, Record, entries, parse_record, scan_device};
use scan::rollup::rollup;
use scan::time::{FILETIME_2000, UNKNOWN};
use scan::{Method, ScanError, ScanStats, Tree};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;

fn parse(mut bytes: Vec<u8>) -> Result<Option<Record>, Malformed> {
    parse_record(&mut bytes, FIRST_USER)
}

fn parsed(bytes: Vec<u8>) -> Record {
    parse(bytes).expect("valid record").expect("record in use")
}

fn scan_image(image: Vec<u8>, sub: &str) -> Result<(Tree, ScanStats), ScanError> {
    let (tx, _rx) = channel();
    scan_device(&mut Cursor::new(image), Path::new(sub), "X:\\", &tx, &AtomicBool::new(false))
}

fn child_size(tree: &Tree, name: &str) -> u64 {
    let n = tree.children(tree.root()).find(|&c| tree.name(c) == name).expect(name);
    tree.node(n).size
}

/// FILETIME of `secs` whole seconds after 2000-01-01 (the `time` encoding of `secs`).
fn at(secs: i64) -> i64 {
    FILETIME_2000 + secs * 10_000_000
}

/// `modified` of the node at `path` (names joined by `/`) below the root.
fn modified_at(tree: &Tree, path: &str) -> u32 {
    let n = path.split('/').fold(tree.root(), |n, want| {
        tree.children(n).find(|&c| tree.name(c) == want).expect(want)
    });
    tree.node(n).modified
}

// ---- parse_record ----

#[test]
fn parse_record_valid_name_and_data_parses_name_parent_size() {
    let r = parsed(record(IN_USE, 0, &[file_name(ROOT, "report.pdf", WIN32), data(5000, 8192)]));

    assert_eq!(
        (r.names, r.data.map(|d| d.size), r.is_dir, r.base),
        (vec![FileName { parent: ROOT, name: "report.pdf".into() }], Some(5000), false, None)
    );
}

#[test]
fn parse_record_fixup_mismatch_is_rejected() {
    let mut bytes = file(ROOT, "a.txt", 10);
    bytes[510] ^= 0xFF; // end of first stride no longer holds the USN

    assert_eq!(parse(bytes), Err(Malformed));
}

#[test]
fn parse_record_fixup_restores_saved_stride_words() {
    // The value at the stride end lives in the update sequence array on disk.
    let mut bytes = record(IN_USE, 0, &[file_name(ROOT, "a.txt", WIN32), resident_data(10)]);
    let at_end = [bytes[510], bytes[511]];

    parse_record(&mut bytes, FIRST_USER).unwrap();

    assert_ne!([bytes[510], bytes[511]], at_end, "USN replaced by the saved word");
}

#[test]
fn parse_record_bad_signature_is_rejected() {
    let mut bytes = file(ROOT, "a.txt", 10);
    bytes[0..4].copy_from_slice(b"BAAD");

    assert_eq!(parse(bytes), Err(Malformed));
}

#[test]
fn parse_record_not_in_use_returns_none() {
    let bytes = record(0, 0, &[file_name(ROOT, "deleted.txt", WIN32), resident_data(4)]);

    assert_eq!(parse(bytes), Ok(None));
}

#[test]
fn parse_record_never_used_zeroed_returns_none() {
    assert_eq!(parse(vec![0u8; RECORD]), Ok(None));
}

#[test]
fn parse_record_resident_data_size_is_value_length() {
    let r = parsed(record(IN_USE, 0, &[file_name(ROOT, "small.txt", WIN32), resident_data(37)]));

    assert_eq!(r.data.map(|d| d.size), Some(37));
}

#[test]
fn parse_record_non_resident_data_uses_real_and_allocated_fields() {
    let r = parsed(record(IN_USE, 0, &[file_name(ROOT, "big.bin", WIN32), data(10_000, 12_288)]));

    assert_eq!(r.data, Some(DataSize { size: 10_000, allocated: 12_288 }));
}

#[test]
fn parse_record_sparse_data_allocated_is_compressed_size() {
    let run = runs(&[(1000, 1)]);
    let d = nonresident_attr(0x80, "", SPARSE, 0, 1 << 20, 1 << 20, Some(4096), &run);

    let r = parsed(record(IN_USE, 0, &[file_name(ROOT, "sparse.img", WIN32), d]));

    assert_eq!(r.data, Some(DataSize { size: 1 << 20, allocated: 4096 }));
}

#[test]
fn parse_record_compressed_data_allocated_is_compressed_size() {
    let run = runs(&[(1000, 2)]);
    let d = nonresident_attr(0x80, "", COMPRESSED, 0, 65_536, 60_000, Some(8192), &run);

    let r = parsed(record(IN_USE, 0, &[file_name(ROOT, "packed.log", WIN32), d]));

    assert_eq!(r.data, Some(DataSize { size: 60_000, allocated: 8192 }));
}

#[test]
fn parse_record_named_data_stream_is_ignored() {
    let ads = resident_attr(0x80, "Zone.Identifier", &[1u8; 26]);

    let r = parsed(record(IN_USE, 0, &[file_name(ROOT, "dl.exe", WIN32), resident_data(9), ads]));

    assert_eq!(r.data.map(|d| d.size), Some(9));
}

#[test]
fn parse_record_data_extent_above_vcn_zero_is_ignored() {
    // Only the extent starting at VCN 0 carries the stream sizes.
    let later = nonresident_attr(0x80, "", 0, 7, 99_999, 99_999, None, &runs(&[(1000, 1)]));

    let r = parsed(record(IN_USE, 0, &[later]));

    assert_eq!(r.data, None);
}

#[test]
fn parse_record_dos_and_win32_names_keeps_win32_only() {
    let attrs = [file_name(ROOT, "LONGFI~1.TXT", DOS), file_name(ROOT, "Long file name.txt", WIN32)];

    let r = parsed(record(IN_USE, 0, &attrs));

    assert_eq!(r.names, vec![FileName { parent: ROOT, name: "Long file name.txt".into() }]);
}

#[test]
fn parse_record_posix_and_win32_dos_names_are_kept() {
    let attrs = [file_name(ROOT, "Readme", POSIX), file_name(ROOT, "README", WIN32_DOS)];

    let r = parsed(record(IN_USE, 0, &attrs));

    assert_eq!(r.names.len(), 2);
}

#[test]
fn parse_record_directory_flag_sets_is_dir() {
    let r = parsed(dir(ROOT, "docs"));

    assert!(r.is_dir);
}

#[test]
fn parse_record_standard_information_gives_last_write_time() {
    let r = parsed(record(IN_USE, 0, &[standard_information(at(50)), file_name(ROOT, "t.txt", WIN32)]));

    assert_eq!(r.modified, 50);
}

#[test]
fn parse_record_without_standard_information_is_unknown_time() {
    let r = parsed(file(ROOT, "t.txt", 10));

    assert_eq!((r.modified, r.data.map(|d| d.size)), (UNKNOWN, Some(10)));
}

#[test]
fn parse_record_short_standard_information_is_unknown_time_not_malformed() {
    let short = resident_attr(0x10, "", &at(50).to_le_bytes()); // 8 bytes: no room for 0x08..0x10

    let r = parsed(record(IN_USE, 0, &[short, file_name(ROOT, "t.txt", WIN32), resident_data(10)]));

    assert_eq!((r.modified, r.data.map(|d| d.size)), (UNKNOWN, Some(10)));
}

#[test]
fn parse_record_non_resident_standard_information_is_unknown_time_not_malformed() {
    let si = nonresident_attr(0x10, "", 0, 0, 4096, 0x48, None, &runs(&[(1000, 1)]));

    let r = parsed(record(IN_USE, 0, &[si, file_name(ROOT, "t.txt", WIN32), resident_data(10)]));

    assert_eq!((r.modified, r.data.map(|d| d.size)), (UNKNOWN, Some(10)));
}

#[test]
fn parse_record_extension_record_reports_base() {
    let r = parsed(record(IN_USE, 300, &[data(1, 4096)]));

    assert_eq!(r.base, Some(300));
}

#[test]
fn parse_record_surrogate_reparse_tag_is_flagged() {
    let r = parsed(record(IN_USE | DIR, 0, &[file_name(ROOT, "link", WIN32), reparse(TAG_MOUNT_POINT)]));

    assert!(r.surrogate);
}

#[test]
fn parse_record_cloud_reparse_tag_is_not_a_surrogate() {
    let r = parsed(record(IN_USE, 0, &[file_name(ROOT, "cloud.docx", WIN32), reparse(TAG_CLOUD)]));

    assert!(!r.surrogate);
}

#[test]
fn parse_record_zero_length_attribute_is_rejected() {
    let mut a = resident_data(4);
    a[4..8].copy_from_slice(&0u32.to_le_bytes());

    assert_eq!(parse(record(IN_USE, 0, &[file_name(ROOT, "x", WIN32), a])), Err(Malformed));
}

#[test]
fn parse_record_attribute_past_used_size_is_rejected() {
    let mut bytes = file(ROOT, "x", 4);
    bytes[0x18..0x1C].copy_from_slice(&0x60u32.to_le_bytes()); // used size cuts the attributes

    assert_eq!(parse(bytes), Err(Malformed));
}

#[test]
fn parse_record_truncated_is_rejected_at_every_length() {
    let full = file(ROOT, "truncated.bin", 5000);

    let accepted: Vec<usize> = (0..RECORD)
        .filter(|&len| parse_record(&mut full[..len].to_vec(), FIRST_USER).is_ok())
        .collect();

    assert_eq!(accepted, Vec::<usize>::new());
}

#[test]
fn parse_record_random_mutations_never_panic() {
    let plain = record(
        IN_USE,
        0,
        &[file_name(ROOT, "fuzz.bin", WIN32), file_name(ROOT, "FUZZ~1.BIN", DOS), data(5000, 8192), reparse(TAG_CLOUD)],
    );
    let with_si = file_at(ROOT, "fuzz-si.bin", 5000, at(50));
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    let mut outcomes = [0u32; 3];
    let mut timed = 0u32;
    for k in 0..20_000 {
        let mut b = if k % 2 == 0 { plain.clone() } else { with_si.clone() };
        for _ in 0..(next() % 8 + 1) {
            let at = (next() % RECORD as u64) as usize;
            b[at] = next() as u8;
        }
        let i = match parse_record(&mut b, FIRST_USER) {
            Ok(Some(r)) => {
                timed += u32::from(r.modified != UNKNOWN);
                0
            }
            Ok(None) => 1,
            Err(Malformed) => 2,
        };
        outcomes[i] += 1;
    }
    for _ in 0..2_000 {
        let mut b: Vec<u8> = (0..RECORD).map(|_| next() as u8).collect();
        b[0..4].copy_from_slice(b"FILE");
        let _ = parse_record(&mut b, FIRST_USER);
    }

    assert!(outcomes[0] > 0 && outcomes[2] > 0, "fuzz reached both paths: {outcomes:?}");
    assert!(timed > 0, "fuzz parsed a $STANDARD_INFORMATION time");
}

// ---- entries ----

#[test]
fn entries_extension_record_data_merges_into_base() {
    let base = parsed(record(IN_USE, 0, &[attribute_list(40), file_name(ROOT, "frag.vhd", WIN32)]));
    let mut ext = record(IN_USE, 39, &[data(7_000_000, 7_004_160)]);
    let ext = parse_record(&mut ext, 40).unwrap().unwrap();
    let base = Record { number: 39, ..base };

    let (list, skipped) = entries(vec![base, ext]);

    let got: Vec<(u64, &str, u64, u64)> =
        list.iter().map(|e| (e.id, e.name.as_str(), e.size, e.allocated)).collect();
    assert_eq!((got, skipped), (vec![(39, "frag.vhd", 7_000_000, 7_004_160)], 0));
}

#[test]
fn entries_extension_before_base_still_merges() {
    let mut ext = record(IN_USE, 39, &[data(123_456, 126_976)]);
    let ext = parse_record(&mut ext, 12).unwrap().unwrap();
    let mut base = record(IN_USE, 0, &[attribute_list(12), file_name(ROOT, "late.bin", WIN32)]);
    let base = parse_record(&mut base, 39).unwrap().unwrap();

    let (list, _) = entries(vec![ext, base]);

    assert_eq!(list.iter().map(|e| e.size).collect::<Vec<_>>(), vec![123_456]);
}

#[test]
fn entries_hardlink_two_names_counts_size_once() {
    let link = parsed(record(
        IN_USE,
        0,
        &[file_name(20, "copy-a.bin", WIN32), file_name(21, "copy-b.bin", WIN32), data(4096, 4096)],
    ));
    let mut d20 = dir(ROOT, "a");
    let mut d21 = dir(ROOT, "b");
    let mut root = record(IN_USE | DIR, 0, &[file_name(ROOT, ".", WIN32_DOS)]);
    let records = vec![
        parse_record(&mut root, ROOT).unwrap().unwrap(),
        parse_record(&mut d20, 20).unwrap().unwrap(),
        parse_record(&mut d21, 21).unwrap().unwrap(),
        link,
    ];

    let (list, skipped) = entries(records);
    let rolled = rollup(ROOT, list).unwrap();

    assert_eq!((rolled.tree.node(rolled.tree.root()).size, rolled.files, skipped), (4096, 1, 0));
}

#[test]
fn entries_junction_directory_becomes_zero_size_file() {
    let r = parsed(record(IN_USE | DIR, 0, &[file_name(ROOT, "link", WIN32), reparse(TAG_MOUNT_POINT)]));

    let (list, _) = entries(vec![r]);

    assert_eq!((list[0].is_dir, list[0].size), (false, 0));
}

#[test]
fn entries_cloud_placeholder_file_keeps_logical_size() {
    let d = nonresident_attr(0x80, "", SPARSE, 0, 1 << 20, 900_000, Some(0), &runs(&[(1000, 1)]));
    let r = parsed(record(IN_USE, 0, &[file_name(ROOT, "cloud.docx", WIN32), reparse(TAG_CLOUD), d]));

    let (list, _) = entries(vec![r]);

    assert_eq!(list[0].size, 900_000);
}

#[test]
fn entries_cloud_placeholder_directory_stays_a_directory() {
    let r = parsed(record(IN_USE | DIR, 0, &[file_name(ROOT, "OneDrive", WIN32), reparse(TAG_CLOUD)]));

    let (list, _) = entries(vec![r]);

    assert!(list[0].is_dir);
}

#[test]
fn entries_directory_record_size_is_zero() {
    let mut d = record(IN_USE | DIR, 0, &[file_name(ROOT, "d", WIN32), data(4096, 4096)]);
    let r = parse_record(&mut d, 30).unwrap().unwrap();

    let (list, _) = entries(vec![r]);

    assert_eq!(list[0].size, 0);
}

#[test]
fn entries_file_without_data_has_zero_size() {
    let r = parsed(record(IN_USE, 0, &[file_name(ROOT, "empty", WIN32)]));

    let (list, skipped) = entries(vec![r]);

    assert_eq!((list[0].size, list[0].allocated, skipped), (0, 0, 0));
}

#[test]
fn entries_nameless_record_with_data_counts_skipped() {
    let r = parsed(record(IN_USE, 0, &[data(5000, 8192)]));

    let (list, skipped) = entries(vec![r]);

    assert_eq!((list.len(), skipped), (0, 1));
}

#[test]
fn entries_nameless_empty_record_is_ignored() {
    // Reserved system records (12-15) are in use but hold no name and no data.
    let r = parsed(record(IN_USE, 0, &[resident_data(0)]));

    let (list, skipped) = entries(vec![r]);

    assert_eq!((list.len(), skipped), (0, 0));
}

#[test]
fn entries_extension_without_base_is_dropped() {
    let mut ext = record(IN_USE, 777, &[data(5000, 8192)]);
    let ext = parse_record(&mut ext, 40).unwrap().unwrap();

    let (list, _) = entries(vec![ext]);

    assert!(list.is_empty());
}

// ---- entries: merge quirks pinned before the one-pass rewrite (hand-built records) ----

fn rec(number: u64, base: Option<u64>, names: &[(u64, &str)], data: Option<(u64, u64)>) -> Record {
    Record {
        number,
        base,
        is_dir: false,
        names: names.iter().map(|&(parent, name)| FileName { parent, name: name.into() }).collect(),
        data: data.map(|(size, allocated)| DataSize { size, allocated }),
        surrogate: false,
        modified: UNKNOWN,
    }
}

fn summary(list: &[scan::rollup::RawEntry]) -> Vec<(u64, u64, &str, u64, u64)> {
    list.iter().map(|e| (e.id, e.parent, e.name.as_str(), e.size, e.allocated)).collect()
}

#[test]
fn entries_two_extensions_of_nameless_base_first_extension_name_wins() {
    let records = vec![
        rec(50, None, &[], None),
        rec(900, Some(50), &[(7, "from-first")], None),
        rec(901, Some(50), &[(8, "from-second")], None),
    ];

    let (list, skipped) = entries(records);

    assert_eq!((summary(&list), skipped), (vec![(50, 7, "from-first", 0, 0)], 0));
}

#[test]
fn entries_extensions_before_base_apply_in_extension_input_order() {
    let records = vec![
        rec(901, Some(50), &[(8, "second-in-input")], None),
        rec(900, Some(50), &[(7, "first-in-input")], None),
        rec(50, None, &[], None),
    ];

    let (list, _) = entries(records);

    assert_eq!(summary(&list), vec![(50, 8, "second-in-input", 0, 0)]);
}

#[test]
fn entries_base_own_name_wins_over_extension_names() {
    let records = vec![rec(50, None, &[(6, "own")], None), rec(900, Some(50), &[(7, "ext")], None)];

    let (list, _) = entries(records);

    assert_eq!(summary(&list), vec![(50, 6, "own", 0, 0)]);
}

#[test]
fn entries_base_own_data_wins_over_extension_data() {
    let records = vec![rec(50, None, &[(6, "f")], Some((10, 16))), rec(900, Some(50), &[], Some((99, 99)))];

    let (list, _) = entries(records);

    assert_eq!(summary(&list), vec![(50, 6, "f", 10, 16)]);
}

#[test]
fn entries_base_without_data_takes_first_extension_data() {
    let records = vec![
        rec(50, None, &[(6, "f")], None),
        rec(900, Some(50), &[], Some((11, 12))),
        rec(901, Some(50), &[], Some((99, 99))),
    ];

    let (list, _) = entries(records);

    assert_eq!(summary(&list), vec![(50, 6, "f", 11, 12)]);
}

#[test]
fn entries_base_with_zero_data_keeps_it_over_extension_data() {
    // `Option::or` keeps a present value even when it is all zero.
    let records = vec![rec(50, None, &[(6, "f")], Some((0, 0))), rec(900, Some(50), &[], Some((99, 99)))];

    let (list, _) = entries(records);

    assert_eq!(summary(&list), vec![(50, 6, "f", 0, 0)]);
}

#[test]
fn entries_base_nameless_with_name_in_extension_uses_extension_name() {
    let records = vec![rec(50, None, &[], Some((5, 8))), rec(900, Some(50), &[(7, "late-name")], None)];

    let (list, skipped) = entries(records);

    assert_eq!((summary(&list), skipped), (vec![(50, 7, "late-name", 5, 8)], 0));
}

#[test]
fn entries_extension_of_an_extension_is_dropped() {
    let records = vec![
        rec(50, None, &[], None),
        rec(60, Some(50), &[(7, "a")], None),
        rec(70, Some(60), &[(8, "b")], Some((999, 999))),
    ];

    let (list, _) = entries(records);

    assert_eq!(summary(&list), vec![(50, 7, "a", 0, 0)]);
}

#[test]
fn entries_duplicate_base_numbers_merge_extension_into_the_last() {
    let records = vec![
        rec(50, None, &[(6, "first")], None),
        rec(50, None, &[(6, "second")], None),
        rec(900, Some(50), &[], Some((77, 80))),
    ];

    let (list, _) = entries(records);

    assert_eq!(summary(&list), vec![(50, 6, "first", 0, 0), (50, 6, "second", 77, 80)]);
}

#[test]
fn entries_surrogate_in_extension_of_directory_gives_zero_size_file() {
    let mut base = rec(50, None, &[(6, "link")], None);
    base.is_dir = true;
    let mut ext = rec(900, Some(50), &[], None);
    ext.surrogate = true;

    let (list, _) = entries(vec![base, ext]);

    assert_eq!((list[0].is_dir, list[0].size, list[0].allocated), (false, 0, 0));
}

#[test]
fn entries_base_without_time_takes_extension_time() {
    let mut ext = rec(900, Some(50), &[], None);
    ext.modified = 77;

    let (list, _) = entries(vec![rec(50, None, &[(6, "f")], None), ext]);

    assert_eq!(list[0].modified, 77);
}

#[test]
fn entries_base_time_wins_over_extension_time() {
    let mut base = rec(50, None, &[(6, "f")], None);
    base.modified = 12;
    let mut ext = rec(900, Some(50), &[], None);
    ext.modified = 77;

    let (list, _) = entries(vec![ext, base]);

    assert_eq!(list[0].modified, 12);
}

#[test]
fn entries_parsed_records_carry_their_time() {
    let mut f = file_at(ROOT, "t.txt", 10, at(50));
    let r = parse_record(&mut f, 30).unwrap().unwrap();

    let (list, _) = entries(vec![r]);

    assert_eq!(list[0].modified, 50);
}

// ---- entries: parity with the 81181b2 oracle on seeded record sets ----

/// A mix of bases (some nameless, some with several names, some with duplicate numbers) and
/// extensions (before or after their base, missing base, extension of an extension).
fn records(seed: u64, n: usize) -> Vec<Record> {
    let mut rng = Rng::new(seed);
    let mut out: Vec<Record> = Vec::new();
    let mut numbers: Vec<u64> = Vec::new();
    let mut ext_numbers: Vec<u64> = Vec::new();
    for k in 0..n {
        let mut r = rec(0, None, &[], None);
        r.is_dir = rng.chance(30);
        r.surrogate = rng.chance(10);
        for j in 0..rng.below(3) {
            let parent = 5 + rng.below(20) as u64;
            r.names.push(FileName { parent, name: format!("n{k}-{j}") });
        }
        if rng.chance(70) {
            r.data = Some(DataSize { size: rng.below(1000) as u64, allocated: rng.below(1000) as u64 });
        }
        if rng.chance(60) {
            r.modified = 1 + rng.below(1000) as u32;
        }
        if rng.chance(20) && !numbers.is_empty() {
            r.base = Some(match rng.below(10) {
                0 => 9_000_000 + k as u64, // base never seen
                1 if !ext_numbers.is_empty() => ext_numbers[rng.below(ext_numbers.len())], // chain
                _ => numbers[rng.below(numbers.len())],
            });
            r.number = 1_000_000 + k as u64;
            ext_numbers.push(r.number);
        } else if rng.chance(5) && !numbers.is_empty() {
            r.number = numbers[rng.below(numbers.len())]; // duplicate base number
        } else {
            r.number = 16 + k as u64;
            numbers.push(r.number);
        }
        out.push(r);
    }
    for i in (1..out.len()).rev() {
        if rng.chance(15) {
            out.swap(i, rng.below(i + 1)); // extensions land before their base
        }
    }
    out
}

#[test]
fn entries_seeded_record_sets_equal_the_reference() {
    let mut cases = 0;
    for seed in 1..=300u64 {
        let n = 1 + (seed as usize * 7) % 120;
        let input = records(seed, n);

        let got = entries(input.clone());

        assert_same_entries(&got, &entries_reference(input));
        cases += 1;
    }
    assert_eq!(cases, 300);
}

// ---- scan_device (volume image) ----

fn sample_volume() -> Vec<(u64, Vec<u8>)> {
    vec![
        (16, dir(ROOT, "data")),
        (17, file(16, "a.bin", 5000)),
        (18, dir(16, "sub")),
        (19, file(18, "b.txt", 100)),
        (20, file(ROOT, "top.txt", 7)),
    ]
}

#[test]
fn scan_device_fragmented_mft_builds_tree() {
    let (tree, _) = scan_image(volume(&sample_volume()), "").unwrap();

    assert_eq!((child_size(&tree, "data"), child_size(&tree, "top.txt")), (5100, 7));
}

#[test]
fn scan_device_whole_volume_counts_mft_itself() {
    let (tree, _) = scan_image(volume(&sample_volume()), "").unwrap();

    assert_eq!(tree.node(tree.root()).size, 5107 + child_size(&tree, "$MFT"));
}

#[test]
fn scan_device_root_is_named_as_given() {
    let (tree, _) = scan_image(volume(&sample_volume()), "").unwrap();

    assert_eq!(tree.name(tree.root()), "X:\\");
}

#[test]
fn scan_device_reports_method_mft_and_counts() {
    let (_, stats) = scan_image(volume(&sample_volume()), "data").unwrap();

    assert_eq!((stats.method, stats.files, stats.dirs, stats.skipped), (Method::Mft, 2, 2, 0));
}

#[test]
fn scan_device_sub_path_roots_tree_at_folder() {
    let (tree, _) = scan_image(volume(&sample_volume()), "data/sub").unwrap();

    let names: Vec<&str> = tree.children(tree.root()).map(|n| tree.name(n)).collect();
    assert_eq!((tree.name(tree.root()), tree.node(tree.root()).size, names), ("X:\\", 100, vec!["b.txt"]));
}

fn timed_volume() -> Vec<(u64, Vec<u8>)> {
    vec![
        (16, dir_at(ROOT, "data", at(9_000))), // a folder's own time is ignored
        (17, file_at(16, "a.bin", 5000, at(300))),
        (18, dir_at(16, "sub", at(9_000))),
        (19, file_at(18, "b.txt", 100, at(200))),
        (20, file_at(ROOT, "top.txt", 7, at(100))),
        (21, file(16, "untimed.bin", 50)),
        (22, dir_at(ROOT, "empty", at(9_000))),
    ]
}

#[test]
fn scan_device_files_keep_their_time_and_folders_roll_up() {
    let (tree, stats) = scan_image(volume(&timed_volume()), "").unwrap();

    let got: Vec<u32> = ["data/a.bin", "data/sub/b.txt", "top.txt", "data/untimed.bin", "data/sub", "data", "empty"]
        .iter()
        .map(|p| modified_at(&tree, p))
        .collect();
    assert_eq!(got, vec![300, 200, 100, UNKNOWN, 200, 300, UNKNOWN]);
    assert_eq!((tree.node(tree.root()).modified, child_size(&tree, "data"), stats.skipped), (300, 5150, 0));
}

#[test]
fn scan_device_sub_path_keeps_times() {
    let (tree, _) = scan_image(volume(&timed_volume()), "data").unwrap();

    let got: Vec<u32> =
        ["a.bin", "sub/b.txt", "sub", "untimed.bin"].iter().map(|p| modified_at(&tree, p)).collect();
    assert_eq!((got, tree.node(tree.root()).modified), (vec![300, 200, 200, UNKNOWN], 300));
}

#[test]
fn scan_device_sub_path_missing_returns_no_root() {
    let result = scan_image(volume(&sample_volume()), "nope");

    assert!(matches!(result, Err(ScanError::NoRoot)));
}

#[test]
fn scan_device_sub_path_to_file_returns_error() {
    let result = scan_image(volume(&sample_volume()), "top.txt");

    assert!(matches!(result, Err(ScanError::Io(_))));
}

#[test]
fn scan_device_fixup_mismatch_counts_one_skipped() {
    let mut records = sample_volume();
    records[4].1[1022] ^= 0xFF; // top.txt: end of second stride corrupted

    let (tree, stats) = scan_image(volume(&records), "").unwrap();

    assert_eq!((stats.skipped, tree.children(tree.root()).any(|n| tree.name(n) == "top.txt")), (1, false));
}

#[test]
fn scan_device_not_ntfs_returns_error() {
    let mut image = volume(&sample_volume());
    image[3..11].copy_from_slice(b"MSDOS5.0");

    assert!(matches!(scan_image(image, ""), Err(ScanError::Io(_))));
}

#[test]
fn scan_device_short_mft_run_list_returns_error() {
    let image = volume_with(&sample_volume(), Volume { short_runs: 1 });

    assert!(matches!(scan_image(image, ""), Err(ScanError::Io(_))));
}

#[test]
fn scan_device_broken_mft_record_zero_returns_error() {
    let mut image = volume(&sample_volume());
    image[MFT_LCN_A as usize * CLUSTER + 510] ^= 0xFF;

    assert!(matches!(scan_image(image, ""), Err(ScanError::Io(_))));
}

#[test]
fn scan_device_truncated_image_returns_error() {
    let mut image = volume(&sample_volume());
    image.truncate(MFT_LCN_A as usize * CLUSTER + 1024);

    assert!(matches!(scan_image(image, ""), Err(ScanError::Io(_))));
}

#[test]
fn scan_device_cancel_flag_set_returns_cancelled() {
    let (tx, _rx) = channel();

    let result = scan_device(
        &mut Cursor::new(volume(&sample_volume())),
        Path::new(""),
        "X:\\",
        &tx,
        &AtomicBool::new(true),
    );

    assert!(matches!(result, Err(ScanError::Cancelled)));
}

/// Read/seek seam: sets `cancel` when the reader seeks to byte `at`, then reads on normally.
struct CancelOnSeek<'a> {
    inner: Cursor<Vec<u8>>,
    at: u64,
    cancel: &'a AtomicBool,
}

impl Read for CancelOnSeek<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for CancelOnSeek<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let p = self.inner.seek(pos)?;
        if p == self.at {
            self.cancel.store(true, Ordering::Relaxed);
        }
        Ok(p)
    }
}

#[test]
fn scan_device_cancel_during_last_chunk_read_returns_cancelled() {
    // The fixture $MFT has two runs; run B is read last. Cancel arrives while it is read.
    let cancel = AtomicBool::new(false);
    let mut dev = CancelOnSeek {
        inner: Cursor::new(volume(&sample_volume())),
        at: MFT_LCN_B * CLUSTER as u64,
        cancel: &cancel,
    };
    let (tx, _rx) = channel();

    let result = scan_device(&mut dev, Path::new(""), "X:\\", &tx, &cancel);

    assert!(matches!(result, Err(ScanError::Cancelled)), "got {:?}", result.map(|(_, s)| s.files));
}

#[test]
fn scan_device_reports_progress() {
    let (tx, rx) = channel();

    scan_device(&mut Cursor::new(volume(&sample_volume())), Path::new(""), "X:\\", &tx, &AtomicBool::new(false))
        .unwrap();

    assert!(rx.try_iter().last().is_some_and(|p| p.files > 0));
}

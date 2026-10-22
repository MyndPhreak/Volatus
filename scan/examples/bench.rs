//! Scan benchmark: times the real `scan::scan_with` path, run after run.
//!
//! ```text
//! cargo run -p scan --release --example bench -- <root> [--runs N] [--method mft|walk]
//!     [--chunk-mib N] [--threads N] [--csv out.csv] [--json]
//! ```
//! No `--method`: the MFT, falling back to the walker, as the app does. `--method mft` never
//! falls back, so an unelevated run fails instead of timing the walker. See docs/BENCHMARK.md.

use scan::{DEFAULT_CHUNK, Method, ScanOptions, ScanStats, scan_with};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const USAGE: &str = "usage: bench <root> [--runs N] [--method mft|walk] [--chunk-mib N] [--threads N] \
                     [--csv out.csv] [--json]";

#[derive(Debug)]
struct Args {
    root: PathBuf,
    runs: usize,
    opts: ScanOptions,
    csv: Option<PathBuf>,
    json: bool,
}

/// One timed scan.
#[derive(Debug)]
struct Run {
    total: Duration,
    stats: ScanStats,
    /// Size of the root, in bytes.
    bytes: u64,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut root = None;
    let mut args =
        Args { root: PathBuf::new(), runs: 3, opts: ScanOptions::default(), csv: None, json: false };
    let number = |v: Option<String>, flag: &str| -> Result<usize, String> {
        v.and_then(|s| s.parse().ok()).filter(|&n| n > 0).ok_or(format!("{flag} needs a whole number above 0"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--runs" => args.runs = number(it.next(), "--runs")?,
            "--chunk-mib" => {
                args.opts.chunk = number(it.next(), "--chunk-mib")?.checked_mul(1 << 20).ok_or("--chunk-mib is too big")?
            }
            "--threads" => args.opts.threads = Some(number(it.next(), "--threads")?),
            "--method" => {
                args.opts.method = match it.next().as_deref() {
                    Some("mft") => Some(Method::Mft),
                    Some("walk") => Some(Method::Walk),
                    _ => return Err("--method needs mft or walk".into()),
                }
            }
            "--csv" => args.csv = Some(it.next().ok_or("--csv needs a file name")?.into()),
            "--json" => args.json = true,
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            path if root.is_none() => root = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    args.root = root.ok_or("missing <root>")?;
    Ok(args)
}

/// Scans `args.root` `args.runs` times through `scan_with`, the path the app uses.
fn bench(args: &Args) -> Result<Vec<Run>, String> {
    let mut runs = Vec::with_capacity(args.runs);
    for i in 0..args.runs {
        let (tx, rx) = std::sync::mpsc::channel();
        // Drained like the app's progress forwarder, so progress costs what it costs there.
        let drain = std::thread::spawn(move || rx.into_iter().count());
        let started = Instant::now();
        let result = scan_with(&args.root, tx, &AtomicBool::new(false), &args.opts);
        let total = started.elapsed();
        let _ = drain.join();
        let (tree, stats) = result.map_err(|e| format!("run {}: scan failed: {e}", i + 1))?;
        runs.push(Run { total, bytes: tree.node(tree.root()).size, stats });
    }
    Ok(runs)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// (min, median, max) in milliseconds. Median of an even count is the mean of the middle two.
fn spread(mut v: Vec<f64>) -> (f64, f64, f64) {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let median = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
    (v[0], median, v[n - 1])
}

/// Phase name and its time in one run.
fn phases(r: &Run) -> [(&'static str, Duration); 5] {
    let p = r.stats.phases;
    [("open", p.open), ("read", p.read), ("parse", p.parse), ("rollup", p.rollup), ("total", r.total)]
}

fn method_name(m: Method) -> &'static str {
    match m {
        Method::Mft => "mft",
        Method::Walk => "walk",
    }
}

fn text_report(args: &Args, runs: &[Run]) -> String {
    let mut s = String::new();
    let chunk = args.opts.chunk as f64 / f64::from(1 << 20);
    let _ = writeln!(s, "root {}  chunk {chunk} MiB  runs {}", args.root.display(), runs.len());
    for (i, r) in runs.iter().enumerate() {
        let st = &r.stats;
        let _ = write!(s, "run {}: {} on {} threads |", i + 1, method_name(st.method), st.threads);
        for (name, d) in phases(r) {
            let _ = write!(s, " {name} {:.1} ms", ms(d));
        }
        let _ = writeln!(s, " | files {} dirs {} skipped {} bytes {}", st.files, st.dirs, st.skipped, r.bytes);
    }
    let _ = writeln!(s, "phase       min ms   median ms      max ms");
    for (k, name) in ["open", "read", "parse", "rollup", "total"].iter().enumerate() {
        let (lo, mid, hi) = spread(runs.iter().map(|r| ms(phases(r)[k].1)).collect());
        let _ = writeln!(s, "{name:<8}{lo:>10.1}{mid:>12.1}{hi:>12.1}");
    }
    let _ = writeln!(s, "read and parse are busy times; on the MFT path they overlap, so they can add up to more than total.");
    s
}

fn json_string(v: &str) -> String {
    let mut s = String::from("\"");
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(s, "\\u{:04x}", c as u32);
            }
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

fn json_report(args: &Args, runs: &[Run]) -> String {
    let mut s = String::new();
    let threads = args.opts.threads.map_or("null".to_string(), |n| n.to_string());
    let _ = write!(
        s,
        "{{\"root\":{},\"chunk_bytes\":{},\"threads_requested\":{threads},\"runs\":[",
        json_string(&args.root.display().to_string()),
        args.opts.chunk
    );
    for (i, r) in runs.iter().enumerate() {
        let st = &r.stats;
        let _ = write!(
            s,
            "{}{{\"method\":\"{}\",\"threads\":{},\"files\":{},\"dirs\":{},\"skipped\":{},\"bytes\":{}",
            if i > 0 { "," } else { "" },
            method_name(st.method),
            st.threads,
            st.files,
            st.dirs,
            st.skipped,
            r.bytes
        );
        for (name, d) in phases(r) {
            let _ = write!(s, ",\"{name}_ms\":{:.3}", ms(d));
        }
        s.push('}');
    }
    s.push_str("],\"summary\":{");
    for (k, name) in ["open", "read", "parse", "rollup", "total"].iter().enumerate() {
        let (lo, mid, hi) = spread(runs.iter().map(|r| ms(phases(r)[k].1)).collect());
        let comma = if k > 0 { "," } else { "" };
        let _ = write!(s, "{comma}\"{name}_ms\":{{\"min\":{lo:.3},\"median\":{mid:.3},\"max\":{hi:.3}}}");
    }
    s.push_str("}}");
    s
}

const CSV_HEADER: &str =
    "unix_time,root,method,chunk_bytes,threads,run,open_ms,read_ms,parse_ms,rollup_ms,total_ms,files,dirs,skipped,bytes";

/// Appends one row per run to `path`, with a header when the file is new or empty.
fn append_csv(path: &PathBuf, args: &Args, runs: &[Run]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    if f.metadata()?.len() == 0 {
        writeln!(f, "{CSV_HEADER}")?;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let root = args.root.display().to_string().replace('"', "\"\"");
    for (i, r) in runs.iter().enumerate() {
        let st = &r.stats;
        let p: Vec<String> = phases(r).iter().map(|(_, d)| format!("{:.3}", ms(*d))).collect();
        writeln!(
            f,
            "{now},\"{root}\",{},{},{},{},{},{},{},{},{}",
            method_name(st.method),
            args.opts.chunk,
            st.threads,
            i + 1,
            p.join(","),
            st.files,
            st.dirs,
            st.skipped,
            r.bytes
        )?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.opts.chunk != DEFAULT_CHUNK && args.opts.method == Some(Method::Walk) {
        eprintln!("note: --chunk-mib has no effect on the walker");
    }
    let runs = match bench(&args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            if args.opts.method == Some(Method::Mft) {
                eprintln!("The MFT path needs an elevated (Administrator) prompt and a local NTFS drive.");
            }
            return ExitCode::FAILURE;
        }
    };
    if let Some(path) = &args.csv
        && let Err(e) = append_csv(path, &args, &runs)
    {
        eprintln!("could not write {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    if args.json {
        println!("{}", json_report(&args, &runs));
    } else {
        print!("{}", text_report(&args, &runs));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_walk_on_temp_folder_reports_every_run() {
        let dir = std::env::temp_dir().join(format!("storagetool-bench-smoke-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.bin"), [0u8; 100]).unwrap();
        std::fs::write(dir.join("sub").join("b.bin"), [0u8; 20]).unwrap();
        let csv = dir.join("out.csv");
        let argv = [dir.to_str().unwrap(), "--runs", "2", "--method", "walk", "--threads", "2", "--csv"];
        let args = parse_args(argv.iter().map(|s| s.to_string()).chain([csv.display().to_string()])).unwrap();

        let runs = bench(&args).unwrap();
        append_csv(args.csv.as_ref().unwrap(), &args, &runs).unwrap();
        let (json, text, rows) =
            (json_report(&args, &runs), text_report(&args, &runs), std::fs::read_to_string(&csv).unwrap());
        let _ = std::fs::remove_dir_all(&dir);

        let counts: Vec<_> = runs.iter().map(|r| (r.stats.files, r.stats.dirs, r.bytes, r.stats.threads)).collect();
        assert_eq!(counts, vec![(2, 2, 120, 2); 2]);
        assert!(json.contains("\"total_ms\":{\"min\":") && json.matches("\"method\":\"walk\"").count() == 2, "{json}");
        assert!(text.contains("median ms"), "{text}");
        assert_eq!(rows.lines().count(), 3, "{rows}");
    }
}

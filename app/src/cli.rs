//! Command line: `volatus [folder] [--timing] [--method auto|mft|walk] [--threads N]`, in any order.

use scan::{Method, ScanOptions};

/// The options that change how the app runs. Carried over by "Restart as administrator".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// `--timing`: show scan timings in the banner and on stderr.
    pub timing: bool,
    /// `--method mft|walk`. `None` for `auto` or when not given.
    pub method: Option<Method>,
    /// `--threads N`. `None` when not given.
    pub threads: Option<usize>,
}

impl Flags {
    /// The scan options for every scan of this run.
    pub fn scan_options(&self) -> ScanOptions {
        ScanOptions { method: self.method, threads: self.threads, ..ScanOptions::default() }
    }
}

/// What the command line asked for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub flags: Flags,
    /// Folder to scan at start. The first one wins.
    pub path: Option<String>,
    /// One sentence for each option that was ignored or corrected, for the banner.
    pub notices: Vec<String>,
}

/// [`parse_args_with`] for this computer.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Args {
    parse_args_with(args, std::thread::available_parallelism().map_or(1, |n| n.get()))
}

/// Threads allowed for each logical CPU. A larger `--threads` is lowered to this.
const THREADS_PER_CPU: usize = 4;

/// Parses the arguments after the program name. `logical` is the number of logical CPUs.
///
/// - `--method auto|mft|walk` and `--threads N`, also as `--method=mft`. The value is not case
///   sensitive. When an option repeats, the last good value wins. A bad or missing value gives a
///   notice and leaves the option as it was. `--threads` above `logical` x 4 is lowered, with a notice.
/// - A value is the next word, unless that word starts with `--`.
/// - `--` ends the options: what follows is a folder, even if it starts with `--`.
/// - Another `--word` is ignored, with a notice.
pub fn parse_args_with<I: IntoIterator<Item = String>>(args: I, logical: usize) -> Args {
    let cap = logical.max(1) * THREADS_PER_CPU;
    let mut out = Args::default();
    let mut flags_ended = false;
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        let Some(option) = arg.strip_prefix("--").filter(|_| !flags_ended) else {
            out.path.get_or_insert(arg);
            continue;
        };
        if option.is_empty() {
            flags_ended = true;
            continue;
        }
        let (name, inline) = option.split_once('=').map_or((option, None), |(n, v)| (n, Some(v.to_owned())));
        match name {
            "timing" if inline.is_none() => out.flags.timing = true,
            "method" | "threads" => {
                let value = inline.or_else(|| args.next_if(|next| !next.starts_with("--")));
                if name == "method" {
                    match value.as_deref().and_then(parse_method) {
                        Some(method) => out.flags.method = method,
                        None => out.notices.push("Invalid value for --method. Use auto, mft or walk.".into()),
                    }
                } else {
                    match value.as_deref().and_then(parse_threads) {
                        Some(n) if n > cap => {
                            out.flags.threads = Some(cap);
                            out.notices.push(format!("The value for --threads is too high. Using {cap}."));
                        }
                        Some(n) => out.flags.threads = Some(n),
                        None => out.notices.push(format!("Invalid value for --threads. Use a number from 1 to {cap}.")),
                    }
                }
            }
            _ => out.notices.push(format!("Unknown option ignored: {arg}.")),
        }
    }
    out
}

/// `Some(None)` for `auto`: the default choice of scanner.
fn parse_method(value: &str) -> Option<Option<Method>> {
    [("auto", None), ("mft", Some(Method::Mft)), ("walk", Some(Method::Walk))]
        .into_iter()
        .find(|(name, _)| value.eq_ignore_ascii_case(name))
        .map(|(_, method)| method)
}

/// A count of one or more: ASCII digits only. A number too big for `usize` is `usize::MAX`.
fn parse_threads(value: &str) -> Option<usize> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(value.parse().unwrap_or(usize::MAX)).filter(|&n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    fn args(timing: bool, path: Option<&str>) -> Args {
        Args { flags: Flags { timing, ..Flags::default() }, path: path.map(Into::into), notices: vec![] }
    }

    #[test]
    fn parse_args_none_gives_defaults() {
        assert_eq!(parse(&[]), args(false, None));
    }

    #[test]
    fn parse_args_timing_only_sets_the_flag() {
        assert_eq!(parse(&["--timing"]), args(true, None));
    }

    #[test]
    fn parse_args_path_only_sets_the_path() {
        assert_eq!(parse(&["C:\\Data"]), args(false, Some("C:\\Data")));
    }

    #[test]
    fn parse_args_timing_then_path() {
        assert_eq!(parse(&["--timing", "C:\\Data"]), args(true, Some("C:\\Data")));
    }

    #[test]
    fn parse_args_path_then_timing() {
        assert_eq!(parse(&["C:\\Data", "--timing"]), args(true, Some("C:\\Data")));
    }

    #[test]
    fn parse_args_path_with_spaces_stays_one_argument() {
        assert_eq!(parse(&["C:\\My Files\\Data"]), args(false, Some("C:\\My Files\\Data")));
    }

    #[test]
    fn parse_args_separator_makes_a_dash_path_a_folder() {
        assert_eq!(parse(&["--", "--timing"]), args(false, Some("--timing")));
        assert_eq!(parse(&["--timing", "--", "--odd"]), args(true, Some("--odd")));
    }

    #[test]
    fn parse_args_separator_alone_gives_no_path() {
        assert_eq!(parse(&["--"]), args(false, None));
    }

    #[test]
    fn parse_args_unknown_flag_is_ignored() {
        let a = parse(&["--nope", "C:\\Data"]);
        assert_eq!((a.flags, a.path.as_deref()), (Flags::default(), Some("C:\\Data")));
    }

    #[test]
    fn parse_args_second_path_is_ignored() {
        assert_eq!(parse(&["C:\\A", "C:\\B"]), args(false, Some("C:\\A")));
    }

    #[test]
    fn parse_args_empty_path_is_kept() {
        assert_eq!(parse(&[""]), args(false, Some("")));
    }

    // ---- options ----
    const CPUS: usize = 8;
    const CAP: usize = 32;
    const UNKNOWN: &str = "Volatus ignored the unknown option ";
    const BAD_METHOD: &str = "The value for --method is not correct. Use auto, mft or walk.";
    const BAD_THREADS: &str = "The value for --threads is not correct. Use a number from 1 to 32.";
    const HIGH_THREADS: &str = "The value for --threads is too high. Volatus uses 32.";

    fn parse_n(args: &[&str]) -> Args {
        parse_args_with(args.iter().map(|s| s.to_string()), CPUS)
    }

    fn method(args: &[&str]) -> (Option<Method>, Vec<String>) {
        let a = parse_n(args);
        (a.flags.method, a.notices)
    }

    fn threads(args: &[&str]) -> (Option<usize>, Vec<String>) {
        let a = parse_n(args);
        (a.flags.threads, a.notices)
    }

    fn one(text: &str) -> Vec<String> {
        vec![text.to_string()]
    }

    #[test]
    fn parse_args_method_values_set_the_method() {
        let cases = [("mft", Some(Method::Mft)), ("walk", Some(Method::Walk)), ("auto", None)];
        for (value, want) in cases {
            assert_eq!(method(&["--method", value]), (want, vec![]), "value {value}");
            assert_eq!(method(&[&format!("--method={value}")]), (want, vec![]), "value {value} with =");
        }
    }

    #[test]
    fn parse_args_method_value_ignores_case() {
        assert_eq!(method(&["--method", "MFT"]), (Some(Method::Mft), vec![]));
        assert_eq!(method(&["--method=Walk"]), (Some(Method::Walk), vec![]));
        assert_eq!(method(&["--method", "AUTO"]), (None, vec![]));
    }

    #[test]
    fn parse_args_method_repeated_last_one_wins() {
        assert_eq!(method(&["--method", "mft", "--method", "walk"]).0, Some(Method::Walk));
        assert_eq!(method(&["--method=mft", "--method=auto"]).0, None);
        assert_eq!(method(&["--method", "walk", "--method=mft"]).0, Some(Method::Mft));
    }

    #[test]
    fn parse_args_method_bad_values_give_a_notice_and_the_default() {
        for bad in ["fast", "", "mft2", "m", " mft"] {
            assert_eq!(method(&["--method", bad]), (None, one(BAD_METHOD)), "value {bad:?}");
            assert_eq!(method(&[&format!("--method={bad}")]), (None, one(BAD_METHOD)), "value {bad:?} with =");
        }
    }

    #[test]
    fn parse_args_method_without_a_value_gives_a_notice() {
        assert_eq!(method(&["--method"]), (None, one(BAD_METHOD)));
    }

    #[test]
    fn parse_args_method_followed_by_a_flag_does_not_swallow_it() {
        let a = parse_n(&["--method", "--timing"]);
        assert_eq!((a.flags.timing, a.flags.method, a.notices), (true, None, one(BAD_METHOD)));
        let a = parse_n(&["--method", "--", "C:\\Data"]);
        assert_eq!((a.path.as_deref(), a.notices), (Some("C:\\Data"), one(BAD_METHOD)));
    }

    #[test]
    fn parse_args_method_takes_the_next_word_as_its_value_even_if_it_is_a_folder() {
        let a = parse_n(&["--method", "C:\\Data"]);
        assert_eq!((a.path, a.notices), (None, one(BAD_METHOD)));
    }

    #[test]
    fn parse_args_method_bad_repeat_keeps_the_earlier_value() {
        assert_eq!(method(&["--method", "mft", "--method", "fast"]), (Some(Method::Mft), one(BAD_METHOD)));
    }

    #[test]
    fn parse_args_threads_numbers_set_the_count() {
        for (value, want) in [("1", 1), ("4", 4), ("8", 8), ("31", 31), ("32", 32), ("007", 7)] {
            assert_eq!(threads(&["--threads", value]), (Some(want), vec![]), "value {value}");
            assert_eq!(threads(&[&format!("--threads={value}")]), (Some(want), vec![]), "value {value} with =");
        }
    }

    #[test]
    fn parse_args_threads_repeated_last_one_wins() {
        assert_eq!(threads(&["--threads", "2", "--threads=6"]).0, Some(6));
    }

    #[test]
    fn parse_args_threads_bad_values_give_a_notice_and_the_default() {
        for bad in ["0", "00", "abc", "-1", "+4", "4.5", "", " 4", "4 ", "1e3", "\u{663}"] {
            assert_eq!(threads(&["--threads", bad]), (None, one(BAD_THREADS)), "value {bad:?}");
            assert_eq!(threads(&[&format!("--threads={bad}")]), (None, one(BAD_THREADS)), "value {bad:?} with =");
        }
    }

    #[test]
    fn parse_args_threads_without_a_value_gives_a_notice() {
        assert_eq!(threads(&["--threads"]), (None, one(BAD_THREADS)));
        let a = parse_n(&["--threads", "--timing"]);
        assert_eq!((a.flags.timing, a.flags.threads, a.notices), (true, None, one(BAD_THREADS)));
    }

    #[test]
    fn parse_args_threads_above_four_times_the_cpus_is_capped_with_a_notice() {
        for value in ["33", "64", "4294967296", "99999999999999999999999999"] {
            assert_eq!(threads(&["--threads", value]), (Some(CAP), one(HIGH_THREADS)), "value {value}");
        }
    }

    #[test]
    fn parse_args_threads_cap_follows_the_cpu_count() {
        let a = parse_args_with(["--threads".to_string(), "5".to_string()], 1);
        assert_eq!((a.flags.threads, a.notices), (Some(4), one("The value for --threads is too high. Volatus uses 4.")));
        let a = parse_args_with(["--threads".to_string(), "5".to_string()], 0);
        assert_eq!(a.flags.threads, Some(4), "zero CPUs counts as one");
    }

    #[test]
    fn parse_args_threads_bad_repeat_keeps_the_earlier_value() {
        assert_eq!(threads(&["--threads", "3", "--threads", "0"]), (Some(3), one(BAD_THREADS)));
    }

    #[test]
    fn parse_args_unknown_option_gives_a_notice() {
        let a = parse_n(&["--foo", "C:\\Data"]);
        assert_eq!((a.path.as_deref(), a.notices), (Some("C:\\Data"), one(&format!("{UNKNOWN}--foo."))));
        assert_eq!(parse_n(&["--foo=bar"]).notices, one(&format!("{UNKNOWN}--foo=bar.")));
        assert_eq!(parse_n(&["--timing=1"]).notices, one(&format!("{UNKNOWN}--timing=1.")));
        assert_eq!(parse_n(&["--Method", "mft"]).notices[0], format!("{UNKNOWN}--Method."));
    }

    #[test]
    fn parse_args_unknown_option_after_the_separator_is_a_folder_without_notice() {
        let a = parse_n(&["--", "--foo"]);
        assert_eq!((a.path.as_deref(), a.notices), (Some("--foo"), vec![]));
    }

    #[test]
    fn parse_args_notices_keep_the_order_of_the_command_line() {
        let a = parse_n(&["--foo", "--method", "x", "--threads", "0", "--bar"]);
        let want = vec![format!("{UNKNOWN}--foo."), BAD_METHOD.to_string(), BAD_THREADS.to_string(), format!("{UNKNOWN}--bar.")];
        assert_eq!(a.notices, want);
    }

    #[test]
    fn parse_args_good_command_lines_give_no_notice() {
        assert_eq!(parse_n(&["C:\\Data", "--timing", "--method", "walk", "--threads", "8"]).notices, Vec::<String>::new());
    }

    #[test]
    fn parse_args_folder_named_like_an_option_after_the_separator_is_a_folder() {
        for name in ["--method", "--threads", "--timing", "--method=mft", "--threads=4", "--"] {
            let a = parse_n(&["--method", "walk", "--", name]);
            assert_eq!(a.path.as_deref(), Some(name), "folder {name}");
            assert_eq!((a.flags.method, a.notices), (Some(Method::Walk), vec![]), "folder {name}");
        }
    }

    #[test]
    fn parse_args_values_after_the_separator_are_not_options() {
        let a = parse_n(&["--", "C:\\A", "--method", "mft", "--threads=2"]);
        assert_eq!((a.path.as_deref(), a.flags, a.notices), (Some("C:\\A"), Flags::default(), vec![]));
    }

    #[test]
    fn parse_args_options_and_folder_in_any_order() {
        let folder = "C:\\My Files\\caf\u{e9} \u{4e2d}\u{6587}";
        let want = Flags { timing: true, method: Some(Method::Walk), threads: Some(8) };
        let options = [vec!["--timing"], vec!["--method", "walk"], vec!["--threads=8"]];
        let orders = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
        for order in orders {
            let words: Vec<&str> = order.iter().flat_map(|&i| options[i].clone()).collect();
            // The folder goes before, between and after the option groups, never between an option and its value.
            let mut cuts = vec![0];
            let mut at = 0;
            for &i in &order {
                at += options[i].len();
                cuts.push(at);
            }
            for cut in cuts {
                let mut argv = words.clone();
                argv.insert(cut, folder);
                let a = parse_n(&argv);
                assert_eq!((a.path.as_deref(), a.flags, a.notices.len()), (Some(folder), want, 0), "argv {argv:?}");
            }
        }
    }

    #[test]
    fn parse_args_without_options_the_flags_are_the_defaults() {
        assert_eq!(parse_n(&["C:\\Data"]).flags, Flags::default());
    }

    #[test]
    fn scan_options_carry_method_and_threads_and_keep_the_default_chunk() {
        let flags = Flags { timing: true, method: Some(Method::Mft), threads: Some(3) };
        let want = ScanOptions { method: Some(Method::Mft), threads: Some(3), ..ScanOptions::default() };
        assert_eq!(flags.scan_options(), want);
        assert_eq!(Flags::default().scan_options(), ScanOptions::default());
    }
}

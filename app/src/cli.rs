//! Command line: `storagetool [--timing] [--] [folder]`, flags and folder in any order.

/// What the command line asked for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// `--timing`: show scan timings in the banner and on stderr.
    pub timing: bool,
    /// Folder to scan at start. The first one wins.
    pub path: Option<String>,
}

/// Parses the arguments after the program name. `--timing` is the only flag. `--` ends the
/// flags: what follows is a folder, even if it starts with `--`. Another `--word` is ignored.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Args {
    let mut out = Args::default();
    let mut flags_ended = false;
    for arg in args {
        match arg.as_str() {
            "--" if !flags_ended => flags_ended = true,
            "--timing" if !flags_ended => out.timing = true,
            _ if !flags_ended && arg.starts_with("--") => {}
            _ => {
                out.path.get_or_insert(arg);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    fn args(timing: bool, path: Option<&str>) -> Args {
        Args { timing, path: path.map(Into::into) }
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
        assert_eq!(parse(&["--nope", "C:\\Data"]), args(false, Some("C:\\Data")));
    }

    #[test]
    fn parse_args_second_path_is_ignored() {
        assert_eq!(parse(&["C:\\A", "C:\\B"]), args(false, Some("C:\\A")));
    }

    #[test]
    fn parse_args_empty_path_is_kept() {
        assert_eq!(parse(&[""]), args(false, Some("")));
    }
}

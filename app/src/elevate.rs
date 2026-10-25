//! "Restart as administrator": relaunch this exe with the `runas` verb.

use scan::Method;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null;
use windows_sys::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// `ShellExecuteW` returns a value above this when it started the program.
const SHELL_EXECUTE_OK: usize = 32;

pub fn is_admin() -> bool {
    // SAFETY: no arguments, no pointers.
    unsafe { IsUserAnAdmin() != 0 }
}

/// Command line argument for `path`, quoted by the `CommandLineToArgvW` rules: backslashes before
/// an embedded quote or the closing quote are doubled, and embedded quotes are escaped.
pub fn quote_arg(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for c in path.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Command line for the elevated copy. `--` comes before the folder, so a folder named like a
/// flag is still read as a folder. An empty `scan_path` gives no folder: the copy opens on the start
/// screen. `--timing`, `--method` and `--threads` are carried over when set (`auto` is the default,
/// so it is left out).
pub fn relaunch_args(scan_path: &str, flags: crate::cli::Flags) -> String {
    let timing = flags.timing.then(|| "--timing".to_string());
    let method = flags.method.map(|m| format!("--method {}", if m == Method::Mft { "mft" } else { "walk" }));
    let threads = flags.threads.map(|n| format!("--threads {n}"));
    let folder = (!scan_path.is_empty()).then(|| format!("-- {}", quote_arg(scan_path)));
    [timing, method, threads, folder].into_iter().flatten().collect::<Vec<_>>().join(" ")
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Starts an elevated copy of this exe that scans `scan_path` (none when empty) with the same `flags`.
/// `Err` if the user declines the UAC prompt or the launch fails: the caller keeps running.
pub fn restart_as_admin(scan_path: &str, flags: crate::cli::Flags) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let verb = wide("runas".as_ref());
    let file = wide(exe.as_os_str());
    let args = wide(relaunch_args(scan_path, flags).as_ref());
    // SAFETY: all strings are NUL terminated and outlive the call.
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), args.as_ptr(), null(), SW_SHOWNORMAL)
    };
    if result as usize > SHELL_EXECUTE_OK { Ok(()) } else { Err(format!("code {}", result as usize)) }
}

#[cfg(test)]
mod tests {
    use super::{quote_arg, relaunch_args, wide};
    use crate::cli::Flags;
    use scan::Method;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    fn flags(timing: bool) -> Flags {
        Flags { timing, ..Flags::default() }
    }

    /// Parses `command_line` with the real Windows rules, as the elevated copy's runtime does.
    fn argv(command_line: &str) -> Vec<String> {
        let line = wide(command_line.as_ref());
        let mut count = 0;
        // SAFETY: `line` is NUL terminated; the returned block is read within `count` and freed once.
        unsafe {
            let list = CommandLineToArgvW(line.as_ptr(), &mut count);
            assert!(!list.is_null());
            let out = (0..count as usize)
                .map(|i| {
                    let p = *list.add(i);
                    let len = (0..).take_while(|&n| *p.add(n) != 0).count();
                    OsString::from_wide(std::slice::from_raw_parts(p, len)).to_string_lossy().into_owned()
                })
                .collect();
            LocalFree(list.cast());
            out
        }
    }

    #[test]
    fn quote_arg_round_trips_through_command_line_to_argv() {
        let cases = [
            r"C:\Data",
            r"C:\Data\",
            r"C:\Data\\",
            r"C:\Data\\\",
            r"C:\",
            r"C:\\",
            r"\\",
            r"\",
            "",
            r"C:\My Files\",
            r"C:\My Files",
            r#"C:\a"b"#,
            r#"C:\a\"b"#,
            r#"C:\a\\"b\"#,
            r#"""#,
            r#""""#,
            "C:\\caf\u{e9}\\\u{4e2d}\u{6587} \u{1f4c1}\\",
            r"\\server\share",
            r"\\server\share\",
            r"\\?\C:\Data\",
        ];
        for path in cases {
            let parsed = argv(&format!("volatus.exe {}", quote_arg(path)));
            assert_eq!(parsed, ["volatus.exe", path], "path {path:?} quoted as {}", quote_arg(path));
        }
    }

    #[test]
    fn quote_arg_trailing_backslash_is_doubled() {
        assert_eq!(quote_arg("C:\\"), "\"C:\\\\\"");
    }

    #[test]
    fn quote_arg_plain_path_is_only_quoted() {
        assert_eq!(quote_arg("C:\\My Files"), "\"C:\\My Files\"");
    }

    #[test]
    fn relaunch_args_without_timing_has_separator_and_quoted_path() {
        assert_eq!(relaunch_args("C:\\My Files", flags(false)), "-- \"C:\\My Files\"");
    }

    #[test]
    fn relaunch_args_with_timing_carries_the_flag() {
        assert_eq!(relaunch_args("C:\\", flags(true)), "--timing -- \"C:\\\\\"");
    }

    #[test]
    fn relaunch_args_with_no_folder_is_empty() {
        assert_eq!(relaunch_args("", flags(false)), "");
    }

    #[test]
    fn relaunch_args_with_no_folder_and_timing_is_only_the_flag() {
        assert_eq!(relaunch_args("", flags(true)), "--timing");
    }

    #[test]
    fn relaunch_args_with_no_folder_parse_to_the_start_screen_with_timing_kept() {
        for timing in [false, true] {
            let line = format!("volatus.exe {}", relaunch_args("", flags(timing)));

            let parsed = crate::cli::parse_args(argv(&line).into_iter().skip(1));

            assert_eq!(parsed, crate::cli::Args { flags: flags(timing), ..Default::default() }, "command line {line:?}");
        }
    }

    #[test]
    fn relaunch_args_dash_path_stays_after_the_separator() {
        assert_eq!(relaunch_args("--timing", flags(false)), "-- \"--timing\"");
    }

    fn full(method: Option<Method>, threads: Option<usize>) -> Flags {
        Flags { timing: true, method, threads }
    }

    #[test]
    fn relaunch_args_carry_method_and_threads_before_the_folder() {
        let f = full(Some(Method::Walk), Some(8));
        assert_eq!(relaunch_args("D:\\", f), "--timing --method walk --threads 8 -- \"D:\\\\\"");
        let f = Flags { timing: false, method: Some(Method::Mft), threads: None };
        assert_eq!(relaunch_args("", f), "--method mft");
        let f = Flags { timing: false, method: None, threads: Some(2) };
        assert_eq!(relaunch_args("", f), "--threads 2");
    }

    #[test]
    fn relaunch_args_auto_method_is_left_out() {
        assert_eq!(relaunch_args("", full(None, None)), "--timing");
    }

    #[test]
    fn relaunch_args_parse_back_to_the_same_options_and_folder() {
        let folders = ["", "C:\\", "C:\\My Files\\", "--method", "--threads=3", "-- x", "caf\u{e9} \"q\"", "\\\\server\\share\\"];
        for timing in [false, true] {
            for method in [None, Some(Method::Mft), Some(Method::Walk)] {
                for threads in [None, Some(1), Some(32)] {
                    for folder in folders {
                        let f = Flags { timing, method, threads };
                        let line = format!("volatus.exe {}", relaunch_args(folder, f));
                        let parsed = crate::cli::parse_args_with(argv(&line).into_iter().skip(1), 8);
                        let path = (!folder.is_empty()).then(|| folder.to_string());
                        let want = crate::cli::Args { flags: f, path, notices: vec![] };
                        assert_eq!(parsed, want, "command line {line:?}");
                    }
                }
            }
        }
    }
}

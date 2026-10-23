//! "Restart as administrator": relaunch this exe with the `runas` verb.

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

/// Command line argument for `path`, quoted so a trailing backslash cannot escape the quote.
pub fn quote_arg(path: &str) -> String {
    let slashes = if path.ends_with('\\') { "\\" } else { "" };
    format!("\"{path}{slashes}\"")
}

/// Command line for the elevated copy. `--` comes before the folder, so a folder named like a
/// flag is still read as a folder. `--timing` is carried over when set.
pub fn relaunch_args(scan_path: &str, timing: bool) -> String {
    let flag = if timing { "--timing " } else { "" };
    format!("{flag}-- {}", quote_arg(scan_path))
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Starts an elevated copy of this exe that scans `scan_path` (and times, if `timing`).
/// `Err` if the user declines the UAC prompt or the launch fails: the caller keeps running.
pub fn restart_as_admin(scan_path: &str, timing: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let verb = wide("runas".as_ref());
    let file = wide(exe.as_os_str());
    let args = wide(relaunch_args(scan_path, timing).as_ref());
    // SAFETY: all strings are NUL terminated and outlive the call.
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), args.as_ptr(), null(), SW_SHOWNORMAL)
    };
    if result as usize > SHELL_EXECUTE_OK { Ok(()) } else { Err(format!("code {}", result as usize)) }
}

#[cfg(test)]
mod tests {
    use super::{quote_arg, relaunch_args, wide};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

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
            let parsed = argv(&format!("storagetool.exe {}", quote_arg(path)));
            assert_eq!(parsed, ["storagetool.exe", path], "path {path:?} quoted as {}", quote_arg(path));
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
        assert_eq!(relaunch_args("C:\\My Files", false), "-- \"C:\\My Files\"");
    }

    #[test]
    fn relaunch_args_with_timing_carries_the_flag() {
        assert_eq!(relaunch_args("C:\\", true), "--timing -- \"C:\\\\\"");
    }

    #[test]
    fn relaunch_args_dash_path_stays_after_the_separator() {
        assert_eq!(relaunch_args("--timing", false), "-- \"--timing\"");
    }
}

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

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Starts an elevated copy of this exe with `scan_path` as its argument.
/// `Err` if the user declines the UAC prompt or the launch fails: the caller keeps running.
pub fn restart_as_admin(scan_path: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let verb = wide("runas".as_ref());
    let file = wide(exe.as_os_str());
    let args = wide(quote_arg(scan_path).as_ref());
    // SAFETY: all strings are NUL terminated and outlive the call.
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), args.as_ptr(), null(), SW_SHOWNORMAL)
    };
    if result as usize > SHELL_EXECUTE_OK { Ok(()) } else { Err(format!("code {}", result as usize)) }
}

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn quote_arg_trailing_backslash_is_doubled() {
        assert_eq!(quote_arg("C:\\"), "\"C:\\\\\"");
    }

    #[test]
    fn quote_arg_plain_path_is_only_quoted() {
        assert_eq!(quote_arg("C:\\My Files"), "\"C:\\My Files\"");
    }
}

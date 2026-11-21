//! In-house terminal coloring (replaces the colored crate)
//!
//! Implements only the 10 `Colorize` methods actually used by the project, all pure ANSI escapes, zero external dependencies.
//! - Non-Windows platforms: terminals support ANSI natively, emit directly.
//! - Windows: console virtual terminal processing must be enabled (see `enable_virtual_terminal`,
//!   mirroring `colored::control::set_virtual_terminal`), otherwise CMD/PowerShell
//!   prints escape sequences as literal characters.
//!
//! Behavior alignment: always emit colors (the original project called `colored::control::set_override(true)`
//! to force it on), with no TTY detection, avoiding behavior drift in piped/log scenarios.

/// All coloring methods return [`String`], implementing `Display` like `colored`'s `ColoredString`,
/// but lighter and naturally usable in `String` operations.
pub trait Colorize {
    fn red(self) -> String;
    fn green(self) -> String;
    fn yellow(self) -> String;
    fn blue(self) -> String;
    fn magenta(self) -> String;
    fn cyan(self) -> String;
    fn bright_white(self) -> String;
    fn bold(self) -> String;
    fn dimmed(self) -> String;
    fn underline(self) -> String;
}

const RESET: &str = "\x1b[0m";

/// Wrap a string with the given SGR code (start code … text … reset).
///
/// Chained calls (e.g. `"x".red().bold()`) produce multiple escape layers, but all attributes
/// before the first `RESET` already apply, so the visual result is correct with no side effects.
fn paint(s: &str, code: &str) -> String {
    format!("\x1b[{code}m{s}{RESET}")
}

/// Uniformly implement `Colorize` for all `AsRef<str>` types (`str` / `String` / `&str` / `&String` / `Cow<str>` etc.).
/// `self` is taken by value, but `&str`/`&String` are `Copy`,
/// so no underlying data is moved; consuming `String` by value also matches the original `colored` semantics.
macro_rules! impl_colorize {
    ($($name:ident => $code:literal),* $(,)?) => {
        impl<T: AsRef<str>> Colorize for T {
            $(fn $name(self) -> String { paint(self.as_ref(), $code) })*
        }
    };
}

impl_colorize! {
    red      => "31",
    green    => "32",
    yellow   => "33",
    blue     => "34",
    magenta  => "35",
    cyan     => "36",
    bright_white => "97",
    bold     => "1",
    dimmed   => "2",
    underline => "4",
}

/// Enable Windows console virtual terminal processing so ANSI escapes work in CMD/PowerShell.
///
/// Mirrors `colored::control::set_virtual_terminal(true)`: for the stdout/stderr console handles
/// add `ENABLE_VIRTUAL_TERMINAL_PROCESSING`. If a handle is not a console (e.g. redirected to a pipe),
/// `GetConsoleMode` returns 0 and is safely skipped, leaving piped scenarios unaffected.
#[cfg(windows)]
pub fn enable_virtual_terminal() {
    use std::os::windows::io::AsRawHandle;

    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    unsafe {
        for h in [
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ] {
            let mut mode = 0u32;
            if GetConsoleMode(h as *mut std::ffi::c_void, &mut mode) != 0 {
                let _ = SetConsoleMode(
                    h as *mut std::ffi::c_void,
                    mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                );
            }
        }
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn GetConsoleMode(hConsoleHandle: *mut std::ffi::c_void, lpMode: *mut u32) -> i32;
    fn SetConsoleMode(hConsoleHandle: *mut std::ffi::c_void, dwMode: u32) -> i32;
}

// Non-Windows terminals support ANSI natively and need no handling; main.rs calls
// `enable_virtual_terminal` only under `#[cfg(target_os = "windows")]`,
// so no empty non-Windows variant is provided here (avoids dead_code).

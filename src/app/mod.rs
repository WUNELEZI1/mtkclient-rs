//! Application bootstrap helpers extracted from `main.rs`.
//!
//! Keeps `main.rs` thin: only the crate-level lint attributes, module
//! declarations and `fn main()` remain there. These helpers are private to
//! the binary crate, so `pub(crate)` visibility is sufficient.

pub(crate) mod avb;
pub(crate) mod logger;
pub(crate) mod platform;

pub(crate) use avb::cmd_avb_offline;
pub(crate) use logger::{TeeLogger, TerminalLogger};
pub(crate) use platform::check_windows_version;

#[cfg(target_os = "windows")]
pub(crate) use platform::{SetConsoleCP, SetConsoleOutputCP};

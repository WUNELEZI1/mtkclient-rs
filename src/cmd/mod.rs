//! User command entry points.
//!
//! Submodules:
//! - `cli`            — CLI argument parsing
//! - `dump`           — image extraction such as dumppreloader
//! - `io`             — IO commands such as r/w/e/reboot/slot
//! - `partition_table` — partition table commands such as printgpt / rl / wl
//! - `handle`         — single-command entry handle_command (Phase1/Phase2)
//! - `dispatch`       — unified dispatch via dispatch_cmd / execute_single_command
//! - `zyb`            — zyb subcommand handling via handle_zyb_command
//!
//! Top-level entry points:
//! - `print_help`       — print help information
//! - `handle_command`   — single-command entry (Phase1 dump/bypass + Phase2 DA commands)

pub mod bootimg;
pub mod buildprop;
pub mod cli;
pub mod detect;
pub mod dump;
pub mod proinfo;
// Removed: fastboot is now a separate binary, fastboot-rs
pub mod io;
pub mod multi;
pub mod partition_table;
pub mod plan;
pub mod preloader_boot_mode;
pub mod rpmb;
pub mod script;
pub mod sim;
pub mod xml;

// Heavy entry functions live in submodules to keep this file under 500 lines
mod dispatch;
mod handle;
mod zyb;

// Helper modules split out of the top level
mod help;
mod resume;
mod validate;

// Re-export pub items moved from submodules so `crate::cmd::<name>` paths stay stable
pub use dispatch::{dispatch_cmd, execute_single_command};
pub use handle::handle_command;
pub use help::print_help;
#[cfg(test)]
pub use resume::pending_read_resume_in_dir;
pub use resume::{is_transport_error, pending_read_resume_cwd};
// PendingRead is public; keep the top-level `crate::cmd::PendingRead` path
#[allow(unused_imports)]
pub use resume::PendingRead;
pub(crate) use resume::{active_read_resume_exists, active_write_resume_exists};
pub(crate) use validate::validate_command;
pub(crate) use xml::handle_xml_command;
pub(crate) use zyb::handle_zyb_command;

#[cfg(test)]
mod tests;

//! Reboot command implementation, split into focused submodules.
//!
//! The pure strategy planner lives in `plan.rs`, the command and its I/O helpers in `command.rs`,
//! and the table-driven tests in `tests.rs`.

#[path = "reboot/plan.rs"]
mod plan;

#[path = "reboot/command.rs"]
mod command;

#[cfg(test)]
#[path = "reboot/tests.rs"]
mod tests;

pub use command::cmd_reboot;

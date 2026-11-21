//! Device I/O commands.
//!
//! - `cmd_read`     — read partition data into a file
//! - `cmd_write`    — write a file to a partition (optionally verify)
//! - `cmd_erase`    — erase a partition
//! - `cmd_reboot`   — reboot the device (system/fastboot/recovery/fastbootd)
//! - `cmd_slot`     — show/switch A/B slot
//! - `cmd_peek`     — read device memory (hex dump output)
//! - `cmd_poke`     — write device memory (hex input)

pub(crate) mod boot_mode;
pub(crate) mod erase;
pub(crate) mod imgshell;
pub(crate) mod read;
pub(crate) mod reboot;
pub(crate) mod slot;
pub(crate) mod write;

pub use erase::cmd_erase_data;
pub use imgshell::cmd_imgshell;
pub use read::{cmd_peek, cmd_poke, cmd_read, parse_addr, parse_size};
pub use reboot::cmd_reboot;
pub use slot::cmd_slot;

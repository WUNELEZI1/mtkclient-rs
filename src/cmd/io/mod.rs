//! 设备 IO 命令
//!
//! - `cmd_read`     — 读取分区数据到文件
//! - `cmd_write`    — 写入文件到分区（支持 verify）
//! - `cmd_erase`    — 擦除分区
//! - `cmd_reboot`   — 重启设备（支持 system/fastboot/recovery/fastbootd）
//! - `cmd_slot`     — 显示/切换 A/B 槽位
//! - `cmd_peek`     — 读取设备内存（hex dump 输出）
//! - `cmd_poke`     — 写入设备内存（hex 数据输入）

pub(crate) mod read;
pub(crate) mod write;
pub(crate) mod erase;
pub(crate) mod reboot;
pub(crate) mod boot_mode;
pub(crate) mod slot;

pub use read::{cmd_read, cmd_peek, cmd_poke, parse_addr, parse_size};
pub use write::cmd_write;
pub use erase::{cmd_erase, cmd_erase_data};
pub use reboot::cmd_reboot;
pub use slot::cmd_slot;

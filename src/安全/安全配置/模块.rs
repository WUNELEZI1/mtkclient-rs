//! SecCfg 处理模块（Bootloader 解锁/锁定）
//!
//! 子模块：
//! - `v4`    — SecCfgV4 结构与解析/构建（28 字节头 + SHA256+AES 签名）
//! - `v3`    — SecCfgV3 结构与解析/构建（44 字节头 + SEJ 加密段）
//! - `build` — 共享的 header 构建辅助（build_v4_header / build_v4_image_online）
//! - `cmd`   — 顶层 `unlock_bootloader` / `lock_bootloader` 入口

#[path = "构建.rs"]
pub mod build;
#[path = "命令.rs"]
pub mod cmd;
#[path = "V3.rs"]
pub mod v3;
#[path = "V4.rs"]
pub mod v4;

pub use cmd::{lock_bootloader, unlock_bootloader};

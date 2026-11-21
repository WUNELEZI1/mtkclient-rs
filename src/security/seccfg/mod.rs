//! SecCfg 处理模块（Bootloader 解锁/锁定）
//!
//! 子模块：
//! - `v4`    — SecCfgV4 结构与解析/构建（28 字节头 + SHA256+AES 签名）
//! - `v3`    — SecCfgV3 结构与解析/构建（44 字节头 + SEJ 加密段）
//! - `build` — 共享的 header 构建辅助（build_v4_header / sign_v4_digest）
//! - `plain` — V3 解密后明文的结构化解析与打分（用于细分 SW/V2/V3/V4）
//! - `cmd`   — 顶层 `seccfg info` 只读查询入口
//!
//! 解锁/上锁统一走 [`crate::ops::seccfg::SeccfgOp`]，V3/V4 全版本支持。

#[path = "build.rs"]
pub mod build;
#[path = "cmd.rs"]
pub mod cmd;
#[path = "info.rs"]
pub mod info;
#[path = "plain.rs"]
pub mod plain;
#[path = "v3.rs"]
pub mod v3;
#[path = "v4.rs"]
pub mod v4;

pub use cmd::{seccfg_info, seccfg_info_file};

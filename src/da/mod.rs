//! Download Agent 系统
//!
//! 对齐 Python mtkclient Library/DA/ 目录结构：
//! - `xflash` — XFlash 协议原语、DA 结构体（DAXFlash）、EMI 提取、诊断、IO
//! - `loader` — DA 两阶段上传（header 解析 + env 初始化 + upload）
//! - `ext`    — DA Extension 命令（patch + generate + meta/adb 控制）
//!
//! DAXFlash 是整个 DA 子系统的核心结构体，在此 re-export。

pub mod ext;
pub mod loader;
pub mod xflash;
pub mod xml;

// 核心 re-export：让外部模块通过 crate::da::DAXFlash 使用
pub use xflash::DAXFlash;
pub use xflash::EmmcInfo;
// XML DA V6 re-export（供外部模块使用）
#[allow(unused_imports)]
pub use xml::DAXML;

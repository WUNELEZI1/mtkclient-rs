//! Kamakiri2 漏洞利用模块
//!
//! 包含 Kamakiri2 漏洞利用的完整流程：
//! - `da_read` / `da_write`：通过 kamakiri2 漏洞读写 BROM 内存（[`da_io`]）
//! - `inject_payload`：注入 kamakiri2 payload（[`inject`]）
//! - `dump_preloader_payload`：流式 dump preloader（[`dump`]）
//! - `bypass_security`：注入 patcher payload 绕过安全保护（[`bypass`]）
//!
//! 子模块通过 `impl Preloader` 块扩展 `Preloader` 结构体的方法
//! （Rust 允许同一类型的 impl 块分散在多个文件中）。

use crate::preloader::Preloader;

pub(crate) mod bypass;
pub(crate) mod da_io;
pub(crate) mod dump;
pub(crate) mod inject;
pub(crate) mod kamakiri2_common;
pub(crate) mod payload;
pub(crate) mod step;

// =============================================================================
// 公共访问器
// =============================================================================

impl Preloader {
    /// 获取 DA BRA 指针（带 chip 配置回退）
    pub(crate) fn ptr_da_bra(&self) -> u32 {
        let chip = self.chip.unwrap();
        chip.ptr_da_bra.unwrap_or(chip.brom_register_access.1)
    }

    /// 获取 ptr_send_addr（带 chip 配置回退）
    pub(crate) fn ptr_send_addr(&self) -> u32 {
        let chip = self.chip.unwrap();
        chip.ptr_send_addr.unwrap_or(chip.send_ptr.1)
    }
}

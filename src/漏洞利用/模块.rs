//! Kamakiri2 漏洞利用模块
//!
//! 包含 Kamakiri2 漏洞利用的完整流程：
//! - `da_read` / `da_write`：通过 kamakiri2 漏洞读写 BROM 内存（[`DA输入输出`]）
//! - `inject_payload`：注入 kamakiri2 payload（[`注入`]）
//! - `dump_preloader_payload`：流式 dump preloader（[`转储`]）
//! - `bypass_security`：注入 patcher payload 绕过安全保护（[`绕过安全`]）
//!
//! 子模块通过 `impl Preloader` 块扩展 `Preloader` 结构体的方法
//! （Rust 允许同一类型的 impl 块分散在多个文件中）。

use crate::预加载器::Preloader;

#[path = "DA输入输出.rs"]
pub(crate) mod DA输入输出;
#[path = "公共.rs"]
pub(crate) mod 公共;
#[path = "步骤.rs"]
pub(crate) mod 步骤;
#[path = "注入.rs"]
pub(crate) mod 注入;
#[path = "绕过安全.rs"]
pub(crate) mod 绕过安全;
#[path = "转储.rs"]
pub(crate) mod 转储;
#[path = "载荷.rs"]
pub(crate) mod 载荷;

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

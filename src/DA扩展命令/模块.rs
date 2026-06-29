//! DA Extensions 子模块
//!
//! - `patches`  — DA1/DA2 通用/独有 patches（binary 搜索 + apply + 公共 patches 表）
//! - `generate` — generate_da_extensions（DA Extensions 模板填充 + 通配符搜索）
//! - `cmd`      — set_meta / enable_adb_and_reboot（DA 模式控制接口）
//!
//! 全部以 `impl DAXFlash` 块扩展 `DAXFlash` 结构体的方法。
//! 模块级公共 API（`patch_da1`/`patch_da2`/`find_binary`/`find_binary_wildcard`）
//! 仅供 `impl DAXFlash` 块内部使用，不向外 re-export（外部调用走方法）。

#[path = "命令.rs"]
pub mod 命令;
#[path = "生成.rs"]
pub mod 生成;
#[path = "补丁.rs"]
pub mod 补丁;

/// DA extensions 模板（预编译的 da_x.bin）
pub const DA_EXTENSIONS_TEMPLATE: &[u8] =
    include_bytes!("../../archive/mtkclient-2.0.1/mtkclient/payloads/da_x.bin");

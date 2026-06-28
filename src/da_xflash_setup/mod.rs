//! DA 上传初始化模块（拆分后入口）
//!
//! 子模块：
//! - `header` — MTK AllInOne DA 文件解析（parse_da_header / parse_da_regions / DaRegion）
//! - `env`    — XFlash 协议环境初始化（setup_env / setup_hw_init / send_emi / boot_to）
//! - `upload` — DA 两阶段上传（upload_da1 / upload_da2）
//!
//! 注意：本模块仍保留为 `crate::da_xflash_setup`（与原文件同名），
//! 只是由单文件 `da_xflash_setup.rs` 拆分为 `da_xflash_setup/` 目录。

pub mod env;
pub mod header;
pub mod upload;

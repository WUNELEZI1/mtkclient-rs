//! DA 上传初始化模块（拆分后入口）
//!
//! 子模块：
//! - `header` — MTK AllInOne DA 文件解析（parse_da_header / parse_da_regions / DaRegion）
//! - `env`    — XFlash 协议环境初始化（setup_env / setup_hw_init / send_emi / boot_to）
//! - `upload` — DA 两阶段上传（upload_da1 / upload_da2）
//!
//! 注意：本模块位于 `crate::da::loader`，
//! 由原 `da_loader/` 目录迁移而来。

#[path = "env.rs"]
pub mod env;
#[path = "header.rs"]
pub mod header;
#[path = "upload.rs"]
pub mod upload;

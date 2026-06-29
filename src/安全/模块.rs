//! 安全配置系统
//!
//! 负责加密签名、Bootloader 解锁、安全配置管理
//!
//! # 系统架构
//! - `sej` - SEJ/HACC 加密签名后端
//! - `seccfg` - 安全配置解析与修改(V3/V4)
//! - `vbmeta` - vbmeta 禁用与修补
//! - `frp` - FRP (Factory Reset Protection) 解锁

#[path = "FRP.rs"]
pub mod frp;
#[path = "安全配置/模块.rs"]
pub mod seccfg;
#[path = "SEJ.rs"]
pub mod sej;
#[path = "VBMETA.rs"]
pub mod vbmeta;

// 公共 API re-export
#[allow(unused_imports)]
pub use frp::frp_unlock;
#[allow(unused_imports)]
pub use seccfg::{lock_bootloader, unlock_bootloader};
#[allow(unused_imports)]
pub use sej::sej_hacc_sign;
#[allow(unused_imports)]
pub use vbmeta::vbmeta_disable;

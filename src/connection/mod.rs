//! 连接管理系统
//!
//! 负责设备连接、驱动安装、会话管理
//!
//! # 系统架构
//! - `driver` - Windows WinUSB 驱动安装/切换
//! - `manager` - 设备连接管理器（串口握手/WinUSB直连/自动降级）
//! - `filter` - USB 设备过滤与检测
//! - `session` - DA 会话状态管理

pub mod driver;
pub mod filter;
pub mod manager;
pub mod session;

// 公共 API re-export
#[allow(unused_imports)]
pub use manager::{ConnectionManager, DeviceMode};
#[allow(unused_imports)]
pub use session::{SessionState, reset_session, save_da_session, try_reuse_da_session};

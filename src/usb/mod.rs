//! USB 通信系统
//!
//! 负责底层 USB 通信、设备交互、日志追踪
//!
//! # 系统架构
//! - `上下文` - USB 阶段枚举、设备检测函数（nusb 无需显式 context）
//! - `设备` - USB 设备结构体与生命周期管理（nusb Interface/Device）
//! - `设备IO` - USB 设备 IO 操作（读取/写入/控制传输/清除停顿）
//! - `设备握手` - BROM 握手协议
//! - `诊断` - USB 连接诊断与状态检测
//! - `日志` - USB 通信追踪日志（--usb-log）

#[path = "context.rs"]
pub mod context;
#[path = "device.rs"]
pub mod device;
#[path = "device_handshake.rs"]
pub mod device_handshake;
#[path = "device_io.rs"]
pub mod device_io;
#[path = "diag.rs"]
pub mod diag;
#[path = "log.rs"]
pub mod log;

// 公共 re-export，保持外部调用方式不变
#[allow(unused_imports)]
pub use context::{USB上下文, USB阶段, 获取第一个联发科VIDPID};
pub use device::USB设备;
#[allow(unused_imports)]
pub use log::{usb_trace, 设置USB日志开关, 设置USB读取静默};
// 注意：usb_trace_tx! / usb_trace_rx! 由 #[macro_export] 在 crate 根导出，
// 调用方应使用 crate::usb_trace_tx! / crate::usb_trace_rx!

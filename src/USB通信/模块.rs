//! USB 通信系统
//!
//! 负责底层 USB 通信、设备交互、日志追踪
//!
//! # 系统架构
//! - `上下文` - libusb context、USB阶段 枚举、设备检测函数
//! - `设备` - USB设备 结构体与生命周期管理
//! - `设备IO` - USB设备 IO 操作（读取/写入/控制传输/清除停顿）
//! - `设备握手` - BROM 握手协议
//! - `诊断` - USB 连接诊断与状态检测
//! - `日志` - USB 通信追踪日志（--usb-log）

#[path = "上下文.rs"]
pub mod 上下文;
#[path = "日志.rs"]
pub mod 日志;
#[path = "设备.rs"]
pub mod 设备;
#[path = "设备IO.rs"]
pub mod 设备IO;
#[path = "设备握手.rs"]
pub mod 设备握手;
#[path = "诊断.rs"]
pub mod 诊断;

// 公共 re-export，保持外部调用方式不变
#[allow(unused_imports)]
pub use 上下文::{
    USB上下文, USB阶段, 是否有联发科设备, 获取第一个联发科VIDPID, 通过libusb检测联发科设备,
};
#[allow(unused_imports)]
pub use 日志::{usb_trace, 设置USB日志开关, 设置USB读取静默};
pub use 设备::USB设备;
// 注意：usb_trace_tx! / usb_trace_rx! 由 #[macro_export] 在 crate 根导出，
// 调用方应使用 crate::usb_trace_tx! / crate::usb_trace_rx!

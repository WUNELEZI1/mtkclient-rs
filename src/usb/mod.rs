//! USB 通信系统
//!
//! 负责底层 USB 通信、设备交互、日志追踪
//!
//! # 系统架构
//! - `context` - libusb context、UsbStage 枚举、设备检测函数
//! - `device` - UsbDevice 结构体与生命周期管理
//! - `device_io` - UsbDevice IO 操作（read/write/ctrl_transfer/clear_halt）
//! - `device_handshake` - BROM 握手协议
//! - `diag` - USB 连接诊断与状态检测
//! - `log` - USB 通信追踪日志（--usb-log）

pub mod context;
pub mod device;
pub mod device_handshake;
pub mod device_io;
pub mod diag;
pub mod log;

// 公共 re-export，保持外部调用方式不变
#[allow(unused_imports)]
pub use context::{
    UsbContext, UsbStage, check_mediatek_device_via_libusb, get_first_mediatek_vid_pid,
    has_any_mediatek_device,
};
pub use device::UsbDevice;
#[allow(unused_imports)]
pub use log::{set_quiet_usb_read, set_usb_log_enabled, usb_trace};
// 注意：usb_trace_tx! / usb_trace_rx! 由 #[macro_export] 在 crate 根导出，
// 调用方应使用 crate::usb_trace_tx! / crate::usb_trace_rx!

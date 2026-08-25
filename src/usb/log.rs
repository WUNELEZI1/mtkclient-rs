//! USB 通信追踪日志
//!
//! 通过 `--usb-log` 启用。当 `--usb-log` 开启时，所有日志（含 USB trace）
//! 由全局 TeeLogger 统一输出到当前目录下的 `usb_debug.log`（覆盖模式）。
//! 本模块仅保留 QUIET_USB_READ 控制标志和 usb_trace 辅助函数。

use log::trace;
use std::sync::atomic::{AtomicBool, Ordering};

/// 全局静默标志：设置为 true 时，read() 不打印 [USB READ] 日志
pub(crate) static QUIET_USB_READ: AtomicBool = AtomicBool::new(false);

/// 全局 USB trace 开关：设置为 true 时，usb_trace 输出 trace 日志
pub(crate) static USB_LOG_ENABLED: AtomicBool = AtomicBool::new(false);

/// 设置 USB 读取静默模式
pub fn set_usb_read_quiet(quiet: bool) {
    QUIET_USB_READ.store(quiet, Ordering::Relaxed);
}

/// 设置 USB trace 开关
pub fn set_usb_log_switch(enabled: bool) {
    USB_LOG_ENABLED.store(enabled, Ordering::Relaxed);
}

/// USB 通信追踪日志，通过 trace! 宏输出，由全局 logger 捕获写入文件
pub fn usb_trace(direction: &str, func_info: &str, data: &[u8]) {
    if !USB_LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    // 限制 hex 数据长度，避免巨量分配（大包只记录前 128 字节）
    let hex_max = 128;
    let hex_str = if data.len() <= hex_max {
        data.iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        let prefix = data[..hex_max]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "{} ({} bytes, showing first {})",
            prefix,
            data.len(),
            hex_max
        )
    };

    trace!("[USB {}] {} {}", direction, func_info, hex_str);
}

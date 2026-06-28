//! USB 通信追踪日志（写入 usb_debug.log）
//!
//! 通过 `--usb-log` 启用，独立于 `--debugmode`。
//! 格式：[HH:MM:SS.mmm] [TX/RX] [函数名::行号] hex_data

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// 全局静默标志：设置为 true 时，read() 不打印 [USB READ] 日志
pub(crate) static QUIET_USB_READ: AtomicBool = AtomicBool::new(false);

/// 全局 USB trace 开关：设置为 true 时，记录所有 USB 通信到 usb_debug.log
pub(crate) static USB_LOG_ENABLED: AtomicBool = AtomicBool::new(false);

/// USB trace 日志文件（使用 Mutex 保证线程安全写入）
pub(crate) static USB_LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

const USB_LOG_FILENAME: &str = "usb_debug.log";

/// 设置 USB 读取静默模式
pub fn set_quiet_usb_read(quiet: bool) {
    QUIET_USB_READ.store(quiet, Ordering::Relaxed);
}

/// 设置 USB trace 开关
pub fn set_usb_log_enabled(enabled: bool) {
    USB_LOG_ENABLED.store(enabled, Ordering::Relaxed);
    if enabled {
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(USB_LOG_FILENAME)
        {
            Ok(file) => {
                let mut guard = USB_LOG_FILE.lock().unwrap();
                *guard = Some(file);
            }
            Err(e) => {
                eprintln!("[ERROR] 无法打开 {}: {}", USB_LOG_FILENAME, e);
            }
        }
    } else {
        let mut guard = USB_LOG_FILE.lock().unwrap();
        *guard = None;
    }
}

/// USB 通信追踪日志，对齐 Python usb_debug.log 格式
/// 格式：[HH:MM:SS.mmm] [TX/RX] [函数名::行号] hex_data
pub fn usb_trace(direction: &str, func_info: &str, data: &[u8]) {
    if !USB_LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    // 转换为本地时间（简化版，UTC+8 东八区）
    let total_secs = secs % 86400;
    let hours = (total_secs / 3600) as u32;
    let minutes = ((total_secs % 3600) / 60) as u32;
    let seconds = (total_secs % 60) as u32;

    let hex_str: String = data
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(" ");

    let log_line = format!(
        "[{:02}:{:02}:{:02}.{:03}] [{}] [{}] {}\n",
        hours, minutes, seconds, millis, direction, func_info, hex_str
    );

    if let Ok(mut guard) = USB_LOG_FILE.lock()
        && let Some(ref mut file) = *guard
    {
        let _ = file.write_all(log_line.as_bytes());
        let _ = file.flush();
    }
}

/// 辅助函数：格式化行号信息
#[macro_export]
macro_rules! usb_trace_tx {
    ($data:expr) => {
        $crate::usb::usb_trace("TX", &format!("{}:{}", file!(), line!()), $data);
    };
}

#[macro_export]
macro_rules! usb_trace_rx {
    ($data:expr) => {
        $crate::usb::usb_trace("RX", &format!("{}:{}", file!(), line!()), $data);
    };
}

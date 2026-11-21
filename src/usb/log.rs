//! USB communication trace log
//!
//! Enabled via `--usb-log`. When `--usb-log` is on, all logs (including USB trace)
//! are written by the global TeeLogger to `usb_debug.log` in the current directory (overwrite mode).
//! This module only keeps the QUIET_USB_READ control flag and the usb_trace helper.

use log::trace;
use std::sync::atomic::{AtomicBool, Ordering};

/// Global quiet flag: when true, read() does not print the [USB READ] log
pub(crate) static QUIET_USB_READ: AtomicBool = AtomicBool::new(false);

/// Global USB trace switch: when true, usb_trace emits trace logs
pub(crate) static USB_LOG_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable/disable USB read quiet mode
pub fn set_usb_read_quiet(quiet: bool) {
    QUIET_USB_READ.store(quiet, Ordering::Relaxed);
}

/// Enable/disable USB trace logging
pub fn set_usb_log_switch(enabled: bool) {
    USB_LOG_ENABLED.store(enabled, Ordering::Relaxed);
}

/// USB communication trace log, emitted via the trace! macro and captured by the global logger into the file
pub fn usb_trace(direction: &str, func_info: &str, data: &[u8]) {
    if !USB_LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    // Cap the hex data length to avoid huge allocations (large packets log only the first 128 bytes)
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

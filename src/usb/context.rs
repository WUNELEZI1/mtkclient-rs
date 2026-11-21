//! USB context + device detection
//!
//! - `UsbContext` — nusb context wrapper (nusb needs no explicit context, kept for API compatibility)
//! - `UsbStage`   — device stage enum (BROM / Preloader / DA / Unknown)
//! - `get_first_mtk_vid_pid` — enumerate the first MediaTek BROM device via nusb (startup diagnostics +
//!   direct probing on non-Windows platforms)

use crate::system::config::DeviceType;
use nusb::MaybeFuture;

/// USB device stage
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum UsbStage {
    Brom,
    Preloader,
    Da,
    Unknown,
}

impl UsbStage {
    pub fn from_pid(pid: u16) -> Self {
        match pid {
            0x0003 => UsbStage::Brom,
            0x2000 => UsbStage::Preloader,
            0x2001 => UsbStage::Da,
            _ => UsbStage::Unknown,
        }
    }
}

/// USB context (nusb needs no explicit context; struct kept for API compatibility)
pub struct UsbContext {
    /// nusb uses no global context; this field is a marker only
    _valid: bool,
}

impl UsbContext {
    pub fn new() -> Result<Self, String> {
        // nusb needs no explicit initialization, return directly
        Ok(UsbContext { _valid: true })
    }
}

/// Enumerate the nusb device list and find a MediaTek BROM device
fn scan_nusb_devices() -> Vec<(u16, u16)> {
    let mut result = Vec::new();
    let devices = match nusb::list_devices().wait() {
        Ok(d) => d,
        Err(_) => return result,
    };
    for dev in devices {
        if dev.vendor_id() == 0x0E8D && dev.product_id() == 0x0003 {
            result.push((dev.vendor_id(), dev.product_id()));
        }
    }
    result
}

/// Enumerate USB devices and return the (VID, PID, DeviceType) of the first MediaTek device
pub fn get_first_mtk_vid_pid() -> Option<(u16, u16, DeviceType)> {
    scan_nusb_devices().into_iter().find_map(|(vid, pid)| {
        let device_type = DeviceType::from_vid_pid(vid, pid);
        log::trace!(
            "[USB] 获取第一个联发科VIDPID: 找到 BROM 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
            vid,
            pid,
            device_type
        );
        Some((vid, pid, device_type))
    })
}

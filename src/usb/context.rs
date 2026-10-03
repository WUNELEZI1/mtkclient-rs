//! USB 上下文 + 设备检测
//!
//! - `USB上下文` — nusb 上下文封装（nusb 无需显式 context，这里保留兼容接口）
//! - `USB阶段`   — 设备阶段枚举（BROM / Preloader / DA / 未知）
//! - `获取第一个联发科VID_PID` — nusb 枚举首台联发科 BROM 设备（启动诊断 +
//!   非 Windows 平台的直连探测）

use crate::system::config::DeviceType;
use nusb::MaybeFuture;

/// USB 设备阶段
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

/// USB 上下文（nusb 不需要显式 context，保留结构体以兼容现有 API）
pub struct UsbContext {
    /// nusb 不使用全局 context，此字段仅做标记
    _valid: bool,
}

impl UsbContext {
    pub fn new() -> Result<Self, String> {
        // nusb 不需要显式初始化，直接返回
        Ok(UsbContext { _valid: true })
    }
}

/// 枚举 nusb 设备列表，查找 MediaTek BROM 设备
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

/// 枚举 USB 设备列表,返回第一个 MediaTek 设备的 (VID, PID, DeviceType)
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

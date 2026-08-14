//! USB 上下文 + 设备检测
//!
//! - `USB上下文` — nusb 上下文封装（nusb 无需显式 context，这里保留兼容接口）
//! - `USB阶段`   — 设备阶段枚举（BROM / Preloader / DA / 未知）
//! - `通过libusb检测联发科设备` — 前置检测 BROM 设备
//! - `获取第一个联发科VID_PID` — DA 会话复用检查
//! - `是否有联发科设备` — 判断是否跳过串口扫描

use crate::system::config::DeviceType;
use nusb::MaybeFuture;


/// USB 设备阶段
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum USB阶段 {
    Brom,
    Preloader,
    Da,
    未知,
}

impl USB阶段 {
    pub fn 从PID生成(pid: u16) -> Self {
        match pid {
            0x0003 => USB阶段::Brom,
            0x2000 => USB阶段::Preloader,
            0x2001 => USB阶段::Da,
            _ => USB阶段::未知,
        }
    }
}

/// USB 上下文（nusb 不需要显式 context，保留结构体以兼容现有 API）
pub struct USB上下文 {
    /// nusb 不使用全局 context，此字段仅做标记
    _valid: bool,
}

impl USB上下文 {
    pub fn 新建() -> Result<Self, String> {
        // nusb 不需要显式初始化，直接返回
        Ok(USB上下文 { _valid: true })
    }

}

/// 枚举 nusb 设备列表，查找 MediaTek BROM 设备
fn 扫描nusb设备() -> Vec<(u16, u16)> {
    let mut 结果 = Vec::new();
    let devices = match nusb::list_devices().wait() {
        Ok(d) => d,
        Err(_) => return 结果,
    };
    for dev in devices {
        if dev.vendor_id() == 0x0E8D && dev.product_id() == 0x0003 {
            结果.push((dev.vendor_id(), dev.product_id()));
        }
    }
    结果
}


/// 枚举 USB 设备列表,返回第一个 MediaTek 设备的 (VID, PID, DeviceType)
pub fn 获取第一个联发科VIDPID() -> Option<(u16, u16, DeviceType)> {
    扫描nusb设备()
        .into_iter()
        .find_map(|(vid, pid)| {
            let 设备类型 = DeviceType::from_vid_pid(vid, pid);
            log::trace!(
                "[USB] 获取第一个联发科VIDPID: 找到 BROM 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
                vid,
                pid,
                设备类型
            );
            Some((vid, pid, 设备类型))
        })
}


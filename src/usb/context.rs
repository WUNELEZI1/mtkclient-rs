//! USB 上下文 + 设备检测
//!
//! - `UsbContext` — libusb context 封装（Drop 时自动 libusb_exit）
//! - `UsbStage`   — 设备阶段枚举（BROM / Preloader / DA / Unknown）
//! - `check_mediatek_device_via_libusb` — 前置检测 BROM 设备
//! - `get_first_mediatek_vid_pid` — DA 会话复用检查
//! - `has_any_mediatek_device` — 判断是否跳过串口扫描

use crate::config::DeviceType;
use log::info;

/// libusb 错误码
pub(crate) const LIBUSB_ERROR_TIMEOUT: i32 = -7;

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

pub struct UsbContext {
    ctx: *mut libusb1_sys::libusb_context,
}

impl UsbContext {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
            let ret = libusb1_sys::libusb_init(&mut ctx);
            if ret != 0 {
                return Err(format!(
                    "libusb 初始化失败 (error {})\n\
                     请检查:\n\
                     1. libusb-1.0.dll 是否存在（与 exe 同目录或系统路径）\n\
                     2. 是否被杀毒软件拦截\n\
                     3. 是否有其他程序占用了 libusb",
                    ret
                ));
            }
            Ok(UsbContext { ctx })
        }
    }

    pub fn as_ptr(&self) -> *mut libusb1_sys::libusb_context {
        self.ctx
    }
}

impl Drop for UsbContext {
    fn drop(&mut self) {
        unsafe {
            if !self.ctx.is_null() {
                libusb1_sys::libusb_exit(self.ctx);
                self.ctx = std::ptr::null_mut();
            }
        }
    }
}

/// 枚举 USB 设备列表，检查是否有任何 MediaTek 设备（BROM 0x0003 / Preloader 0x2000/0x2001）
///
/// 返回值：
/// - `Some((pid, device_type))`：找到的第一个 MediaTek 设备
/// - `None`：无 MediaTek 设备
///
/// 用途：在 smart_init 的 COM 扫描之前调用，如果 libusb 已经能看到 BROM 设备 (PID 0x0003)，
/// 就直接走 WinUSB 模式，跳过耗时 21 秒的 COM 扫描。
/// 注意：只识别 BROM 阶段设备 (PID 0x0003)，其他 PID 一律忽略，避免误识别触发错误路径。
pub fn check_mediatek_device_via_libusb() -> Option<(u16, DeviceType)> {
    unsafe {
        let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut ctx) != 0 {
            return None;
        }

        let mut dev_list: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let dev_count = libusb1_sys::libusb_get_device_list(ctx, &mut dev_list);
        if dev_count <= 0 {
            libusb1_sys::libusb_free_device_list(dev_list, 1);
            libusb1_sys::libusb_exit(ctx);
            return None;
        }

        let mut result = None;
        for i in 0..dev_count as isize {
            let dev = *dev_list.wrapping_offset(i);
            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(dev, &mut desc) != 0 {
                continue;
            }
            if desc.idVendor != 0x0E8D {
                continue;
            }
            // 严格仅识别 BROM 阶段 (PID 0x0003)
            if desc.idProduct != 0x0003 {
                continue;
            }
            let dev_type = DeviceType::from_vid_pid(desc.idVendor, desc.idProduct);
            info!(
                "[USB] 前置检测：发现 BROM 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
                desc.idVendor, desc.idProduct, dev_type
            );
            result = Some((desc.idProduct, dev_type));
            break;
        }

        libusb1_sys::libusb_free_device_list(dev_list, 1);
        libusb1_sys::libusb_exit(ctx);
        result
    }
}

/// 枚举 USB 设备列表，返回第一个 MediaTek 设备的 (VID, PID, DeviceType)
///
/// 用途：main.rs 启动时检测 DA 会话复用。
pub fn get_first_mediatek_vid_pid() -> Option<(u16, u16, DeviceType)> {
    unsafe {
        let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut ctx) != 0 {
            return None;
        }

        let mut dev_list: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let dev_count = libusb1_sys::libusb_get_device_list(ctx, &mut dev_list);
        if dev_count <= 0 {
            libusb1_sys::libusb_free_device_list(dev_list, 1);
            libusb1_sys::libusb_exit(ctx);
            return None;
        }

        let mut result = None;
        for i in 0..dev_count as isize {
            let dev = *dev_list.wrapping_offset(i);
            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(dev, &mut desc) != 0 {
                continue;
            }
            if desc.idVendor != 0x0E8D {
                continue;
            }
            if desc.idProduct != 0x0003 {
                continue;
            }
            let dev_type = DeviceType::from_vid_pid(desc.idVendor, desc.idProduct);
            log::trace!(
                "[USB] get_first_mediatek_vid_pid: 找到 BROM 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
                desc.idVendor,
                desc.idProduct,
                dev_type
            );
            result = Some((desc.idVendor, desc.idProduct, dev_type));
            break;
        }

        libusb1_sys::libusb_free_device_list(dev_list, 1);
        libusb1_sys::libusb_exit(ctx);
        result
    }
}

/// 检查当前是否连接了任何 MediaTek USB 设备（VID=0x0E8D）
///
/// 用于在 smart_init 中判断是否跳过串口扫描。
pub fn has_any_mediatek_device() -> bool {
    unsafe {
        let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut ctx) != 0 {
            return false;
        }

        let mut dev_list: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let dev_count = libusb1_sys::libusb_get_device_list(ctx, &mut dev_list);
        if dev_count <= 0 {
            libusb1_sys::libusb_free_device_list(dev_list, 1);
            libusb1_sys::libusb_exit(ctx);
            return false;
        }

        let mut found = false;
        for i in 0..dev_count as isize {
            let dev = *dev_list.wrapping_offset(i);
            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(dev, &mut desc) != 0 {
                continue;
            }
            if desc.idVendor == 0x0E8D {
                found = true;
                break;
            }
        }

        libusb1_sys::libusb_free_device_list(dev_list, 1);
        libusb1_sys::libusb_exit(ctx);
        found
    }
}

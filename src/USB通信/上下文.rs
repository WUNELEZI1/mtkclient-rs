//! USB 上下文 + 设备检测
//!
//! - `USB上下文` — libusb context 封装（Drop 时自动 libusb_exit）
//! - `USB阶段`   — 设备阶段枚举（BROM / Preloader / DA / 未知）
//! - `通过libusb检测联发科设备` — 前置检测 BROM 设备
//! - `获取第一个联发科VID_PID` — DA 会话复用检查
//! - `是否有联发科设备` — 判断是否跳过串口扫描

use crate::config::DeviceType;
use log::info;

/// libusb 错误码
pub(crate) const LIBUSB错误_超时: i32 = -7;

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

pub struct USB上下文 {
    上下文指针: *mut libusb1_sys::libusb_context,
}

impl USB上下文 {
    pub fn 新建() -> Result<Self, String> {
        unsafe {
            let mut 上下文指针: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
            let 返回码 = libusb1_sys::libusb_init(&mut 上下文指针);
            if 返回码 != 0 {
                return Err(format!(
                    "libusb 初始化失败 (error {})\n\
                     请检查:\n\
                     1. libusb-1.0.dll 是否存在（与 exe 同目录或系统路径）\n\
                     2. 是否被杀毒软件拦截\n\
                     3. 是否有其他程序占用了 libusb",
                    返回码
                ));
            }
            Ok(USB上下文 { 上下文指针 })
        }
    }

    pub fn 获取指针(&self) -> *mut libusb1_sys::libusb_context {
        self.上下文指针
    }
}

impl Drop for USB上下文 {
    fn drop(&mut self) {
        unsafe {
            if !self.上下文指针.is_null() {
                libusb1_sys::libusb_exit(self.上下文指针);
                self.上下文指针 = std::ptr::null_mut();
            }
        }
    }
}

/// 枚举 USB 设备列表,检查是否有任何 MediaTek 设备（BROM 0x0003 / Preloader 0x2000/0x2001）
///
/// 返回值：
/// - `Some((pid, device_type))`：找到的第一个 MediaTek 设备
/// - `None`：无 MediaTek 设备
///
/// 用途：在 smart_init 的 COM 扫描之前调用,如果 libusb 已经能看到 BROM 设备 (PID 0x0003),
/// 就直接走 WinUSB 模式,跳过耗时 21 秒的 COM 扫描。
/// 注意：只识别 BROM 阶段设备 (PID 0x0003),其他 PID 一律忽略,避免误识别触发错误路径。
pub fn 通过libusb检测联发科设备() -> Option<(u16, DeviceType)> {
    unsafe {
        let mut 上下文指针: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut 上下文指针) != 0 {
            return None;
        }

        let mut 设备列表: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let 设备数量 = libusb1_sys::libusb_get_device_list(上下文指针, &mut 设备列表);
        if 设备数量 <= 0 {
            libusb1_sys::libusb_free_device_list(设备列表, 1);
            libusb1_sys::libusb_exit(上下文指针);
            return None;
        }

        let mut 结果 = None;
        for i in 0..设备数量 as isize {
            let 设备 = *设备列表.wrapping_offset(i);
            let mut 描述符: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(设备, &mut 描述符) != 0 {
                continue;
            }
            if 描述符.idVendor != 0x0E8D {
                continue;
            }
            // 严格仅识别 BROM 阶段 (PID 0x0003)
            if 描述符.idProduct != 0x0003 {
                continue;
            }
            let 设备类型 = DeviceType::from_vid_pid(描述符.idVendor, 描述符.idProduct);
            info!(
                "[USB] 前置检测：发现 BROM 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
                描述符.idVendor, 描述符.idProduct, 设备类型
            );
            结果 = Some((描述符.idProduct, 设备类型));
            break;
        }

        libusb1_sys::libusb_free_device_list(设备列表, 1);
        libusb1_sys::libusb_exit(上下文指针);
        结果
    }
}

/// 枚举 USB 设备列表,返回第一个 MediaTek 设备的 (VID, PID, DeviceType)
///
/// 用途：main.rs 启动时检测 DA 会话复用。
pub fn 获取第一个联发科VIDPID() -> Option<(u16, u16, DeviceType)> {
    unsafe {
        let mut 上下文指针: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut 上下文指针) != 0 {
            return None;
        }

        let mut 设备列表: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let 设备数量 = libusb1_sys::libusb_get_device_list(上下文指针, &mut 设备列表);
        if 设备数量 <= 0 {
            libusb1_sys::libusb_free_device_list(设备列表, 1);
            libusb1_sys::libusb_exit(上下文指针);
            return None;
        }

        let mut 结果 = None;
        for i in 0..设备数量 as isize {
            let 设备 = *设备列表.wrapping_offset(i);
            let mut 描述符: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(设备, &mut 描述符) != 0 {
                continue;
            }
            if 描述符.idVendor != 0x0E8D {
                continue;
            }
            if 描述符.idProduct != 0x0003 {
                continue;
            }
            let 设备类型 = DeviceType::from_vid_pid(描述符.idVendor, 描述符.idProduct);
            log::trace!(
                "[USB] 获取第一个联发科VIDPID: 找到 BROM 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
                描述符.idVendor,
                描述符.idProduct,
                设备类型
            );
            结果 = Some((描述符.idVendor, 描述符.idProduct, 设备类型));
            break;
        }

        libusb1_sys::libusb_free_device_list(设备列表, 1);
        libusb1_sys::libusb_exit(上下文指针);
        结果
    }
}

/// 检查当前是否连接了任何 MediaTek USB 设备（VID=0x0E8D）
///
/// 用于在 smart_init 中判断是否跳过串口扫描。
pub fn 是否有联发科设备() -> bool {
    unsafe {
        let mut 上下文指针: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut 上下文指针) != 0 {
            return false;
        }

        let mut 设备列表: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let 设备数量 = libusb1_sys::libusb_get_device_list(上下文指针, &mut 设备列表);
        if 设备数量 <= 0 {
            libusb1_sys::libusb_free_device_list(设备列表, 1);
            libusb1_sys::libusb_exit(上下文指针);
            return false;
        }

        let mut 找到 = false;
        for i in 0..设备数量 as isize {
            let 设备 = *设备列表.wrapping_offset(i);
            let mut 描述符: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(设备, &mut 描述符) != 0 {
                continue;
            }
            if 描述符.idVendor == 0x0E8D {
                找到 = true;
                break;
            }
        }

        libusb1_sys::libusb_free_device_list(设备列表, 1);
        libusb1_sys::libusb_exit(上下文指针);
        找到
    }
}

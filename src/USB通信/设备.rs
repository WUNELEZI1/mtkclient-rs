//! USB设备 构造与生命周期（open / close / reopen）
//!
//! - `USB设备::新建`        — 按 SUPPORTED_DEVICES 顺序查找第一个可用设备
//! - `USB设备::按VID_PID打开` — 按指定 VID/PID 打开
//! - `USB设备::关闭`      — 释放接口 + 关闭句柄
//! - `USB设备::重新打开`     — 重新打开设备（用于 BROM↔DA 切换后恢复）

use super::上下文::USB上下文;
use super::上下文::USB阶段;
use crate::config::{DeviceType, SUPPORTED_DEVICES};
use log::{info, trace};
use std::time::Duration;

/// USB bulk 端点默认值（找不到时回退）
const 默认输出端点: u8 = 0x01;
const 默认输入端点: u8 = 0x81;
const 默认最大包大小: u16 = 512;
const 默认超时毫秒: u64 = 5000;
const 重新打开延迟毫秒: u64 = 200;

pub struct USB设备 {
    pub(crate) 设备句柄: *mut libusb1_sys::libusb_device_handle,
    pub vid: u16,
    pub pid: u16,
    pub 阶段: USB阶段,
    pub(crate) 设备类型: DeviceType,
    pub 输出端点: u8,
    pub 输入端点: u8,
    #[allow(dead_code)]
    输出端点最大包大小: u16,
    pub 输入端点最大包大小: u16,
    pub(crate) 超时: Duration,
}

impl USB设备 {
    pub fn 新建(context: &USB上下文) -> Result<Self, String> {
        let 上下文指针 = context.获取指针();
        unsafe {
            // 使用统一配置表查找设备
            let mut 设备句柄 = std::ptr::null_mut();
            let mut 找到的设备类型 = DeviceType::Unknown;
            for dev_config in SUPPORTED_DEVICES {
                设备句柄 = libusb1_sys::libusb_open_device_with_vid_pid(
                    上下文指针,
                    dev_config.vid,
                    dev_config.pid,
                );
                if !设备句柄.is_null() {
                    info!(
                        "[USB] 已连接:{} - {} (VID:0x{:04X} PID:0x{:04X})",
                        dev_config.name, dev_config.description, dev_config.vid, dev_config.pid
                    );
                    找到的设备类型 = dev_config.device_type;
                    break;
                }
            }
            if 设备句柄.is_null() {
                return Err("未找到支持的设备".into());
            }

            // 对齐 Python usblib.py connect():先 claim 0 再 claim 1
            libusb1_sys::libusb_detach_kernel_driver(设备句柄,0);
            let _ = libusb1_sys::libusb_claim_interface(设备句柄,0);
            libusb1_sys::libusb_detach_kernel_driver(设备句柄,1);
            if libusb1_sys::libusb_claim_interface(设备句柄,1) != 0 {
                return Err("claim interface 1 failed".into());
            }

            let 设备 = libusb1_sys::libusb_get_device(设备句柄);
            if 设备.is_null() {
                return Err("获取设备描述失败:设备可能已断开".into());
            }

            let mut 描述符: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            let 返回码_描述符 = libusb1_sys::libusb_get_device_descriptor(设备,&mut 描述符);
            if 返回码_描述符 != 0 {
                return Err(format!(
                    "获取设备描述失败 (error {}): libusb 驱动异常",
                    返回码_描述符
                ));
            }

            trace!("[USB] scanning endpoints...");
            let mut 配置指针: *const libusb1_sys::libusb_config_descriptor = std::ptr::null();
            let mut 输出端点地址: u8 = 默认输出端点;
            let mut 输入端点地址: u8 = 默认输入端点;
            let mut 输出端点最大包: u16 = 默认最大包大小;
            let mut 输入端点最大包: u16 = 默认最大包大小;
            let 返回码 = libusb1_sys::libusb_get_active_config_descriptor(设备,&mut 配置指针);
            if 返回码 != 0 || 配置指针.is_null() {
                info!(
                    "[USB] WARNING: get_active_config_descriptor failed (ret={}), trying known combos...",
                    返回码
                );
            } else {
                let 配置 = &*配置指针;
                for i in 0..配置.bNumInterfaces as isize {
                    let 接口 = &*配置.interface.wrapping_add(i as usize);
                    for j in 0..接口.num_altsetting {
                        let 备用设置 = &*接口.altsetting.wrapping_add(j as usize);
                        trace!(
                            "[USB] interface {} altsetting {} num_endpoints={}",
                            i, j, 备用设置.bNumEndpoints
                        );
                        for k in 0..备用设置.bNumEndpoints as isize {
                            let 端点 = &*备用设置.endpoint.wrapping_add(k as usize);
                            let 地址 = 端点.bEndpointAddress;
                            let 方向 = if 地址 & 0x80 != 0 { "IN" } else { "OUT" };
                            let 端点类型 = match 端点.bmAttributes & 0x03 {
                                0 => "Control",
                                1 => "Isochronous",
                                2 => "Bulk",
                                3 => "Interrupt",
                                _ => "Unknown",
                            };
                            trace!(
                                "[USB]   EP: 0x{:02X} dir={} type={} size={}",
                                地址,方向,端点类型,端点.wMaxPacketSize
                            );
                            if 方向 == "OUT" && 端点类型 == "Bulk" {
                                输出端点地址 = 地址;
                                输出端点最大包 = 端点.wMaxPacketSize;
                            }
                            if 方向 == "IN" && 端点类型 == "Bulk" {
                                输入端点地址 = 地址;
                                输入端点最大包 = 端点.wMaxPacketSize;
                            }
                        }
                    }
                }
                libusb1_sys::libusb_free_config_descriptor(配置指针);
            }

            info!(
                "[USB] EP_OUT=0x{:02X} wMaxPacketSize={} EP_IN=0x{:02X}",
                输出端点地址,输出端点最大包,输入端点地址
            );

            let 阶段 = USB阶段::从PID生成(描述符.idProduct);

            Ok(USB设备 {
                设备句柄,
                vid: 描述符.idVendor,
                pid: 描述符.idProduct,
                阶段,
                设备类型:找到的设备类型,
                输出端点:输出端点地址,
                输入端点:输入端点地址,
                输出端点最大包大小:输出端点最大包,
                输入端点最大包大小:输入端点最大包,
                超时:Duration::from_millis(默认超时毫秒),
            })
        }
    }

    /// 按指定 VID/PID 打开设备
    pub fn 按VID_PID打开(context: &USB上下文, vid: u16, pid: u16) -> Result<Self, String> {
        let 上下文指针 = context.获取指针();
        unsafe {
            let 设备句柄 = libusb1_sys::libusb_open_device_with_vid_pid(上下文指针,vid, pid);
            if 设备句柄.is_null() {
                return Err(format!("未找到设备 VID={:04X} PID={:04X}", vid, pid));
            }

            // 对齐 Python usblib.py connect():先 claim 0 再 claim 1
            libusb1_sys::libusb_detach_kernel_driver(设备句柄,0);
            let _ = libusb1_sys::libusb_claim_interface(设备句柄,0);
            libusb1_sys::libusb_detach_kernel_driver(设备句柄,1);
            if libusb1_sys::libusb_claim_interface(设备句柄,1) != 0 {
                libusb1_sys::libusb_close(设备句柄);
                return Err("claim interface 1 failed".into());
            }

            let 设备 = libusb1_sys::libusb_get_device(设备句柄);
            if 设备.is_null() {
                libusb1_sys::libusb_close(设备句柄);
                return Err("获取设备描述失败:设备可能已断开".into());
            }

            let mut 描述符: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            let 返回码_描述符 = libusb1_sys::libusb_get_device_descriptor(设备,&mut 描述符);
            if 返回码_描述符 != 0 {
                libusb1_sys::libusb_close(设备句柄);
                return Err(format!(
                    "获取设备描述失败 (error {}): libusb 驱动异常",
                    返回码_描述符
                ));
            }

            trace!("[USB] scanning endpoints...");
            let mut 配置指针: *const libusb1_sys::libusb_config_descriptor = std::ptr::null();
            let mut 输出端点地址: u8 = 默认输出端点;
            let mut 输入端点地址: u8 = 默认输入端点;
            let mut 输出端点最大包: u16 = 默认最大包大小;
            let mut 输入端点最大包: u16 = 默认最大包大小;
            let 返回码 = libusb1_sys::libusb_get_active_config_descriptor(设备,&mut 配置指针);
            if 返回码 != 0 || 配置指针.is_null() {
                info!(
                    "[USB] WARNING: get_active_config_descriptor failed (ret={}), using defaults",
                    返回码
                );
            } else {
                let 配置 = &*配置指针;
                for i in 0..配置.bNumInterfaces as isize {
                    let 接口 = &*配置.interface.wrapping_add(i as usize);
                    for j in 0..接口.num_altsetting {
                        let 备用设置 = &*接口.altsetting.wrapping_add(j as usize);
                        for k in 0..备用设置.bNumEndpoints as isize {
                            let 端点 = &*备用设置.endpoint.wrapping_add(k as usize);
                            let 地址 = 端点.bEndpointAddress;
                            let 方向 = if 地址 & 0x80 != 0 { "IN" } else { "OUT" };
                            let 端点类型 = match 端点.bmAttributes & 0x03 {
                                2 => "Bulk",
                                _ => continue,
                            };
                            if 方向 == "OUT" && 端点类型 == "Bulk" {
                                输出端点地址 = 地址;
                                输出端点最大包 = 端点.wMaxPacketSize;
                            }
                            if 方向 == "IN" && 端点类型 == "Bulk" {
                                输入端点地址 = 地址;
                                输入端点最大包 = 端点.wMaxPacketSize;
                            }
                        }
                    }
                }
                libusb1_sys::libusb_free_config_descriptor(配置指针);
            }

            let 阶段 = USB阶段::从PID生成(描述符.idProduct);
            trace!(
                "[USB] 按VID_PID打开 OK: VID={:04X} PID={:04X} stage={:?}",
                vid, pid, 阶段
            );

            Ok(USB设备 {
                设备句柄,
                vid: 描述符.idVendor,
                pid: 描述符.idProduct,
                阶段,
                设备类型:DeviceType::from_vid_pid(vid, 描述符.idProduct),
                输出端点:输出端点地址,
                输入端点:输入端点地址,
                输出端点最大包大小:输出端点最大包,
                输入端点最大包大小:输入端点最大包,
                超时:Duration::from_millis(默认超时毫秒),
            })
        }
    }

    /// 是否是 libusb（WinUSB）后端
    pub fn 是libusb(&self) -> bool {
        true
    }

    /// 获取 EP_OUT 的最大包大小
    #[allow(dead_code)]
    pub fn 获取输出端点最大包大小(&self) -> u16 {
        self.输出端点最大包大小
    }

    pub fn 设置超时(&mut self, duration: Duration) {
        self.超时 = duration;
    }

    pub fn 获取超时(&self) -> Duration {
        self.超时
    }

    pub fn 关闭(&mut self) {
        unsafe {
            if !self.设备句柄.is_null() {
                libusb1_sys::libusb_release_interface(self.设备句柄,1);
                libusb1_sys::libusb_release_interface(self.设备句柄,0);
                libusb1_sys::libusb_close(self.设备句柄);
                self.设备句柄 = std::ptr::null_mut();
            }
        }
    }

    /// 重新打开 USB 设备:关闭旧句柄,等待设备稳定,重新打开并 claim interface
    pub fn 重新打开(&mut self, context: &USB上下文) -> Result<(), String> {
        self.关闭();
        // 等待设备稳定
        std::thread::sleep(Duration::from_millis(重新打开延迟毫秒));

        let mut 新设备 = USB设备::新建(context)?;

        // 交换字段
        self.设备句柄 = 新设备.设备句柄;
        self.输入端点 = 新设备.输入端点;
        self.输出端点 = 新设备.输出端点;
        self.输入端点最大包大小 = 新设备.输入端点最大包大小;
        self.vid = 新设备.vid;
        self.pid = 新设备.pid;
        self.阶段 = 新设备.阶段;
        self.超时 = 新设备.超时;
        self.设备类型 = 新设备.设备类型;
        // 防止新设备 drop 时关闭已转移的句柄
        新设备.设备句柄 = std::ptr::null_mut();
        info!("USB 重连成功");
        Ok(())
    }
}

impl Drop for USB设备 {
    fn drop(&mut self) {
        self.关闭();
    }
}

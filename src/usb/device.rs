//! UsbDevice 构造与生命周期（open / close / reopen）
//!
//! - `UsbDevice::new`        — 按 SUPPORTED_DEVICES 顺序查找第一个可用设备
//! - `UsbDevice::open_by_vid_pid` — 按指定 VID/PID 打开
//! - `UsbDevice::close`      — 释放接口 + 关闭句柄
//! - `UsbDevice::reopen`     — 重新打开设备（用于 BROM↔DA 切换后恢复）

use super::context::UsbContext;
use super::context::UsbStage;
use crate::config::{DeviceType, SUPPORTED_DEVICES};
use log::{info, trace};
use std::time::Duration;

/// USB bulk 端点默认值（找不到时回退）
const DEFAULT_EP_OUT: u8 = 0x01;
const DEFAULT_EP_IN: u8 = 0x81;
const DEFAULT_MAX_PACKET: u16 = 512;
const DEFAULT_TIMEOUT_MS: u64 = 5000;
const REOPEN_DELAY_MS: u64 = 200;

pub struct UsbDevice {
    pub(crate) handle: *mut libusb1_sys::libusb_device_handle,
    pub vid: u16,
    pub pid: u16,
    pub stage: UsbStage,
    pub(crate) device_type: DeviceType,
    pub ep_out: u8,
    pub ep_in: u8,
    #[allow(dead_code)]
    ep_out_max_packet_size: u16,
    pub ep_in_max_packet_size: u16,
    pub(crate) timeout: Duration,
}

impl UsbDevice {
    pub fn new(context: &UsbContext) -> Result<Self, String> {
        let ctx = context.as_ptr();
        unsafe {
            // 使用统一配置表查找设备
            let mut handle = std::ptr::null_mut();
            let mut found_device_type = DeviceType::Unknown;
            for dev_config in SUPPORTED_DEVICES {
                handle = libusb1_sys::libusb_open_device_with_vid_pid(
                    ctx,
                    dev_config.vid,
                    dev_config.pid,
                );
                if !handle.is_null() {
                    info!(
                        "[USB] 已连接: {} - {} (VID:0x{:04X} PID:0x{:04X})",
                        dev_config.name, dev_config.description, dev_config.vid, dev_config.pid
                    );
                    found_device_type = dev_config.device_type;
                    break;
                }
            }
            if handle.is_null() {
                return Err("未找到支持的设备".into());
            }

            // 对齐 Python usblib.py connect()：先 claim 0 再 claim 1
            libusb1_sys::libusb_detach_kernel_driver(handle, 0);
            let _ = libusb1_sys::libusb_claim_interface(handle, 0);
            libusb1_sys::libusb_detach_kernel_driver(handle, 1);
            if libusb1_sys::libusb_claim_interface(handle, 1) != 0 {
                return Err("claim interface 1 failed".into());
            }

            let device = libusb1_sys::libusb_get_device(handle);
            if device.is_null() {
                return Err("获取设备描述失败：设备可能已断开".into());
            }

            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            let ret_desc = libusb1_sys::libusb_get_device_descriptor(device, &mut desc);
            if ret_desc != 0 {
                return Err(format!(
                    "获取设备描述失败 (error {}): libusb 驱动异常",
                    ret_desc
                ));
            }

            trace!("[USB] scanning endpoints...");
            let mut config_ptr: *const libusb1_sys::libusb_config_descriptor = std::ptr::null();
            let mut ep_out_addr: u8 = DEFAULT_EP_OUT;
            let mut ep_in_addr: u8 = DEFAULT_EP_IN;
            let mut ep_out_max_pkt: u16 = DEFAULT_MAX_PACKET;
            let mut ep_in_max_pkt: u16 = DEFAULT_MAX_PACKET;
            let ret = libusb1_sys::libusb_get_active_config_descriptor(device, &mut config_ptr);
            if ret != 0 || config_ptr.is_null() {
                info!(
                    "[USB] WARNING: get_active_config_descriptor failed (ret={}), trying known combos...",
                    ret
                );
            } else {
                let config = &*config_ptr;
                for i in 0..config.bNumInterfaces as isize {
                    let iface = &*config.interface.wrapping_add(i as usize);
                    for j in 0..iface.num_altsetting {
                        let alt = &*iface.altsetting.wrapping_add(j as usize);
                        trace!(
                            "[USB] interface {} altsetting {} num_endpoints={}",
                            i, j, alt.bNumEndpoints
                        );
                        for k in 0..alt.bNumEndpoints as isize {
                            let ep = &*alt.endpoint.wrapping_add(k as usize);
                            let addr = ep.bEndpointAddress;
                            let dir = if addr & 0x80 != 0 { "IN" } else { "OUT" };
                            let ep_type = match ep.bmAttributes & 0x03 {
                                0 => "Control",
                                1 => "Isochronous",
                                2 => "Bulk",
                                3 => "Interrupt",
                                _ => "Unknown",
                            };
                            trace!(
                                "[USB]   EP: 0x{:02X} dir={} type={} size={}",
                                addr, dir, ep_type, ep.wMaxPacketSize
                            );
                            if dir == "OUT" && ep_type == "Bulk" {
                                ep_out_addr = addr;
                                ep_out_max_pkt = ep.wMaxPacketSize;
                            }
                            if dir == "IN" && ep_type == "Bulk" {
                                ep_in_addr = addr;
                                ep_in_max_pkt = ep.wMaxPacketSize;
                            }
                        }
                    }
                }
                libusb1_sys::libusb_free_config_descriptor(config_ptr);
            }

            info!(
                "[USB] EP_OUT=0x{:02X} wMaxPacketSize={} EP_IN=0x{:02X}",
                ep_out_addr, ep_out_max_pkt, ep_in_addr
            );

            let stage = UsbStage::from_pid(desc.idProduct);

            Ok(UsbDevice {
                handle,
                vid: desc.idVendor,
                pid: desc.idProduct,
                stage,
                device_type: found_device_type,
                ep_out: ep_out_addr,
                ep_in: ep_in_addr,
                ep_out_max_packet_size: ep_out_max_pkt,
                ep_in_max_packet_size: ep_in_max_pkt,
                timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            })
        }
    }

    /// 按指定 VID/PID 打开设备
    pub fn open_by_vid_pid(context: &UsbContext, vid: u16, pid: u16) -> Result<Self, String> {
        let ctx = context.as_ptr();
        unsafe {
            let handle = libusb1_sys::libusb_open_device_with_vid_pid(ctx, vid, pid);
            if handle.is_null() {
                return Err(format!("未找到设备 VID={:04X} PID={:04X}", vid, pid));
            }

            // 对齐 Python usblib.py connect()：先 claim 0 再 claim 1
            libusb1_sys::libusb_detach_kernel_driver(handle, 0);
            let _ = libusb1_sys::libusb_claim_interface(handle, 0);
            libusb1_sys::libusb_detach_kernel_driver(handle, 1);
            if libusb1_sys::libusb_claim_interface(handle, 1) != 0 {
                libusb1_sys::libusb_close(handle);
                return Err("claim interface 1 failed".into());
            }

            let device = libusb1_sys::libusb_get_device(handle);
            if device.is_null() {
                libusb1_sys::libusb_close(handle);
                return Err("获取设备描述失败：设备可能已断开".into());
            }

            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            let ret_desc = libusb1_sys::libusb_get_device_descriptor(device, &mut desc);
            if ret_desc != 0 {
                libusb1_sys::libusb_close(handle);
                return Err(format!(
                    "获取设备描述失败 (error {}): libusb 驱动异常",
                    ret_desc
                ));
            }

            trace!("[USB] scanning endpoints...");
            let mut config_ptr: *const libusb1_sys::libusb_config_descriptor = std::ptr::null();
            let mut ep_out_addr: u8 = DEFAULT_EP_OUT;
            let mut ep_in_addr: u8 = DEFAULT_EP_IN;
            let mut ep_out_max_pkt: u16 = DEFAULT_MAX_PACKET;
            let mut ep_in_max_pkt: u16 = DEFAULT_MAX_PACKET;
            let ret = libusb1_sys::libusb_get_active_config_descriptor(device, &mut config_ptr);
            if ret != 0 || config_ptr.is_null() {
                info!(
                    "[USB] WARNING: get_active_config_descriptor failed (ret={}), using defaults",
                    ret
                );
            } else {
                let config = &*config_ptr;
                for i in 0..config.bNumInterfaces as isize {
                    let iface = &*config.interface.wrapping_add(i as usize);
                    for j in 0..iface.num_altsetting {
                        let alt = &*iface.altsetting.wrapping_add(j as usize);
                        for k in 0..alt.bNumEndpoints as isize {
                            let ep = &*alt.endpoint.wrapping_add(k as usize);
                            let addr = ep.bEndpointAddress;
                            let dir = if addr & 0x80 != 0 { "IN" } else { "OUT" };
                            let ep_type = match ep.bmAttributes & 0x03 {
                                2 => "Bulk",
                                _ => continue,
                            };
                            if dir == "OUT" && ep_type == "Bulk" {
                                ep_out_addr = addr;
                                ep_out_max_pkt = ep.wMaxPacketSize;
                            }
                            if dir == "IN" && ep_type == "Bulk" {
                                ep_in_addr = addr;
                                ep_in_max_pkt = ep.wMaxPacketSize;
                            }
                        }
                    }
                }
                libusb1_sys::libusb_free_config_descriptor(config_ptr);
            }

            let stage = UsbStage::from_pid(desc.idProduct);
            trace!(
                "[USB] open_by_vid_pid OK: VID={:04X} PID={:04X} stage={:?}",
                vid, pid, stage
            );

            Ok(UsbDevice {
                handle,
                vid: desc.idVendor,
                pid: desc.idProduct,
                stage,
                device_type: DeviceType::from_vid_pid(vid, desc.idProduct),
                ep_out: ep_out_addr,
                ep_in: ep_in_addr,
                ep_out_max_packet_size: ep_out_max_pkt,
                ep_in_max_packet_size: ep_in_max_pkt,
                timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            })
        }
    }

    /// 是否是 libusb（WinUSB）后端
    pub fn is_libusb(&self) -> bool {
        true
    }

    /// 获取 EP_OUT 的最大包大小
    #[allow(dead_code)]
    pub fn ep_out_max_packet_size(&self) -> u16 {
        self.ep_out_max_packet_size
    }

    pub fn set_timeout(&mut self, duration: Duration) {
        self.timeout = duration;
    }

    pub fn get_timeout(&self) -> Duration {
        self.timeout
    }

    pub fn close(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                libusb1_sys::libusb_release_interface(self.handle, 1);
                libusb1_sys::libusb_release_interface(self.handle, 0);
                libusb1_sys::libusb_close(self.handle);
                self.handle = std::ptr::null_mut();
            }
        }
    }

    /// 重新打开 USB 设备：关闭旧句柄，等待设备稳定，重新打开并 claim interface
    pub fn reopen(&mut self, context: &UsbContext) -> Result<(), String> {
        self.close();
        // 等待设备稳定
        std::thread::sleep(Duration::from_millis(REOPEN_DELAY_MS));

        let mut new_device = UsbDevice::new(context)?;

        // 交换字段
        self.handle = new_device.handle;
        self.ep_in = new_device.ep_in;
        self.ep_out = new_device.ep_out;
        self.ep_in_max_packet_size = new_device.ep_in_max_packet_size;
        self.vid = new_device.vid;
        self.pid = new_device.pid;
        self.stage = new_device.stage;
        self.timeout = new_device.timeout;
        self.device_type = new_device.device_type;
        // 防止新设备 drop 时关闭已转移的句柄
        new_device.handle = std::ptr::null_mut();
        info!("USB 重连成功");
        Ok(())
    }
}

impl Drop for UsbDevice {
    fn drop(&mut self) {
        self.close();
    }
}

//! USB 设备构造与生命周期（open / close / reopen）
//!
//! - `USB设备::新建`        — 按 SUPPORTED_DEVICES 顺序查找第一个可用设备
//! - `USB设备::按VID_PID打开` — 按指定 VID/PID 打开
//! - `USB设备::关闭`      — 释放接口 + 关闭句柄
//! - `USB设备::重新打开`     — 重新打开设备（用于 BROM↔DA 切换后恢复）
//!
//! 底层使用 nusb 纯 Rust USB 库，Windows 上通过 WinUSB 后端通信。

use super::context::USB上下文;
use super::context::USB阶段;
use crate::system::config::{DeviceType, SUPPORTED_DEVICES};
use log::{info, trace, warn};
use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use std::time::Duration;

/// USB bulk 端点默认值（找不到时回退）
const 默认输出端点: u8 = 0x01;
const 默认输入端点: u8 = 0x81;
const 默认最大包大小: u16 = 512;
const 默认超时毫秒: u64 = 5000;
const 重新打开延迟毫秒: u64 = 200;

pub struct USB设备 {
    /// nusb 设备接口（持有 claim 的 interface）
    interface: Option<nusb::Interface>,
    /// CDC/WinUSB 控制传输接口
    控制interface: Option<nusb::Interface>,
    /// nusb 设备句柄（用于 reopen 时 drop + reopen）
    device: Option<nusb::Device>,
    pub vid: u16,
    pub pid: u16,
    pub 阶段: USB阶段,
    pub(crate) 设备类型: DeviceType,
    pub 输出端点: u8,
    pub 输入端点: u8,
    pub(crate) 接口编号: u8,
    pub(crate) 控制接口编号: u8,
    #[allow(dead_code)]
    输出端点最大包大小: u16,
    pub 输入端点最大包大小: u16,
    pub(crate) 超时: Duration,
    /// 是否已关闭（防止 drop 后重复关闭）
    已关闭: bool,
}

impl USB设备 {
    /// 查找并打开第一个支持的设备
    pub fn 新建(_context: &USB上下文) -> Result<Self, String> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| format!("枚举 USB 设备失败: {}", e))?;

        let mut 找到的设备类型 = DeviceType::Unknown;
        let mut 目标设备信息: Option<nusb::DeviceInfo> = None;

        for dev in devices {
            for dev_config in SUPPORTED_DEVICES {
                if dev.vendor_id() == dev_config.vid && dev.product_id() == dev_config.pid {
                    info!(
                        "[USB] 已连接:{} - {} (VID:0x{:04X} PID:0x{:04X})",
                        dev_config.name, dev_config.description, dev_config.vid, dev_config.pid
                    );
                    找到的设备类型 = dev_config.device_type;
                    目标设备信息 = Some(dev);
                    break;
                }
            }
            if 目标设备信息.is_some() {
                break;
            }
        }

        let 设备信息 = 目标设备信息.ok_or("未找到支持的设备")?;

        // 从 DeviceInfo 获取 vid/pid（nusb::Device 上没有这些方法）
        let vid = 设备信息.vendor_id();
        let pid = 设备信息.product_id();

        // 打开设备并 claim interface
        let device = 设备信息
            .open()
            .wait()
            .map_err(|e| format!("打开设备失败: {}", e))?;

        // 扫描端点（从 active_configuration 获取）
        let (输出端点地址, 输入端点地址, 输出端点最大包, 输入端点最大包, bulk接口编号) =
            Self::扫描端点_from_device(&device);

        let (interface, 接口编号, 控制interface, 控制接口编号) =
            Self::claim_bulk_and_control_interfaces(&device, bulk接口编号)?;

        info!(
            "[USB] EP_OUT=0x{:02X} wMaxPacketSize={} EP_IN=0x{:02X}",
            输出端点地址, 输出端点最大包, 输入端点地址
        );

        let 阶段 = USB阶段::从PID生成(pid);

        Ok(USB设备 {
            interface: Some(interface),
            控制interface: Some(控制interface),
            device: Some(device),
            vid,
            pid,
            阶段,
            设备类型: 找到的设备类型,
            输出端点: 输出端点地址,
            输入端点: 输入端点地址,
            接口编号,
            控制接口编号,
            输出端点最大包大小: 输出端点最大包,
            输入端点最大包大小: 输入端点最大包,
            超时: Duration::from_millis(默认超时毫秒),
            已关闭: false,
        })
    }

    /// 按指定 VID/PID 打开设备
    pub fn 按VID_PID打开(_context: &USB上下文, vid: u16, pid: u16) -> Result<Self, String> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| format!("枚举 USB 设备失败: {}", e))?;

        let 设备信息 = devices
            .into_iter()
            .find(|d| d.vendor_id() == vid && d.product_id() == pid)
            .ok_or_else(|| format!("未找到设备 VID={:04X} PID={:04X}", vid, pid))?;

        let device = 设备信息
            .open()
            .wait()
            .map_err(|e| format!("打开设备失败: {}", e))?;

        // 扫描端点
        let (输出端点地址, 输入端点地址, 输出端点最大包, 输入端点最大包, bulk接口编号) =
            Self::扫描端点_from_device(&device);

        let (interface, 接口编号, 控制interface, 控制接口编号) =
            Self::claim_bulk_and_control_interfaces(&device, bulk接口编号)?;

        let 阶段 = USB阶段::从PID生成(pid);
        trace!(
            "[USB] 按VID_PID打开 OK: VID={:04X} PID={:04X} stage={:?}",
            vid, pid, 阶段
        );

        Ok(USB设备 {
            interface: Some(interface),
            控制interface: Some(控制interface),
            device: Some(device),
            vid,
            pid,
            阶段,
            设备类型: DeviceType::from_vid_pid(vid, pid),
            输出端点: 输出端点地址,
            输入端点: 输入端点地址,
            接口编号,
            控制接口编号,
            输出端点最大包大小: 输出端点最大包,
            输入端点最大包大小: 输入端点最大包,
            超时: Duration::from_millis(默认超时毫秒),
            已关闭: false,
        })
    }

    /// 从 Device 的 active_configuration 扫描 bulk 端点
    fn 扫描端点_from_device(device: &nusb::Device) -> (u8, u8, u16, u16, u8) {
        let mut 输出端点地址: u8 = 默认输出端点;
        let mut 输入端点地址: u8 = 默认输入端点;
        let mut 输出端点最大包: u16 = 默认最大包大小;
        let mut 输入端点最大包: u16 = 默认最大包大小;
        let mut bulk接口编号: u8 = 1;

        trace!("[USB] scanning endpoints from active_configuration...");

        if let Ok(config) = device.active_configuration() {
            for iface_desc in config.interfaces() {
                let alt_setting = iface_desc.first_alt_setting();
                trace!(
                    "[USB] interface {} num_endpoints={}",
                    alt_setting.interface_number(),
                    alt_setting.endpoints().count()
                );
                let 接口编号 = alt_setting.interface_number();
                for ep in alt_setting.endpoints() {
                    let 地址 = ep.address();
                    let 方向 = if 地址 & 0x80 != 0 { "IN" } else { "OUT" };
                    let 端点类型 = match ep.transfer_type() {
                        TransferType::Bulk => "Bulk",
                        TransferType::Control => "Control",
                        TransferType::Interrupt => "Interrupt",
                        TransferType::Isochronous => "Isochronous",
                    };
                    let 最大包 = ep.max_packet_size();
                    trace!(
                        "[USB]   EP: 0x{:02X} dir={} type={} size={}",
                        地址, 方向, 端点类型, 最大包
                    );
                    if 方向 == "OUT" && 端点类型 == "Bulk" {
                        输出端点地址 = 地址;
                        输出端点最大包 = 最大包 as u16;
                        bulk接口编号 = 接口编号;
                    }
                    if 方向 == "IN" && 端点类型 == "Bulk" {
                        输入端点地址 = 地址;
                        输入端点最大包 = 最大包 as u16;
                        bulk接口编号 = 接口编号;
                    }
                }
            }
        } else {
            info!("[USB] WARNING: 无法读取 active configuration，使用默认端点");
        }

        (
            输出端点地址,
            输入端点地址,
            输出端点最大包,
            输入端点最大包,
            bulk接口编号,
        )
    }

    fn claim_bulk_and_control_interfaces(
        device: &nusb::Device,
        bulk接口编号: u8,
    ) -> Result<(nusb::Interface, u8, nusb::Interface, u8), String> {
        let interface = device
            .claim_interface(bulk接口编号)
            .wait()
            .map_err(|e| format!("claim bulk interface {} 失败: {}", bulk接口编号, e))?;
        trace!("[USB] claim bulk interface {} 成功", bulk接口编号);

        if bulk接口编号 == 0 {
            return Ok((interface.clone(), bulk接口编号, interface, 0));
        }

        match device.claim_interface(0).wait() {
            Ok(控制interface) => {
                trace!("[USB] claim control interface 0 成功");
                Ok((interface, bulk接口编号, 控制interface, 0))
            }
            Err(e) => {
                trace!(
                    "[USB] claim control interface 0 失败 ({})，复用 bulk interface {}",
                    e, bulk接口编号
                );
                Ok((interface.clone(), bulk接口编号, interface, bulk接口编号))
            }
        }
    }

    /// 是否是 nusb（WinUSB）后端
    pub fn 是libusb(&self) -> bool {
        true
    }

    /// 获取 EP_OUT 的最大包大小
    #[allow(dead_code)]
    pub fn 获取输出端点最大包大小(&self) -> u16 {
        self.输出端点最大包大小
    }

    /// 获取 EP_IN 的最大包大小
    pub fn 获取输入端点最大包大小(&self) -> u16 {
        self.输入端点最大包大小
    }

    pub fn 设置超时(&mut self, duration: Duration) {
        self.超时 = duration;
    }

    pub fn 获取超时(&self) -> Duration {
        self.超时
    }

    pub fn 关闭(&mut self) {
        if self.已关闭 {
            return;
        }
        // nusb: drop interface 会自动 release，drop device 会自动 close
        self.interface = None;
        self.控制interface = None;
        self.device = None;
        self.已关闭 = true;
        trace!("[USB] 设备已关闭");
    }

    /// USB 总线复位（对齐 Python device.reset()）
    /// 注意：nusb 目前没有直接的 reset_device API，
    /// 这里通过关闭并重新打开来模拟
    pub fn reset_device(&mut self) -> Result<(), String> {
        // nusb 不提供 libusb_reset_device 等价操作
        // USB 复位通常由 DA reinit 中的 set_usb_speed + close + reopen 处理
        warn!("[USB] nusb 不支持 USB 总线复位，跳过（由 reinit 流程处理）");
        Ok(())
    }

    /// 重新打开 USB 设备：关闭旧句柄，等待设备稳定，重新打开并 claim interface
    pub fn 重新打开(&mut self, context: &USB上下文) -> Result<(), String> {
        self.关闭();
        // 等待设备稳定
        std::thread::sleep(Duration::from_millis(重新打开延迟毫秒));

        let mut 新设备 = USB设备::新建(context)?;

        // 交换字段
        self.interface = 新设备.interface.take();
        self.控制interface = 新设备.控制interface.take();
        self.device = 新设备.device.take();
        self.输入端点 = 新设备.输入端点;
        self.输出端点 = 新设备.输出端点;
        self.输入端点最大包大小 = 新设备.输入端点最大包大小;
        self.接口编号 = 新设备.接口编号;
        self.控制接口编号 = 新设备.控制接口编号;
        self.vid = 新设备.vid;
        self.pid = 新设备.pid;
        self.阶段 = 新设备.阶段;
        self.超时 = 新设备.超时;
        self.设备类型 = 新设备.设备类型;
        self.已关闭 = false;
        info!("USB 重连成功");
        Ok(())
    }

    /// 获取 nusb Interface 引用（用于 device_io.rs 的 bulk transfer）
    pub(crate) fn 获取interface(&self) -> Option<&nusb::Interface> {
        self.interface.as_ref()
    }

    /// 获取 nusb Interface 可变引用
    pub(crate) fn 获取interface_mut(&mut self) -> Option<&mut nusb::Interface> {
        self.interface.as_mut()
    }

    /// 获取控制传输 Interface 引用
    pub(crate) fn 获取控制interface(&self) -> Option<&nusb::Interface> {
        self.控制interface.as_ref()
    }
}

impl Drop for USB设备 {
    fn drop(&mut self) {
        self.关闭();
    }
}

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
use log::{debug, info, trace, warn};
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
    /// 缓存的 bulk IN 端点句柄
    输入端点句柄: Option<nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::In>>,
    /// 缓存的 bulk OUT 端点句柄
    输出端点句柄: Option<nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>>,
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
    pub(crate) 输入暂存: Vec<u8>,
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
        let (输入端点句柄, 输出端点句柄) =
            Self::open_bulk_endpoints(&interface, 输入端点地址, 输出端点地址)?;

        info!(
            "[USB] EP_OUT=0x{:02X} wMaxPacketSize={} EP_IN=0x{:02X}",
            输出端点地址, 输出端点最大包, 输入端点地址
        );

        let 阶段 = USB阶段::从PID生成(pid);

        Ok(USB设备 {
            interface: Some(interface),
            控制interface: Some(控制interface),
            输入端点句柄: Some(输入端点句柄),
            输出端点句柄: Some(输出端点句柄),
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
            输入暂存: Vec::new(),
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
        let (输入端点句柄, 输出端点句柄) =
            Self::open_bulk_endpoints(&interface, 输入端点地址, 输出端点地址)?;

        let 阶段 = USB阶段::从PID生成(pid);
        trace!(
            "[USB] 按VID_PID打开 OK: VID={:04X} PID={:04X} stage={:?}",
            vid, pid, 阶段
        );

        Ok(USB设备 {
            interface: Some(interface),
            控制interface: Some(控制interface),
            输入端点句柄: Some(输入端点句柄),
            输出端点句柄: Some(输出端点句柄),
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
            输入暂存: Vec::new(),
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

    fn open_bulk_endpoints(
        interface: &nusb::Interface,
        输入端点地址: u8,
        输出端点地址: u8,
    ) -> Result<
        (
            nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::In>,
            nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>,
        ),
        String,
    > {
        let 输入端点句柄 = interface
            .endpoint::<nusb::transfer::Bulk, nusb::transfer::In>(输入端点地址)
            .map_err(|e| format!("打开输入端点失败: {}", e))?;
        let 输出端点句柄 = interface
            .endpoint::<nusb::transfer::Bulk, nusb::transfer::Out>(输出端点地址)
            .map_err(|e| format!("打开输出端点失败: {}", e))?;
        Ok((输入端点句柄, 输出端点句柄))
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
        if self.已关闭 {
            return;
        }
        self.取消挂起传输();
        // nusb: drop interface 会自动 release，drop device 会自动 close
        self.输入端点句柄 = None;
        self.输出端点句柄 = None;
        self.interface = None;
        self.控制interface = None;
        self.device = None;
        self.已关闭 = true;
        trace!("[USB] 设备已关闭");
    }

    pub fn 取消挂起传输(&mut self) {
        if let Some(ep_in) = self.输入端点句柄.as_mut() {
            if ep_in.pending() > 0 {
                trace!("[USB] 取消 {} 个挂起 IN 传输", ep_in.pending());
                ep_in.cancel_all();
                while ep_in.pending() > 0 {
                    let _ = ep_in.wait_next_complete(Duration::from_millis(10));
                }
            }
        }
        if let Some(ep_out) = self.输出端点句柄.as_mut() {
            if ep_out.pending() > 0 {
                trace!("[USB] 取消 {} 个挂起 OUT 传输", ep_out.pending());
                ep_out.cancel_all();
                while ep_out.pending() > 0 {
                    let _ = ep_out.wait_next_complete(Duration::from_millis(10));
                }
            }
        }
    }

    /// 清空 IN pending data，防止读取操作残留数据干扰后续写入操作
    pub fn drain_pending(&mut self) {
        self.输入暂存.clear();
        if let Some(ep_in) = self.输入端点句柄.as_mut() {
            if ep_in.pending() > 0 {
                trace!(
                    "[USB] drain_pending: 取消 {} 个挂起 IN 传输",
                    ep_in.pending()
                );
                ep_in.cancel_all();
                while ep_in.pending() > 0 {
                    let _ = ep_in.wait_next_complete(Duration::from_millis(10));
                }
            }
        }
        // 用短超时读取一次，清除可能的残留 IN 数据
        let orig_timeout = self.超时;
        self.超时 = Duration::from_millis(50);
        let mut tmp = [0u8; 512];
        let _ = self.读取(&mut tmp);
        self.超时 = orig_timeout;
    }

    /// 循环排空 IN 管道残留数据（不 reset HALT，不破坏 USB data toggle 序列）
    /// 用于 DA 会话复用时：新进程打开 USB 后需要排空上一个进程留下的残留数据，
    /// 但不能 clear_halt（会重置 data toggle 导致后续读取错位）。
    pub fn drain_pipes(&mut self) {
        self.输入暂存.clear();

        // 先取消挂起的 IN transfer
        if let Some(ep_in) = self.输入端点句柄.as_mut() {
            if ep_in.pending() > 0 {
                trace!("[USB] drain_pipes: 取消 {} 个挂起 IN 传输", ep_in.pending());
                ep_in.cancel_all();
                while ep_in.pending() > 0 {
                    let _ = ep_in.wait_next_complete(Duration::from_millis(10));
                }
            }
        }

        // 循环排空 IN 管道残留数据（500ms 超时，循环直到无数据）
        let orig_timeout = self.超时;
        self.超时 = Duration::from_millis(500);
        let mut drain_count = 0;
        loop {
            let mut tmp = [0u8; 512];
            match self.读取(&mut tmp) {
                Ok(0) => {
                    break; // 超时无数据，排空完毕
                }
                Ok(n) => {
                    drain_count += 1;
                    trace!(
                        "[USB] drain_pipes: 排空 {} 字节残留数据 (第 {} 次)",
                        n, drain_count
                    );
                    continue;
                }
                Err(_) => break,
            }
        }
        self.超时 = orig_timeout;

        if drain_count > 0 {
            debug!("[USB] drain_pipes: 共排空 {} 次 IN 残留数据", drain_count);
        } else {
            trace!("[USB] drain_pipes: 无残留数据");
        }
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
        self.输入端点句柄 = 新设备.输入端点句柄.take();
        self.输出端点句柄 = 新设备.输出端点句柄.take();
        self.interface = 新设备.interface.take();
        self.控制interface = 新设备.控制interface.take();
        self.device = 新设备.device.take();
        self.输入端点 = 新设备.输入端点;
        self.输出端点 = 新设备.输出端点;
        self.输入端点最大包大小 = 新设备.输入端点最大包大小;
        self.接口编号 = 新设备.接口编号;
        self.控制接口编号 = 新设备.控制接口编号;
        self.输入暂存.clear();
        self.vid = 新设备.vid;
        self.pid = 新设备.pid;
        self.阶段 = 新设备.阶段;
        self.超时 = 新设备.超时;
        self.设备类型 = 新设备.设备类型;
        self.已关闭 = false;
        info!("USB 重连成功");
        Ok(())
    }


    /// 获取 nusb Interface 可变引用
    pub(crate) fn 获取interface_mut(&mut self) -> Option<&mut nusb::Interface> {
        self.interface.as_mut()
    }

    /// 获取缓存的 bulk IN 端点
    pub(crate) fn 获取输入端点_mut(
        &mut self,
    ) -> Option<&mut nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::In>> {
        self.输入端点句柄.as_mut()
    }

    /// 获取缓存的 bulk OUT 端点
    pub(crate) fn 获取输出端点_mut(
        &mut self,
    ) -> Option<&mut nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>> {
        self.输出端点句柄.as_mut()
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

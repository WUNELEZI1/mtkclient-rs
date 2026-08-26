//! USB 设备构造与生命周期（open / close / reopen）
//!
//! - `USB设备::新建`        — 按 SUPPORTED_DEVICES 顺序查找第一个可用设备
//! - `USB设备::按VID_PID打开` — 按指定 VID/PID 打开
//! - `USB设备::关闭`      — 释放接口 + 关闭句柄
//! - `USB设备::重新打开`     — 重新打开设备（用于 BROM↔DA 切换后恢复）
//!
//! 底层使用 nusb 纯 Rust USB 库，Windows 上通过 WinUSB 后端通信。

use super::context::UsbContext;
use super::context::UsbStage;
use crate::system::config::{DeviceType, SUPPORTED_DEVICES};
use log::{debug, info, trace, warn};
use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use std::time::Duration;

/// USB bulk 端点默认值（找不到时回退）
const DEFAULT_OUT_EP: u8 = 0x01;
const DEFAULT_IN_EP: u8 = 0x81;
const DEFAULT_MAX_PACKET_SIZE: u16 = 512;
// 默认 bulk 读/写超时：从 5000ms 收紧到 1500ms。
// DA/BROM 的 status 与数据响应通常在毫秒级到达；此超时仅作"单笔传输未达首字节"的上限。
// 收紧后可把"缓冲区污染导致读 miss"的空等代价从 5s 降到 1.5s（写超时已由 drain_pending 修复，
// miss 本应趋零，此处为兜底保险）。EMI/BOOT_TO 等确需长耗时的步骤会在调用处显式 set_timeout(5000)。
const DEFAULT_TIMEOUT_MS: u64 = 1500;
const REOPEN_DELAY_MS: u64 = 200;

pub struct UsbDevice {
    /// nusb 设备接口（持有 claim 的 interface）
    interface: Option<nusb::Interface>,
    /// CDC/WinUSB 控制传输接口
    control_interface: Option<nusb::Interface>,
    /// 缓存的 bulk IN 端点句柄
    in_ep_handle: Option<nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::In>>,
    /// 缓存的 bulk OUT 端点句柄
    out_ep_handle: Option<nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>>,
    /// nusb 设备句柄（用于 reopen 时 drop + reopen）
    device: Option<nusb::Device>,
    pub vid: u16,
    pub pid: u16,
    pub stage: UsbStage,
    pub(crate) device_type: DeviceType,
    pub out_ep: u8,
    pub in_ep: u8,
    pub(crate) iface_num: u8,
    pub(crate) control_iface_num: u8,
    #[allow(dead_code)]
    out_ep_max_packet: u16,
    pub in_ep_max_packet_size: u16,
    pub(crate) in_buf: Vec<u8>,
    pub(crate) timeout: Duration,
    /// 是否已关闭（防止 drop 后重复关闭）
    closed: bool,
}

impl UsbDevice {
    /// 查找并打开第一个支持的设备
    pub fn new(_context: &UsbContext) -> Result<Self, String> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| format!("枚举 USB 设备失败: {}", e))?;

        let mut found_dev_type = DeviceType::Unknown;
        let mut target_dev_info: Option<nusb::DeviceInfo> = None;

        for dev in devices {
            for dev_config in SUPPORTED_DEVICES {
                if dev.vendor_id() == dev_config.vid && dev.product_id() == dev_config.pid {
                    info!(
                        "[USB] 已连接:{} - {} (VID:0x{:04X} PID:0x{:04X})",
                        dev_config.name, dev_config.description, dev_config.vid, dev_config.pid
                    );
                    found_dev_type = dev_config.device_type;
                    target_dev_info = Some(dev);
                    break;
                }
            }
            if target_dev_info.is_some() {
                break;
            }
        }

        let dev_info = target_dev_info.ok_or("未找到支持的设备")?;

        // 从 DeviceInfo 获取 vid/pid（nusb::Device 上没有这些方法）
        let vid = dev_info.vendor_id();
        let pid = dev_info.product_id();

        // 打开设备并 claim interface
        let device = dev_info
            .open()
            .wait()
            .map_err(|e| format!("打开设备失败: {}", e))?;

        // 扫描端点（从 active_configuration 获取）
        let (out_ep_addr, in_ep_addr, out_ep_max_packet, in_ep_max_packet, bulk_iface_num) =
            Self::scan_endpoints_from_device(&device);

        let (interface, iface_num, control_interface, control_iface_num) =
            Self::claim_bulk_and_control_interfaces(&device, bulk_iface_num)?;
        let (in_ep_handle, out_ep_handle) =
            Self::open_bulk_endpoints(&interface, in_ep_addr, out_ep_addr)?;

        info!(
            "[USB] EP_OUT=0x{:02X} wMaxPacketSize={} EP_IN=0x{:02X}",
            out_ep_addr, out_ep_max_packet, in_ep_addr
        );

        let stage = UsbStage::from_pid(pid);

        Ok(UsbDevice {
            interface: Some(interface),
            control_interface: Some(control_interface),
            in_ep_handle: Some(in_ep_handle),
            out_ep_handle: Some(out_ep_handle),
            device: Some(device),
            vid,
            pid,
            stage,
            device_type: found_dev_type,
            out_ep: out_ep_addr,
            in_ep: in_ep_addr,
            iface_num,
            control_iface_num,
            out_ep_max_packet,
            in_ep_max_packet_size: in_ep_max_packet,
            in_buf: Vec::new(),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            closed: false,
        })
    }

    /// 按指定 VID/PID 打开设备
    pub fn open_by_vid_pid(_context: &UsbContext, vid: u16, pid: u16) -> Result<Self, String> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| format!("枚举 USB 设备失败: {}", e))?;

        let dev_info = devices
            .into_iter()
            .find(|d| d.vendor_id() == vid && d.product_id() == pid)
            .ok_or_else(|| format!("未找到设备 VID={:04X} PID={:04X}", vid, pid))?;

        let device = dev_info
            .open()
            .wait()
            .map_err(|e| format!("打开设备失败: {}", e))?;

        // 扫描端点
        let (out_ep_addr, in_ep_addr, out_ep_max_packet, in_ep_max_packet, bulk_iface_num) =
            Self::scan_endpoints_from_device(&device);

        let (interface, iface_num, control_interface, control_iface_num) =
            Self::claim_bulk_and_control_interfaces(&device, bulk_iface_num)?;
        let (in_ep_handle, out_ep_handle) =
            Self::open_bulk_endpoints(&interface, in_ep_addr, out_ep_addr)?;

        let stage = UsbStage::from_pid(pid);
        trace!(
            "[USB] 按VID_PID打开 OK: VID={:04X} PID={:04X} stage={:?}",
            vid, pid, stage
        );

        Ok(UsbDevice {
            interface: Some(interface),
            control_interface: Some(control_interface),
            in_ep_handle: Some(in_ep_handle),
            out_ep_handle: Some(out_ep_handle),
            device: Some(device),
            vid,
            pid,
            stage,
            device_type: DeviceType::from_vid_pid(vid, pid),
            out_ep: out_ep_addr,
            in_ep: in_ep_addr,
            iface_num,
            control_iface_num,
            out_ep_max_packet,
            in_ep_max_packet_size: in_ep_max_packet,
            in_buf: Vec::new(),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            closed: false,
        })
    }

    /// 从 Device 的 active_configuration 扫描 bulk 端点
    fn scan_endpoints_from_device(device: &nusb::Device) -> (u8, u8, u16, u16, u8) {
        let mut out_ep_addr: u8 = DEFAULT_OUT_EP;
        let mut in_ep_addr: u8 = DEFAULT_IN_EP;
        let mut out_ep_max_packet: u16 = DEFAULT_MAX_PACKET_SIZE;
        let mut in_ep_max_packet: u16 = DEFAULT_MAX_PACKET_SIZE;
        let mut bulk_iface_num: u8 = 1;

        trace!("[USB] scanning endpoints from active_configuration...");

        if let Ok(config) = device.active_configuration() {
            for iface_desc in config.interfaces() {
                let alt_setting = iface_desc.first_alt_setting();
                trace!(
                    "[USB] interface {} num_endpoints={}",
                    alt_setting.interface_number(),
                    alt_setting.endpoints().count()
                );
                let iface_num = alt_setting.interface_number();
                for ep in alt_setting.endpoints() {
                    let addr = ep.address();
                    let direction = if addr & 0x80 != 0 { "IN" } else { "OUT" };
                    let ep_type = match ep.transfer_type() {
                        TransferType::Bulk => "Bulk",
                        TransferType::Control => "Control",
                        TransferType::Interrupt => "Interrupt",
                        TransferType::Isochronous => "Isochronous",
                    };
                    let max_packet = ep.max_packet_size();
                    trace!(
                        "[USB]   EP: 0x{:02X} dir={} type={} size={}",
                        addr, direction, ep_type, max_packet
                    );
                    if direction == "OUT" && ep_type == "Bulk" {
                        out_ep_addr = addr;
                        out_ep_max_packet = max_packet as u16;
                        bulk_iface_num = iface_num;
                    }
                    if direction == "IN" && ep_type == "Bulk" {
                        in_ep_addr = addr;
                        in_ep_max_packet = max_packet as u16;
                        bulk_iface_num = iface_num;
                    }
                }
            }
        } else {
            info!("[USB] WARNING: 无法读取 active configuration，使用默认端点");
        }

        (
            out_ep_addr,
            in_ep_addr,
            out_ep_max_packet,
            in_ep_max_packet,
            bulk_iface_num,
        )
    }

    fn claim_bulk_and_control_interfaces(
        device: &nusb::Device,
        bulk_iface_num: u8,
    ) -> Result<(nusb::Interface, u8, nusb::Interface, u8), String> {
        let interface = device
            .claim_interface(bulk_iface_num)
            .wait()
            .map_err(|e| format!("claim bulk interface {} 失败: {}", bulk_iface_num, e))?;
        trace!("[USB] claim bulk interface {} 成功", bulk_iface_num);

        if bulk_iface_num == 0 {
            return Ok((interface.clone(), bulk_iface_num, interface, 0));
        }

        match device.claim_interface(0).wait() {
            Ok(control_interface) => {
                trace!("[USB] claim control interface 0 成功");
                Ok((interface, bulk_iface_num, control_interface, 0))
            }
            Err(e) => {
                trace!(
                    "[USB] claim control interface 0 失败 ({})，复用 bulk interface {}",
                    e, bulk_iface_num
                );
                Ok((interface.clone(), bulk_iface_num, interface, bulk_iface_num))
            }
        }
    }

    fn open_bulk_endpoints(
        interface: &nusb::Interface,
        in_ep_addr: u8,
        out_ep_addr: u8,
    ) -> Result<
        (
            nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::In>,
            nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>,
        ),
        String,
    > {
        let in_ep_handle = interface
            .endpoint::<nusb::transfer::Bulk, nusb::transfer::In>(in_ep_addr)
            .map_err(|e| format!("打开输入端点失败: {}", e))?;
        let out_ep_handle = interface
            .endpoint::<nusb::transfer::Bulk, nusb::transfer::Out>(out_ep_addr)
            .map_err(|e| format!("打开输出端点失败: {}", e))?;
        Ok((in_ep_handle, out_ep_handle))
    }

    /// 获取 EP_OUT 的最大包大小
    #[allow(dead_code)]
    pub fn out_ep_max_packet_size(&self) -> u16 {
        self.out_ep_max_packet
    }

    pub fn set_timeout(&mut self, duration: Duration) {
        self.timeout = duration;
    }

    pub fn get_timeout(&self) -> Duration {
        self.timeout
    }

    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.cancel_pending_transfers();
        // nusb: drop interface 会自动 release，drop device 会自动 close
        self.in_ep_handle = None;
        self.out_ep_handle = None;
        self.interface = None;
        self.control_interface = None;
        self.device = None;
        self.closed = true;
        trace!("[USB] 设备已关闭");
    }

    pub fn cancel_pending_transfers(&mut self) {
        if let Some(ep_in) = self.in_ep_handle.as_mut() {
            if ep_in.pending() > 0 {
                trace!("[USB] 取消 {} 个挂起 IN 传输", ep_in.pending());
                ep_in.cancel_all();
                while ep_in.pending() > 0 {
                    let _ = ep_in.wait_next_complete(Duration::from_millis(10));
                }
            }
        }
        if let Some(ep_out) = self.out_ep_handle.as_mut() {
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
        self.in_buf.clear();
        if let Some(ep_in) = self.in_ep_handle.as_mut() {
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
        let orig_timeout = self.timeout;
        self.timeout = Duration::from_millis(50);
        let mut tmp = [0u8; 512];
        let _ = self.read(&mut tmp);
        self.timeout = orig_timeout;
    }

    /// 循环排空 IN 管道残留数据（不 reset HALT，不破坏 USB data toggle 序列）
    /// 用于 DA 会话复用时：新进程打开 USB 后需要排空上一个进程留下的残留数据，
    /// 但不能 clear_halt（会重置 data toggle 导致后续读取错位）。
    pub fn drain_pipes(&mut self) {
        self.in_buf.clear();

        // 先取消挂起的 IN transfer
        if let Some(ep_in) = self.in_ep_handle.as_mut() {
            if ep_in.pending() > 0 {
                trace!("[USB] drain_pipes: 取消 {} 个挂起 IN 传输", ep_in.pending());
                ep_in.cancel_all();
                while ep_in.pending() > 0 {
                    let _ = ep_in.wait_next_complete(Duration::from_millis(10));
                }
            }
        }

        // 循环排空 IN 管道残留数据（500ms 超时，循环直到无数据）
        let orig_timeout = self.timeout;
        self.timeout = Duration::from_millis(500);
        let mut drain_count = 0;
        loop {
            let mut tmp = [0u8; 512];
            match self.read(&mut tmp) {
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
        self.timeout = orig_timeout;

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
    pub fn reopen(&mut self, context: &UsbContext) -> Result<(), String> {
        self.close();
        // 等待设备稳定
        std::thread::sleep(Duration::from_millis(REOPEN_DELAY_MS));

        let mut new_device = UsbDevice::new(context)?;

        // 交换字段
        self.in_ep_handle = new_device.in_ep_handle.take();
        self.out_ep_handle = new_device.out_ep_handle.take();
        self.interface = new_device.interface.take();
        self.control_interface = new_device.control_interface.take();
        self.device = new_device.device.take();
        self.in_ep = new_device.in_ep;
        self.out_ep = new_device.out_ep;
        self.in_ep_max_packet_size = new_device.in_ep_max_packet_size;
        self.iface_num = new_device.iface_num;
        self.control_iface_num = new_device.control_iface_num;
        self.in_buf.clear();
        self.vid = new_device.vid;
        self.pid = new_device.pid;
        self.stage = new_device.stage;
        self.timeout = new_device.timeout;
        self.device_type = new_device.device_type;
        self.closed = false;
        info!("USB 重连成功");
        Ok(())
    }

    /// 获取 nusb Interface 可变引用
    pub(crate) fn interface_mut(&mut self) -> Option<&mut nusb::Interface> {
        self.interface.as_mut()
    }

    /// 获取缓存的 bulk IN 端点
    pub(crate) fn in_endpoint_mut(
        &mut self,
    ) -> Option<&mut nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::In>> {
        self.in_ep_handle.as_mut()
    }

    /// 获取缓存的 bulk OUT 端点
    pub(crate) fn out_endpoint_mut(
        &mut self,
    ) -> Option<&mut nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>> {
        self.out_ep_handle.as_mut()
    }

    /// 获取控制传输 Interface 引用
    pub(crate) fn control_iface(&self) -> Option<&nusb::Interface> {
        self.control_interface.as_ref()
    }
}

impl Drop for UsbDevice {
    fn drop(&mut self) {
        self.close();
    }
}

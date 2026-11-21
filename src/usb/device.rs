//! USB device construction and lifecycle (open / close / reopen)
//!
//! - `UsbDevice::new`            — find the first available device in SUPPORTED_DEVICES order
//! - `UsbDevice::open_by_vid_pid` — open by the given VID/PID
//! - `UsbDevice::close`          — release interfaces + close handles
//! - `UsbDevice::reopen`         — reopen the device (to recover after a BROM↔DA switch)
//!
//! Uses the pure-Rust nusb library, communicating via the WinUSB backend on Windows.

use super::context::UsbContext;
use super::context::UsbStage;
use crate::error::UsbError;
use crate::system::config::{DeviceType, SUPPORTED_DEVICES};
use log::{debug, info, trace, warn};
use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use std::time::Duration;

/// Default USB bulk endpoints (used when discovery fails)
const DEFAULT_OUT_EP: u8 = 0x01;
const DEFAULT_IN_EP: u8 = 0x81;
const DEFAULT_MAX_PACKET_SIZE: u16 = 512;
// Default bulk read/write timeout: tightened from 5000ms to 1500ms.
// DA/BROM status and data responses usually arrive within milliseconds; this timeout only caps "no first byte for a transfer".
// Tightening cuts the idle cost of a read miss caused by buffer pollution from 5s to 1.5s (write timeouts were fixed by drain_pending,
// so misses should approach zero; this is a safety net). Steps that genuinely need longer, such as EMI/BOOT_TO, call set_timeout(5000) explicitly.
const DEFAULT_TIMEOUT_MS: u64 = 1500;
/// Reconnect poll interval (ms): after a DA reboot, USB re-enumeration usually takes 1~3s,
/// and a single fixed wait tends to fail before the device is ready. Poll and retry at this interval instead.
/// Follows penumbra `reenumerate` (200ms interval / 10s total timeout).
const REOPEN_POLL_INTERVAL_MS: u64 = 200;
/// Total reconnect timeout (ms): fail if the device is still not enumerated after this.
const REOPEN_TIMEOUT_MS: u64 = 10_000;

/// Whether reconnect polling should continue: retry while elapsed time is below the total timeout.
///
/// Extracted as a pure function for easy unit testing (I/O-free). Matches penumbra `reenumerate`'s
/// "200ms interval + 10s total timeout" semantics.
pub(crate) fn reopen_should_retry(elapsed_ms: u64) -> bool {
    elapsed_ms < REOPEN_TIMEOUT_MS
}

pub struct UsbDevice {
    /// nusb device interface (the claimed interface)
    interface: Option<nusb::Interface>,
    /// CDC/WinUSB control transfer interface
    control_interface: Option<nusb::Interface>,
    /// Cached bulk IN endpoint handle
    in_ep_handle: Option<nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::In>>,
    /// Cached bulk OUT endpoint handle
    out_ep_handle: Option<nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>>,
    /// nusb device handle (dropped and reopened on reopen)
    device: Option<nusb::Device>,
    pub vid: u16,
    pub pid: u16,
    pub stage: UsbStage,
    pub(crate) device_type: DeviceType,
    pub out_ep: u8,
    pub in_ep: u8,
    pub(crate) iface_num: u8,
    pub(crate) control_iface_num: u8,
    out_ep_max_packet: u16,
    pub in_ep_max_packet_size: u16,
    pub(crate) in_buf: Vec<u8>,
    pub(crate) timeout: Duration,
    /// Whether already closed (prevents double close after drop)
    closed: bool,
}

impl UsbDevice {
    /// Find and open the first supported device
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

        let dev_info = target_dev_info.ok_or_else(|| UsbError::NotFound.to_string())?;

        // Get vid/pid from DeviceInfo (nusb::Device does not expose these)
        let vid = dev_info.vendor_id();
        let pid = dev_info.product_id();

        // Open the device and claim its interface
        let device = dev_info
            .open()
            .wait()
            .map_err(|e| UsbError::OpenFailed(e.to_string()).to_string())?;

        // Scan endpoints (from the active_configuration)
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

    /// Open the device with the given VID/PID
    pub fn open_by_vid_pid(_context: &UsbContext, vid: u16, pid: u16) -> Result<Self, String> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| format!("枚举 USB 设备失败: {}", e))?;

        let dev_info = devices
            .into_iter()
            .find(|d| d.vendor_id() == vid && d.product_id() == pid)
            .ok_or_else(|| format!("{}（VID={:04X} PID={:04X}）", UsbError::NotFound, vid, pid))?;

        let device = dev_info
            .open()
            .wait()
            .map_err(|e| UsbError::OpenFailed(e.to_string()).to_string())?;

        // Scan endpoints
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

    /// Scan bulk endpoints from the Device's active_configuration
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
        let interface = device.claim_interface(bulk_iface_num).wait().map_err(|e| {
            format!(
                "{}（claim bulk interface {}）: {}",
                UsbError::InterfaceNotFound,
                bulk_iface_num,
                e
            )
        })?;
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
            .map_err(|e| {
                format!(
                    "{}（IN 端点 0x{:02X}）: {}",
                    UsbError::InterfaceNotFound,
                    in_ep_addr,
                    e
                )
            })?;
        let out_ep_handle = interface
            .endpoint::<nusb::transfer::Bulk, nusb::transfer::Out>(out_ep_addr)
            .map_err(|e| {
                format!(
                    "{}（OUT 端点 0x{:02X}）: {}",
                    UsbError::InterfaceNotFound,
                    out_ep_addr,
                    e
                )
            })?;
        Ok((in_ep_handle, out_ep_handle))
    }

    /// Get the max packet size of EP_OUT
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
        // nusb: dropping the interface releases it; dropping the device closes it
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

    /// Clear pending IN data so leftovers from reads do not disturb later writes
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
        // Read once with a short timeout to clear possible leftover IN data
        let orig_timeout = self.timeout;
        self.timeout = Duration::from_millis(50);
        let mut tmp = [0u8; 512];
        let _ = self.read(&mut tmp);
        self.timeout = orig_timeout;
    }

    /// Drain leftover IN pipe data in a loop (without resetting HALT, preserving the USB data toggle sequence)
    /// For DA session reuse: a new process must drain leftovers from the previous process after opening USB,
    /// but must not clear_halt (it resets the data toggle and misaligns later reads).
    pub fn drain_pipes(&mut self) {
        self.in_buf.clear();

        // First cancel pending IN transfers
        if let Some(ep_in) = self.in_ep_handle.as_mut() {
            if ep_in.pending() > 0 {
                trace!("[USB] drain_pipes: 取消 {} 个挂起 IN 传输", ep_in.pending());
                ep_in.cancel_all();
                while ep_in.pending() > 0 {
                    let _ = ep_in.wait_next_complete(Duration::from_millis(10));
                }
            }
        }

        // Drain leftover IN pipe data in a loop (500ms timeout, until no data remains)
        let orig_timeout = self.timeout;
        self.timeout = Duration::from_millis(500);
        let mut drain_count = 0;
        loop {
            let mut tmp = [0u8; 512];
            match self.read(&mut tmp) {
                Ok(0) => {
                    break; // timeout with no data: drained
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

    /// USB bus reset (matching Python device.reset())
    /// Note: nusb has no direct reset_device API,
    /// so this is emulated by closing and reopening
    pub fn reset_device(&mut self) -> Result<(), String> {
        // nusb has no libusb_reset_device equivalent
        // USB reset is usually handled by set_usb_speed + close + reopen in DA reinit
        warn!("[USB] nusb 不支持 USB 总线复位，跳过（由 reinit 流程处理）");
        Ok(())
    }

    /// Reopen the USB device: close old handles, then poll for re-enumeration every 200ms (10s total timeout),
    /// reopening and claiming the interface.
    ///
    /// After a DA reboot, USB re-enumeration usually takes 1~3s; the old implementation waited 200ms once and then called
    /// `new()`, which inevitably failed while the device was not ready. Now it polls every `REOPEN_POLL_INTERVAL_MS`
    /// until it succeeds or the elapsed time exceeds `REOPEN_TIMEOUT_MS`.
    /// Follows penumbra `reenumerate()`.
    pub fn reopen(&mut self, context: &UsbContext) -> Result<(), String> {
        self.close();

        let start = std::time::Instant::now();
        // Assigned by the loop's single break branch, hence no initializer (avoids unused_assignments)
        let last_err: String;

        loop {
            // Wait for the device to settle / re-enumerate
            std::thread::sleep(Duration::from_millis(REOPEN_POLL_INTERVAL_MS));

            match UsbDevice::new(context) {
                Ok(mut new_device) => {
                    // Swap fields
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
                    info!("USB 重连成功（耗时 {}ms）", start.elapsed().as_millis());
                    return Ok(());
                }
                Err(e) => {
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    if !reopen_should_retry(elapsed_ms) {
                        last_err = e;
                        break;
                    }
                    debug!(
                        "[USB] 设备尚未重新枚举（已等待 {}ms），继续轮询: {}",
                        elapsed_ms, e
                    );
                }
            }
        }

        Err(format!(
            "USB 重连失败（{}ms 内未重新枚举到设备）: {}",
            REOPEN_TIMEOUT_MS, last_err
        ))
    }

    /// Get a mutable reference to the nusb Interface
    pub(crate) fn interface_mut(&mut self) -> Option<&mut nusb::Interface> {
        self.interface.as_mut()
    }

    /// Get the cached bulk IN endpoint
    pub(crate) fn in_endpoint_mut(
        &mut self,
    ) -> Option<&mut nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::In>> {
        self.in_ep_handle.as_mut()
    }

    /// Get the cached bulk OUT endpoint
    pub(crate) fn out_endpoint_mut(
        &mut self,
    ) -> Option<&mut nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>> {
        self.out_ep_handle.as_mut()
    }

    /// Get a reference to the control transfer Interface
    pub(crate) fn control_iface(&self) -> Option<&nusb::Interface> {
        self.control_interface.as_ref()
    }
}

impl Drop for UsbDevice {
    fn drop(&mut self) {
        self.close();
    }
}

// =============================================================================
// Unit tests — reopen polling policy (P1-1)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopen_retries_before_total_timeout() {
        assert!(reopen_should_retry(0));
        assert!(reopen_should_retry(REOPEN_POLL_INTERVAL_MS));
        assert!(reopen_should_retry(REOPEN_TIMEOUT_MS - 1));
    }

    #[test]
    fn reopen_stops_at_total_timeout() {
        assert!(!reopen_should_retry(REOPEN_TIMEOUT_MS));
        assert!(!reopen_should_retry(REOPEN_TIMEOUT_MS + 1));
    }

    #[test]
    fn reopen_constants_match_penumbra_policy() {
        // 200ms polling, 10s total timeout (matching penumbra reenumerate)
        assert_eq!(REOPEN_POLL_INTERVAL_MS, 200);
        assert_eq!(REOPEN_TIMEOUT_MS, 10_000);
    }

    // Compile-time check: the total timeout must allow at least 10 polls, otherwise polling degenerates to "wait once"
    const _: () = assert!(REOPEN_TIMEOUT_MS / REOPEN_POLL_INTERVAL_MS >= 10);
}

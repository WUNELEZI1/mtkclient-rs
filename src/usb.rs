//! libusb0 (libusb-win32) USB 通信层

use crate::libusb0;
use crate::config::{DeviceType, SUPPORTED_DEVICES};
use log::{debug, info};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::raw::{c_int, c_uint};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 全局静默标志
static QUIET_USB_READ: AtomicBool = AtomicBool::new(false);

/// 全局 USB trace 开关
static USB_LOG_ENABLED: AtomicBool = AtomicBool::new(false);

/// USB trace 日志文件
static USB_LOG_FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);

pub fn set_quiet_usb_read(quiet: bool) {
    QUIET_USB_READ.store(quiet, Ordering::Relaxed);
}

pub fn set_usb_log_enabled(enabled: bool) {
    USB_LOG_ENABLED.store(enabled, Ordering::Relaxed);
    if enabled {
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open("usb_debug.log")
        {
            Ok(file) => {
                let mut guard = USB_LOG_FILE.lock().unwrap();
                *guard = Some(file);
            }
            Err(e) => {
                eprintln!("[ERROR] 无法打开 usb_debug.log: {}", e);
            }
        }
    } else {
        let mut guard = USB_LOG_FILE.lock().unwrap();
        *guard = None;
    }
}

pub fn usb_trace(direction: &str, func_info: &str, data: &[u8]) {
    if !USB_LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    let total_secs = secs % 86400;
    let hours = (total_secs / 3600) as u32;
    let minutes = ((total_secs % 3600) / 60) as u32;
    let seconds = (total_secs % 60) as u32;

    let hex_str: String = data
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(" ");

    let log_line = format!(
        "[{:02}:{:02}:{:02}.{:03}] [{}] [{}] {}\n",
        hours, minutes, seconds, millis, direction, func_info, hex_str
    );

    if let Ok(mut guard) = USB_LOG_FILE.lock()
        && let Some(ref mut file) = *guard
    {
        let _ = file.write_all(log_line.as_bytes());
        let _ = file.flush();
    }
}

#[macro_export]
macro_rules! usb_trace_tx {
    ($data:expr) => {
        $crate::usb::usb_trace("TX", &format!("{}:{}", file!(), line!()), $data);
    };
}

#[macro_export]
macro_rules! usb_trace_rx {
    ($data:expr) => {
        $crate::usb::usb_trace("RX", &format!("{}:{}", file!(), line!()), $data);
    };
}

#[derive(Debug, PartialEq, Clone)]
pub enum UsbStage {
    Brom,
    Preloader,
    Unknown,
}

impl UsbStage {
    pub fn from_pid(pid: u16) -> Self {
        match pid {
            0x0003 => UsbStage::Brom,
            0x2000 => UsbStage::Preloader,
            _ => UsbStage::Unknown,
        }
    }
}

/// libusb0 上下文（全局初始化）
pub struct UsbContext {
    initialized: bool,
}

impl UsbContext {
    pub fn new() -> Result<Self, String> {
        unsafe {
            libusb0::usb_init();
            
            // 必须先调用 find_busses 和 find_devices，否则 get_busses 返回 null
            let bus_count = libusb0::usb_find_busses();
            let dev_count = libusb0::usb_find_devices();
            
            debug!("[USB] libusb0 initialized, found {} busses, {} devices", bus_count, dev_count);
        }
        Ok(UsbContext { initialized: true })
    }
}

impl Drop for UsbContext {
    fn drop(&mut self) {
        // libusb0 没有显式的 cleanup 函数
        debug!("[USB] UsbContext dropped");
    }
}

pub struct UsbDevice {
    handle: *mut libusb0::usb_dev_handle,
    pub vid: u16,
    pub pid: u16,
    pub stage: UsbStage,
    device_type: DeviceType,
    pub ep_out: u8,
    pub ep_in: u8,
    timeout: Duration,
}

impl UsbDevice {
    pub fn open_by_vid_pid(_context: &UsbContext, vid: u16, pid: u16) -> Result<Self, String> {
        Self::open_device(vid, pid)
    }

    fn open_device(vid: u16, pid: u16) -> Result<Self, String> {
        unsafe {
            // 重新扫描设备（不重新调用 usb_init，避免重置内部状态）
            let bus_count = libusb0::usb_find_busses();
            let dev_count = libusb0::usb_find_devices();
            debug!("[USB] rescanned: {} busses, {} devices", bus_count, dev_count);

            // 枚举所有设备
            let busses = libusb0::usb_get_busses();
            if busses.is_null() {
                return Err("usb_get_busses() returned null".into());
            }
            
            // 打印结构体布局信息
            debug!("[USB] usb_bus size: {} bytes", std::mem::size_of::<libusb0::usb_bus>());
            debug!("[USB] usb_device size: {} bytes", std::mem::size_of::<libusb0::usb_device>());
            debug!("[USB] usb_bus.devices offset: {}", {
                let dummy: libusb0::usb_bus = std::mem::zeroed();
                let base = &dummy as *const _ as usize;
                let field = std::ptr::addr_of!(dummy.devices) as usize;
                field - base
            });

            let mut current_bus = busses;
            let mut bus_idx = 0;
            while !current_bus.is_null() {
                let bus_bytes = std::slice::from_raw_parts(current_bus as *const u8, std::mem::size_of::<libusb0::usb_bus>());
                
                // 扫描整个 bus 结构体，找到所有非零区域
                debug!("[USB] bus[{}] total size={} bytes, scanning for non-zero regions:", bus_idx, bus_bytes.len());
                let mut i = 0;
                while i < bus_bytes.len() {
                    if bus_bytes[i] != 0 {
                        let start = i;
                        while i < bus_bytes.len() && bus_bytes[i] != 0 { i += 1; }
                        let hex: String = bus_bytes[start..i].iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
                        debug!("[USB]   non-zero at offset {}..{} (len={}): {}", start, i, i - start, hex);
                    }
                    i += 1;
                }
                
                let bus = &*current_bus;
                // 安全读取 packed 结构体字段
                let bus_devices = std::ptr::addr_of!(bus.devices).read_unaligned();
                let bus_location = std::ptr::addr_of!(bus.location).read_unaligned();
                let bus_root_dev = std::ptr::addr_of!(bus.root_dev).read_unaligned();
                let bus_next = std::ptr::addr_of!(bus.next).read_unaligned();
                
                debug!("[USB] bus[{}]: dirname={:?}, devices={:?}, location={}, root_dev={:?}", 
                    bus_idx, String::from_utf8_lossy(&bus.dirname[..64]).trim_end_matches('\0'),
                    bus_devices, bus_location, bus_root_dev);
                
                let mut current_dev = bus_devices;
                let mut dev_count = 0;
                while !current_dev.is_null() && dev_count < 64 {  // 最多64个设备
                    dev_count += 1;
                    
                    // 用 std::ptr::read_unaligned 代替直接解引用，避免对齐问题
                    let dev_vid = std::ptr::read_unaligned(std::ptr::addr_of!((*current_dev).descriptor.idVendor));
                    let dev_pid = std::ptr::read_unaligned(std::ptr::addr_of!((*current_dev).descriptor.idProduct));
                    
                    debug!("[USB]   device: VID=0x{:04X} PID=0x{:04X}", dev_vid, dev_pid);
                    
                    if dev_vid == vid && dev_pid == pid {
                        debug!("[USB] 找到目标设备，准备打开...");
                        let handle = libusb0::usb_open(current_dev as *mut _);
                        if handle.is_null() {
                            return Err(format!("usb_open failed for VID=0x{:04X} PID=0x{:04X}", vid, pid));
                        }
                        debug!("[USB] usb_open 成功，handle={:?}", handle);

                        let mut ep_out_addr: u8 = 0x01;
                        let mut ep_in_addr: u8 = 0x81;

                        // 安全读取 config 指针
                        debug!("[USB] 读取 config 指针...");
                        let dev_config = std::ptr::read_unaligned(std::ptr::addr_of!((*current_dev).config));
                        debug!("[USB] dev_config = {:?}", dev_config);
                        if !dev_config.is_null() {
                            debug!("[USB] 读取 config 结构体字段...");
                            // 使用 read_unaligned 读取 config 结构体字段
                            let b_num_interfaces = std::ptr::read_unaligned(std::ptr::addr_of!((*dev_config).bNumInterfaces));
                            let interface_ptr = std::ptr::read_unaligned(std::ptr::addr_of!((*dev_config).interface));
                            debug!("[USB] bNumInterfaces={}, interface={:?}", b_num_interfaces, interface_ptr);
                            
                            if !interface_ptr.is_null() && b_num_interfaces > 0 && b_num_interfaces < 16 {
                                for i in 0..b_num_interfaces as isize {
                                    // 安全读取 interface
                                    let iface_altsetting = std::ptr::read_unaligned(std::ptr::addr_of!((*interface_ptr.offset(i)).altsetting));
                                    let iface_num_alt = std::ptr::read_unaligned(std::ptr::addr_of!((*interface_ptr.offset(i)).num_altsetting));
                                    
                                    if !iface_altsetting.is_null() && iface_num_alt > 0 && iface_num_alt < 16 {
                                        for j in 0..iface_num_alt as isize {
                                            // 安全读取 altsetting
                                            let alt_num_endpoints = std::ptr::read_unaligned(std::ptr::addr_of!((*iface_altsetting.offset(j)).bNumEndpoints));
                                            let alt_endpoint_ptr = std::ptr::read_unaligned(std::ptr::addr_of!((*iface_altsetting.offset(j)).endpoint));
                                            
                                            if !alt_endpoint_ptr.is_null() && alt_num_endpoints > 0 && alt_num_endpoints < 16 {
                                                for k in 0..alt_num_endpoints as isize {
                                                    // 安全读取 endpoint
                                                    let addr = std::ptr::read_unaligned(std::ptr::addr_of!((*alt_endpoint_ptr.offset(k)).bEndpointAddress));
                                                    let attrs = std::ptr::read_unaligned(std::ptr::addr_of!((*alt_endpoint_ptr.offset(k)).bmAttributes));
                                                    
                                                    let is_in = (addr & 0x80) != 0;
                                                    let is_bulk = (attrs & 0x03) == 2;
                                                    if is_bulk && !is_in { ep_out_addr = addr; }
                                                    if is_bulk && is_in { ep_in_addr = addr; }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        debug!("[USB] VID=0x{:04X} PID=0x{:04X} detected, EP_OUT=0x{:02X}, EP_IN=0x{:02X}",
                            vid, pid, ep_out_addr, ep_in_addr);

                        // 设置配置：先尝试配置 1，失败则尝试配置 0
                        debug!("[USB] 调用 usb_set_configuration(handle, 1)...");
                        let ret = libusb0::usb_set_configuration(handle, 1);
                        if ret != 0 {
                            debug!("[USB] usb_set_configuration(1) 失败: {}, 尝试 config 0", ret);
                            let ret2 = libusb0::usb_set_configuration(handle, 0);
                            debug!("[USB] usb_set_configuration(0) 返回: {}", ret2);
                        } else {
                            debug!("[USB] usb_set_configuration(1) 成功");
                        }

                        // 声明接口
                        debug!("[USB] 调用 usb_claim_interface(handle, 0)...");
                        let ret = libusb0::usb_claim_interface(handle, 0);
                        if ret != 0 {
                            debug!("[USB] usb_claim_interface(0) failed: {}, 继续尝试（某些设备可跳过）", ret);
                            // 不返回错误，继续尝试（某些 filter 驱动下可跳过 claim）
                        } else {
                            debug!("[USB] usb_claim_interface(0) 成功");
                        }

                        // 如果有多个接口，也声明第二个
                        if !dev_config.is_null() {
                            let num_ifaces = std::ptr::read_unaligned(std::ptr::addr_of!((*dev_config).bNumInterfaces));
                            if num_ifaces > 1 {
                                debug!("[USB] 调用 usb_claim_interface(handle, 1)...");
                                let ret = libusb0::usb_claim_interface(handle, 1);
                                if ret != 0 {
                                    debug!("[USB] usb_claim_interface(1) failed: {}, 继续尝试", ret);
                                } else {
                                    debug!("[USB] usb_claim_interface(1) 成功");
                                }
                            }
                        }

                        return Ok(UsbDevice {
                            handle,
                            vid,
                            pid,
                            stage: UsbStage::from_pid(pid),
                            device_type: DeviceType::Unknown,
                            ep_out: ep_out_addr,
                            ep_in: ep_in_addr,
                            timeout: Duration::from_millis(1000),
                        });
                    }
                    
                    // 保存 next 指针再移动
                    let next_dev = std::ptr::read_unaligned(std::ptr::addr_of!((*current_dev).next));
                    if next_dev == current_dev {  // 检测自引用
                        debug!("[USB] 检测到自引用，停止遍历");
                        break;
                    }
                    current_dev = next_dev;
                }
                
                current_bus = bus_next;
                bus_idx += 1;
            }

            Err(format!("未找到设备 VID=0x{:04X} PID=0x{:04X}", vid, pid))
        }
    }

    pub fn new(context: &UsbContext) -> Result<Self, String> {
        for dev_config in SUPPORTED_DEVICES {
            if let Ok(device) = Self::open_by_vid_pid(context, dev_config.vid, dev_config.pid) {
                info!(
                    "[USB] 已连接: {} - {} (VID:0x{:04X} PID:0x{:04X})",
                    dev_config.name, dev_config.description, dev_config.vid, dev_config.pid
                );
                return Ok(device);
            }
        }
        Err("未找到支持的设备".into())
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        if data.is_empty() {
            return Ok(0);
        }

        usb_trace("TX", "UsbDevice::write", data);

        unsafe {
            let ret = libusb0::usb_bulk_write(
                self.handle,
                self.ep_out as c_int,
                data.as_ptr(),
                data.len() as c_int,
                self.timeout.as_millis() as c_int,
            );
            
            if ret < 0 {
                return Err(format!("write err {} (expected {})", ret, data.len()));
            }
            Ok(ret as usize)
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let quiet = QUIET_USB_READ.load(Ordering::Relaxed);
        
        if !quiet {
            debug!(
                "[USB READ] starting, buf_len={}, timeout={:?}ms",
                buf.len(),
                self.timeout.as_millis()
            );
        }

        unsafe {
            let ret = libusb0::usb_bulk_read(
                self.handle,
                self.ep_in as c_int,
                buf.as_mut_ptr(),
                buf.len() as c_int,
                self.timeout.as_millis() as c_int,
            );

            if !quiet {
                debug!("[USB READ] bulk_read returned: {}", ret);
            }

            if ret < 0 {
                if !quiet {
                    debug!("[USB READ] error, returning");
                }
                return Err(format!("read err {}", ret));
            }

            let total = ret as usize;
            if total > 0 {
                usb_trace("RX", "UsbDevice::read", &buf[..total]);
            }

            Ok(total)
        }
    }

    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if buf.is_empty() {
            return Ok(0);
        }

        debug!(
            "[USB READ EXACT] starting, buf_len={}, timeout={:?}ms",
            buf.len(),
            self.timeout.as_millis()
        );

        let mut total = 0usize;
        while total < buf.len() {
            unsafe {
                let ret = libusb0::usb_bulk_read(
                    self.handle,
                    self.ep_in as c_int,
                    buf[total..].as_mut_ptr(),
                    (buf.len() - total) as c_int,
                    self.timeout.as_millis() as c_int,
                );

                debug!(
                    "[USB READ EXACT] bulk_read returned: {}",
                    ret
                );

                if ret < 0 {
                    return Err(format!("read_exact err {}", ret));
                }

                total += ret as usize;
            }
        }

        debug!("[USB READ EXACT] total read: {}/{} bytes", total, buf.len());
        if total > 0 {
            usb_trace("RX", "UsbDevice::read_exact", &buf[..total]);
        }

        Ok(total)
    }

    #[allow(dead_code)] // 预留：Kamakiri2 exploit 需要 USB control transfer 读取数据
    pub fn ctrl_transfer_in(
        &mut self,
        requesttype: u8,
        request: u8,
        value: u16,
        index: u16,
        length: u16,
    ) -> Result<Vec<u8>, String> {
        debug!(
            "[CTRL] IN rt=0x{:02X} r=0x{:02X} v=0x{:04X} i=0x{:04X} len={}",
            requesttype, request, value, index, length
        );

        unsafe {
            let mut buf = vec![0u8; length as usize];
            let ret = libusb0::usb_control_msg(
                self.handle,
                requesttype as c_int,
                request as c_int,
                value as c_int,
                index as c_int,
                buf.as_mut_ptr(),
                length as c_int,
                self.timeout.as_millis() as c_int,
            );

            if ret < 0 {
                debug!("[CTRL] IN error: {}", ret);
                Err(format!("ctrl_in {}", ret))
            } else {
                buf.truncate(ret as usize);
                usb_trace("RX", "UsbDevice::ctrl_transfer_in", &buf);
                debug!(
                    "[CTRL] IN OK: {:02X?}",
                    &buf[..std::cmp::min(buf.len(), 16)]
                );
                Ok(buf)
            }
        }
    }

    #[allow(dead_code)]
    pub fn clear_halt_in(&mut self) -> Result<(), String> {
        self.clear_halt_ep(self.ep_in)
    }

    #[allow(dead_code)] // 预留：bulk OUT 端点 stall 时复位
    pub fn clear_halt_out(&mut self) -> Result<(), String> {
        self.clear_halt_ep(self.ep_out)
    }

    fn clear_halt_ep(&mut self, ep: u8) -> Result<(), String> {
        unsafe {
            let ret = libusb0::usb_clear_halt(self.handle, ep as c_uint);
            if ret != 0 {
                Err(format!("clear_halt ep=0x{:02X} err {}", ep, ret))
            } else {
                debug!("clear_halt ep=0x{:02X} OK", ep);
                Ok(())
            }
        }
    }

    #[allow(dead_code)] // 预留：Kamakiri2 exploit 需要 USB control transfer 发送数据
    pub fn ctrl_transfer_out(
        &mut self,
        requesttype: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), String> {
        usb_trace("TX", "UsbDevice::ctrl_transfer_out", data);
        debug!(
            "[CTRL] OUT rt=0x{:02X} r=0x{:02X} v=0x{:04X} i=0x{:04X} data={:02X?}",
            requesttype, request, value, index, data
        );

        unsafe {
            let ret = libusb0::usb_control_msg(
                self.handle,
                requesttype as c_int,
                request as c_int,
                value as c_int,
                index as c_int,
                data.as_ptr() as *mut _,
                data.len() as c_int,
                self.timeout.as_millis() as c_int,
            );

            if ret < 0 {
                debug!("[CTRL] OUT error: {}", ret);
                Err(format!("ctrl_out {}", ret))
            } else {
                debug!("[CTRL] OUT OK: {} bytes sent", ret);
                Ok(())
            }
        }
    }

    pub fn set_timeout(&mut self, duration: Duration) {
        self.timeout = duration;
    }

    pub fn get_timeout(&self) -> Duration {
        self.timeout
    }

    pub fn do_handshake(&mut self) -> Result<bool, String> {
        let cmd = [0xA0u8, 0x0A, 0x50, 0x05];
        let maxinsize = 512u16;

        if !self.device_type.is_brom() {
            info!(
                "[USB] non-BROM PID (0x{:04X}), sending 0xA0 first",
                self.pid
            );
            let _ = self.write(&[0xA0]);
            std::thread::sleep(Duration::from_millis(10));
        }

        for attempt in 0..10 {
            if attempt > 0 {
                info!(
                    "[USB] handshake attempt {}/10, waiting 300ms...",
                    attempt + 1
                );
                std::thread::sleep(Duration::from_millis(300));
            }

            let orig_timeout = self.timeout;
            self.timeout = Duration::from_millis(50);
            
            // 清空缓冲区
            let mut drain = [0u8; 64];
            loop {
                match self.read(&mut drain) {
                    Ok(n) if n > 0 => continue,
                    _ => break,
                }
            }
            
            self.timeout = orig_timeout;

            let mut ok = true;
            let mut i = 0;
            while i < 4 {
                if let Err(e) = self.write(&[cmd[i]]) {
                    info!("[USB] handshake write error at byte {}: {}", i, e);
                    ok = false;
                    break;
                }
                usb_trace("TX", "UsbDevice::do_handshake echo_write", &[cmd[i]]);
                
                let mut r = vec![0u8; maxinsize as usize];
                let n = match self.read(&mut r) {
                    Ok(n) => n,
                    Err(_) => {
                        info!("[USB] handshake read error at byte {}", i);
                        ok = false;
                        break;
                    }
                };
                
                if n == 0 {
                    info!("[USB] handshake read returned 0 bytes at byte {}", i);
                    ok = false;
                    break;
                }

                usb_trace("RX", "UsbDevice::do_handshake echo_read", &r[..n]);
                let last_byte = r[n - 1];
                
                if last_byte == cmd[i] {
                    i += 1;
                } else {
                    info!(
                        "[USB] handshake mismatch at byte {}: got 0x{:02X}, expected 0x{:02X}",
                        i, last_byte, cmd[i]
                    );
                    i = 0;
                }
            }
            
            if ok {
                info!("Handshake OK");
                return Ok(true);
            }
        }
        
        Err("Handshake failed after 10 attempts".into())
    }

    pub fn close(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                let _ = libusb0::usb_release_interface(self.handle, 1);
                let _ = libusb0::usb_release_interface(self.handle, 0);
                let _ = libusb0::usb_close(self.handle);
                self.handle = std::ptr::null_mut();
            }
        }
    }

    #[allow(dead_code)] // 预留：bypass 后重建 USB 连接（当前通过 ConnectionManager 重连）
    pub fn reopen(&mut self, context: &UsbContext) -> Result<(), String> {
        self.close();
        std::thread::sleep(Duration::from_millis(200));

        let mut new_device = UsbDevice::new(context)?;

        self.handle = new_device.handle;
        self.ep_in = new_device.ep_in;
        self.ep_out = new_device.ep_out;
        self.vid = new_device.vid;
        self.pid = new_device.pid;
        self.timeout = new_device.timeout;
        self.device_type = new_device.device_type;
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

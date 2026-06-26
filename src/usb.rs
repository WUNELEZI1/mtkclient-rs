use crate::config::{DeviceType, SUPPORTED_DEVICES};
use log::{debug, info};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const LIBUSB_ERROR_TIMEOUT: i32 = -7;

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

/// 枚举 USB 设备列表，检查是否有任何 MediaTek 设备（BROM 0x0003 / Preloader 0x2000/0x2001）
///
/// 返回值：
/// - `Some((pid, device_type))`：找到的第一个 MediaTek 设备
/// - `None`：无 MediaTek 设备
///
/// 用途：在 smart_init 的 COM 扫描之前调用，如果 libusb 已经能看到 BROM 设备 (PID 0x0003)，
/// 就直接走 WinUSB 模式，跳过耗时 21 秒的 COM 扫描。
/// 注意：只识别 BROM 阶段设备 (PID 0x0003)，其他 PID（Preloader 0x2000、DA 0x2001、Unknown 0x2008 等）
/// 一律忽略，避免误识别触发错误路径。
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
/// 与 check_mediatek_device_via_libusb 的区别：
/// - check_mediatek_device_via_libusb 只识别 BROM 设备 (PID 0x0003)，用于前置检测
/// - 本函数识别所有 MediaTek 设备（BROM/Preloader/DA），用于 DA 会话复用检查
///
/// 用途：main.rs 启动时检测 DA 会话复用：
/// 1. 枚举所有 MediaTek USB 设备
/// 2. 找到第一个设备的 (VID, PID)
/// 3. 与 .state 文件比对，如果 .state 中 da_loaded=true 且设备在线，触发会话复用
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
            // 严格仅识别 BROM 阶段 (PID 0x0003)
            if desc.idProduct != 0x0003 {
                continue;
            }
            let dev_type = DeviceType::from_vid_pid(desc.idVendor, desc.idProduct);
            debug!(
                "[USB] get_first_mediatek_vid_pid: 找到 BROM 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
                desc.idVendor, desc.idProduct, dev_type
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
/// 用于在 smart_init 中判断是否跳过串口扫描。只要设备以 USB 方式连接（无论什么 PID），
/// 基本上就不会在串口上产生握手响应，直接走 USB 等待逻辑能节省 21 秒。
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

/// 全局静默标志：设置为 true 时，read() 不打印 [USB READ] 日志
static QUIET_USB_READ: AtomicBool = AtomicBool::new(false);

/// 全局 USB trace 开关：设置为 true 时，记录所有 USB 通信到 usb_debug.log
static USB_LOG_ENABLED: AtomicBool = AtomicBool::new(false);

/// USB trace 日志文件（使用 Mutex 保证线程安全写入）
static USB_LOG_FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);

/// 设置 USB 读取静默模式
pub fn set_quiet_usb_read(quiet: bool) {
    QUIET_USB_READ.store(quiet, Ordering::Relaxed);
}

/// 设置 USB trace 开关
pub fn set_usb_log_enabled(enabled: bool) {
    USB_LOG_ENABLED.store(enabled, Ordering::Relaxed);
    if enabled {
        // 打开 usb_debug.log 文件（追加模式）
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

/// USB 通信追踪日志，对齐 Python usb_debug.log 格式
/// 格式：[HH:MM:SS.mmm] [TX/RX] [函数名::行号] hex_data
pub fn usb_trace(direction: &str, func_info: &str, data: &[u8]) {
    if !USB_LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    // 获取当前时间
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    // 转换为本地时间（简化版，使用 UTC+8 东八区）
    let total_secs = secs % 86400;
    let hours = (total_secs / 3600) as u32;
    let minutes = ((total_secs % 3600) / 60) as u32;
    let seconds = (total_secs % 60) as u32;

    // 格式化 hex 数据
    let hex_str: String = data.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");

    // 格式化日志行
    let log_line = format!(
        "[{:02}:{:02}:{:02}.{:03}] [{}] [{}] {}\n",
        hours, minutes, seconds, millis, direction, func_info, hex_str
    );

    // 写入文件
    if let Ok(mut guard) = USB_LOG_FILE.lock() {
        if let Some(ref mut file) = *guard {
            let _ = file.write_all(log_line.as_bytes());
            let _ = file.flush();
        }
    }
}

/// 辅助函数：格式化行号信息
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

pub struct UsbDevice {
    handle: *mut libusb1_sys::libusb_device_handle,
    pub vid: u16,
    pub pid: u16,
    pub stage: UsbStage,
    device_type: DeviceType,
    pub ep_out: u8,
    pub ep_in: u8,
    #[allow(dead_code)]
    ep_out_max_packet_size: u16,
    timeout: Duration,
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
                return Err(format!("获取设备描述失败 (error {}): libusb 驱动异常", ret_desc));
            }

            debug!("[USB] scanning endpoints...");
            let mut config_ptr: *const libusb1_sys::libusb_config_descriptor = std::ptr::null();
            let mut ep_out_addr: u8 = 0x01;
            let mut ep_in_addr: u8 = 0x81;
            let mut ep_out_max_pkt: u16 = 512;
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
                        debug!(
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
                            debug!(
                                "[USB]   EP: 0x{:02X} dir={} type={} size={}",
                                addr, dir, ep_type, ep.wMaxPacketSize
                            );
                            if dir == "OUT" && ep_type == "Bulk" {
                                ep_out_addr = addr;
                                ep_out_max_pkt = ep.wMaxPacketSize;
                            }
                            if dir == "IN" && ep_type == "Bulk" {
                                ep_in_addr = addr;
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
                timeout: Duration::from_millis(5000),
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
                return Err(format!("获取设备描述失败 (error {}): libusb 驱动异常", ret_desc));
            }

            debug!("[USB] scanning endpoints...");
            let mut config_ptr: *const libusb1_sys::libusb_config_descriptor = std::ptr::null();
            let mut ep_out_addr: u8 = 0x01;
            let mut ep_in_addr: u8 = 0x81;
            let mut ep_out_max_pkt: u16 = 512;
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
                            }
                        }
                    }
                }
                libusb1_sys::libusb_free_config_descriptor(config_ptr);
            }

            let stage = UsbStage::from_pid(desc.idProduct);
            debug!("[USB] open_by_vid_pid OK: VID={:04X} PID={:04X} stage={:?}", vid, pid, stage);

            Ok(UsbDevice {
                handle,
                vid: desc.idVendor,
                pid: desc.idProduct,
                stage,
                device_type: DeviceType::from_vid_pid(vid, desc.idProduct),
                ep_out: ep_out_addr,
                ep_in: ep_in_addr,
                ep_out_max_packet_size: ep_out_max_pkt,
                timeout: Duration::from_millis(5000),
            })
        }
    }

    /// 是否是 libusb（WinUSB）后端
    pub fn is_libusb(&self) -> bool {
        true
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        if data.is_empty() {
            // ZLP (Zero Length Packet)
            usb_trace("TX", "UsbDevice::write ZLP", &[]);
            unsafe {
                let mut transferred: i32 = 0;
                let ret = libusb1_sys::libusb_bulk_transfer(
                    self.handle,
                    self.ep_out,
                    std::ptr::null_mut(),
                    0,
                    &mut transferred,
                    self.timeout.as_millis() as u32,
                );
                if ret != 0 {
                    return Err(format!("write ZLP err {}", ret));
                }
            }
            return Ok(0);
        }

        // TX trace: 记录发送的数据
        usb_trace("TX", "UsbDevice::write", data);

        // Python usbwrite sends all data in one libusb_bulk_transfer call
        // No chunking - let libusb handle USB packetization internally
        unsafe {
            let mut transferred: i32 = 0;
            let ret = libusb1_sys::libusb_bulk_transfer(
                self.handle,
                self.ep_out,
                data.as_ptr() as *mut u8,
                data.len() as i32,
                &mut transferred,
                self.timeout.as_millis() as u32,
            );
            if ret != 0 {
                return Err(format!(
                    "write err {} (transferred={}/{})",
                    ret,
                    transferred,
                    data.len()
                ));
            }
            Ok(transferred as usize)
        }
    }

    /// 获取 EP_OUT 的最大包大小
    #[allow(dead_code)]
    pub fn ep_out_max_packet_size(&self) -> u16 {
        self.ep_out_max_packet_size
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let quiet = QUIET_USB_READ.load(Ordering::Relaxed);
        let mut total = 0usize;
        let deadline = std::time::Instant::now() + self.timeout;
        if !quiet {
            debug!(
                "[USB READ] starting, buf_len={}, timeout={:?}ms",
                buf.len(),
                self.timeout.as_millis()
            );
        }
        while total < buf.len() {
            let remaining = buf.len() - total;
            unsafe {
                let mut transferred: i32 = 0;
                let now = std::time::Instant::now();
                if now >= deadline {
                    if !quiet {
                        debug!("[USB READ] deadline reached, breaking, total={}", total);
                    }
                    break;
                }
                let ms_left = match (deadline - now).checked_sub(Duration::ZERO) {
                    Some(d) => {
                        let ms = d.as_millis() as u32;
                        if ms == 0 { 1 } else { ms }
                    }
                    None => break,
                };
                if !quiet {
                    debug!(
                        "[USB READ] calling bulk_transfer, remaining={}, timeout={}ms",
                        remaining, ms_left
                    );
                }
                let ret = libusb1_sys::libusb_bulk_transfer(
                    self.handle,
                    self.ep_in,
                    buf[total..].as_mut_ptr(),
                    remaining as i32,
                    &mut transferred,
                    ms_left,
                );
                if !quiet {
                    debug!(
                        "[USB READ] bulk_transfer returned: ret={}, transferred={}",
                        ret, transferred
                    );
                }

                if ret == LIBUSB_ERROR_TIMEOUT && timeout < 50 {
                    // 优化：对于极短超时（如 flush/drain），超时即视为无更多数据，直接返回
                    return Ok(total);
                }

                if ret != 0 && ret != LIBUSB_ERROR_TIMEOUT {
                    if !quiet {
                        debug!("[USB READ] error, returning");
                    }
                    return Err(format!("read err {}", ret));
                }
                total += transferred as usize;

                // 核心修复：一旦读到任何数据，立即返回，不要等待读满整个 buf.len()
                // 这符合标准 read 语义，也避免了 handshake 等场景下的挂起
                if total > 0 {
                    if !quiet {
                        debug!("[USB READ] data received ({} bytes), returning early", total);
                    }
                    break;
                }

                if transferred == 0 {
                    // ZLP 或设备忙，对齐 PyUSB 行为：自动忽略 ZLP 继续等待数据
                    if ret == 0 {
                        // ZLP (zero-length packet): ret=0, transferred=0
                        if !quiet {
                            debug!("[USB READ] ZLP received, retrying");
                        }
                    }
                    if now < deadline {
                        if !quiet {
                            debug!("[USB READ] transferred=0, retrying in 10ms");
                        }
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    } else {
                        if !quiet {
                            debug!("[USB READ] transferred=0 at deadline, breaking");
                        }
                        break;
                    }
                }
            }
        }
        if !quiet {
            debug!("[USB READ] returning total={}", total);
        }
        // RX trace: 记录实际读取的数据
        if total > 0 {
            usb_trace("RX", "UsbDevice::read", &buf[..total]);
        }
        Ok(total)
    }

    /// 精确读取：循环 bulk transfer 直到读满 buf.len()
    /// 对齐 Python usbread(length) — 精确读 length 字节
    /// 修复：之前只调用一次 bulk_transfer，如果设备分多次返回数据（如先返回 2 字节再返回 2 字节），
    ///       会导致读不完整，残留字节污染后续 echo 通信。
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
                let mut transferred: i32 = 0;
                let ret = libusb1_sys::libusb_bulk_transfer(
                    self.handle,
                    self.ep_in,
                    buf[total..].as_mut_ptr(),
                    (buf.len() - total) as i32,
                    &mut transferred,
                    self.timeout.as_millis() as u32,
                );
                debug!(
                    "[USB READ EXACT] bulk_transfer returned: ret={}, transferred={}",
                    ret, transferred
                );
                if ret != 0 && ret != LIBUSB_ERROR_TIMEOUT {
                    return Err(format!("read_exact err {}", ret));
                }
                if transferred == 0 {
                    if ret == LIBUSB_ERROR_TIMEOUT {
                        if total > 0 {
                            debug!("[USB READ EXACT] partial read: {}/{} bytes before timeout", total, buf.len());
                            break;
                        }
                        return Err("read_exact timeout".to_string());
                    }
                    break;
                }
                total += transferred as usize;
            }
        }
        debug!("[USB READ EXACT] total read: {}/{} bytes", total, buf.len());
        // RX trace
        if total > 0 {
            usb_trace("RX", "UsbDevice::read_exact", &buf[..total]);
        }
        Ok(total)
    }

    pub fn ctrl_transfer_in(
        &mut self,
        rt: u8,
        r: u8,
        v: u16,
        i: u16,
        len: u16,
    ) -> Result<Vec<u8>, String> {
        debug!(
            "[CTRL] IN rt=0x{:02X} r=0x{:02X} v=0x{:04X} i=0x{:04X} len={}",
            rt, r, v, i, len
        );
        unsafe {
            let mut buf = vec![0u8; len as usize];
            let ret = libusb1_sys::libusb_control_transfer(
                self.handle,
                rt,
                r,
                v,
                i,
                buf.as_mut_ptr(),
                len,
                self.timeout.as_millis() as u32,
            );
            if ret < 0 {
                debug!("[CTRL] IN error: {}", ret);
                Err(format!("ctrl_in {}", ret))
            } else {
                buf.truncate(ret as usize);
                // RX trace: 记录 control transfer 接收的数据
                usb_trace("RX", "UsbDevice::ctrl_transfer_in", &buf);
                debug!(
                    "[CTRL] IN OK: {:02X?}",
                    &buf[..std::cmp::min(buf.len(), 16)]
                );
                Ok(buf)
            }
        }
    }

    /// 复位 bulk IN 端点（清除 halt/stall 状态）
    #[allow(dead_code)]
    pub fn clear_halt_in(&mut self) -> Result<(), String> {
        self.clear_halt_ep(self.ep_in)
    }

    /// 复位 bulk OUT 端点（清除 halt/stall 状态）
    #[allow(dead_code)]
    pub fn clear_halt_out(&mut self) -> Result<(), String> {
        self.clear_halt_ep(self.ep_out)
    }

    /// 复位指定 bulk 端点
    pub fn clear_halt_ep(&mut self, ep: u8) -> Result<(), String> {
        unsafe {
            let ret = libusb1_sys::libusb_clear_halt(self.handle, ep);
            if ret != 0 {
                Err(format!("clear_halt ep=0x{:02X} err {}", ep, ret))
            } else {
                debug!("clear_halt ep=0x{:02X} OK", ep);
                Ok(())
            }
        }
    }

    pub fn ctrl_transfer_out(
        &mut self,
        rt: u8,
        r: u8,
        v: u16,
        i: u16,
        data: &[u8],
    ) -> Result<(), String> {
        // TX trace: 记录 control transfer 发送的数据
        usb_trace("TX", "UsbDevice::ctrl_transfer_out", data);
        debug!(
            "[CTRL] OUT rt=0x{:02X} r=0x{:02X} v=0x{:04X} i=0x{:04X} data={:02X?}",
            rt, r, v, i, data
        );
        unsafe {
            let ret = libusb1_sys::libusb_control_transfer(
                self.handle,
                rt,
                r,
                v,
                i,
                data.as_ptr() as *mut u8,
                data.len() as u16,
                self.timeout.as_millis() as u32,
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
        let maxinsize = 512u16; // wMaxPacketSize for high-speed bulk

        // 使用缓存的设备类型判断握手策略
        if !self.device_type.is_brom() {
            info!(
                "[USB] non-BROM PID (0x{:04X}), sending 0xA0 first",
                self.pid
            );
            let _ = self.write(&[0xA0]);
            std::thread::sleep(Duration::from_millis(10));
        }

        // Python retries handshakes with delays between attempts
        for attempt in 0..10 {
            if attempt > 0 {
                info!(
                    "[USB] handshake attempt {}/10, waiting 300ms...",
                    attempt + 1
                );
                std::thread::sleep(Duration::from_millis(300));
            }
            // Drain any stale data first
            let orig_timeout = self.timeout;
            self.timeout = Duration::from_millis(50);
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
                // echo write trace
                usb_trace("TX", "UsbDevice::do_handshake echo_write", &[cmd[i]]);
                let mut r = [0u8; 1]; // 握手 echo 每次只读 1 字节
                match self.read(&mut r) {
                    Ok(n) if n > 0 => {
                        // echo read trace
                        usb_trace("RX", "UsbDevice::do_handshake echo_read", &r[..n]);
                        let last_byte = r[n - 1];
                        if last_byte == !cmd[i] {
                            i += 1;
                        } else {
                            info!(
                                "[USB] handshake mismatch at byte {}: got 0x{:02X}, expected 0x{:02X}",
                                i, last_byte, !cmd[i]
                            );
                            i = 0; // Python 重置计数器
                        }
                    }
                    _ => {
                        info!("[USB] handshake read error at byte {}", i);
                        ok = false;
                        break;
                    }
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
        std::thread::sleep(Duration::from_millis(200));

        let mut new_device = UsbDevice::new(context)?;

        // 交换字段
        self.handle = new_device.handle;
        self.ep_in = new_device.ep_in;
        self.ep_out = new_device.ep_out;
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

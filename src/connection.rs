use crate::preloader::{self, BromTransport, Preloader};
use crate::usb;
use crate::usb::UsbContext;
use colored::Colorize;
use log::{debug, info};
use std::time::Duration;

/// 设备模式
#[derive(Debug, PartialEq, Clone)]
pub enum DeviceMode {
    /// BROM 模式（通过 libusb 通信）
    Brom,
    /// Preloader 模式（串口已释放，切换到 USB BROM）
    Preloader,
}

/// 统一连接管理器
/// 
/// 职责：
/// 1. 统一设备入口（smart_init）
/// 2. 管理连接状态（BROM ↔ Preloader 切换）
/// 3. 自动重连（reconnect_loop）
/// 4. 对齐 MTKClient Python 行为
/// 
/// 连接优先级（工程级稳定）：
/// 1. USB 直接检测（BROM VID=0x0E8D PID=0x0003）→ 最快路径
/// 2. 串口检测（Preloader 模式）→ 握手 → 释放 → USB 重枚举
/// 3. reconnect_loop（10 秒超时，200ms 重试）
/// 
/// 对齐 MTKClient Python:
/// - usblib.py::connect() → UsbDevice::open_by_vid_pid()
/// - mtk_preloader.py::init() → ConnectionManager::smart_init()
/// - Port.py::run_handshake() → SerialPortTransport::do_handshake()
/// 
/// 驱动策略：
/// - 不使用 libusb-win32 filter（install-filter.exe）
/// - 依赖 UsbDk 或系统 WinUSB
/// - 串口释放后设备自动重枚举为 BROM VID/PID
pub struct ConnectionManager {
    mode: DeviceMode,
    port_name: Option<String>,
    /// 设备重连后自动调用的回调（预留：DA 加载后重连使用）
    reconnect_callback: Option<Box<dyn Fn() -> Result<(), String>>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        ConnectionManager {
            mode: DeviceMode::Preloader,
            port_name: None,
            reconnect_callback: None,
        }
    }

    /// 统一设备初始化入口
    /// 
    /// 完整流程（对齐刷机匣 + MTKClient Python）：
    /// ```
    /// 优先级 1: USB 直接连接（BROM 模式）
    ///   └─ open_by_vid_pid(0x0E8D, 0x0003) → 成功 → 返回
    /// 
    /// 优先级 2: 串口检测 + 握手 + 切换
    ///   └─ 扫描 COM 端口 → 打开串口 → BROM 握手 → init
    ///   └─ 释放串口 → 等待重枚举 → reconnect_loop → libusb 接管
    /// 
    /// 优先级 3: reconnect_loop 循环检测
    ///   └─ 10 秒超时，200ms 重试
    /// ```
    pub fn smart_init(
        &mut self,
        context: &UsbContext,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!(
            "等待设备连接 (Preloader: 直接连接 / BROM: Vol+ + Vol- + Power)"
        );

        // === 优先级 1：USB 直接连接（最快路径） ===
        if let Ok(device) = usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x0003) {
            info!(
                "  VID: {:04x}, PID: {:04x}, EP_OUT=0x{:02X}, EP_IN=0x{:02X}",
                device.vid, device.pid, device.ep_out, device.ep_in
            );
            info!("{}", "USB 直接连接成功 (BROM 模式)".green().bold());
            self.mode = DeviceMode::Brom;
            return Ok((Preloader::new(Box::new(device)), DeviceMode::Brom));
        }
        debug!("USB 直接连接失败，尝试串口检测...");

        // === 优先级 2：串口检测 + 握手 + 切换 ===
        if let Some((port_name, preloader)) = self.serial_connect()? {
            info!("串口握手成功，准备切换到 USB 模式...");

            // 释放串口（关键：必须释放 COM 句柄，否则 USB 无法打开）
            drop(preloader);
            debug!("串口已释放");

            // 等待设备重枚举（设备从 CDC 切换到 BROM VID/PID）
            info!("等待设备重枚举...");
            std::thread::sleep(Duration::from_millis(500));

            // reconnect_loop: 循环检测 libusb 设备（10 秒，200ms 重试）
            let usb_device = self.reconnect_loop(context, 0x0E8D, 0x0003)?;

            info!(
                "  VID: {:04x}, PID: {:04x}, EP_OUT=0x{:02X}, EP_IN=0x{:02X}",
                usb_device.vid, usb_device.pid, usb_device.ep_out, usb_device.ep_in
            );

            self.mode = DeviceMode::Brom;
            self.port_name = Some(port_name);
            return Ok((
                Preloader::new(Box::new(usb_device)),
                DeviceMode::Brom,
            ));
        }

        // === 优先级 3：reconnect_loop 循环检测 ===
        info!("串口检测超时，尝试 USB 循环连接...");

        let usb_device = self.reconnect_loop(context, 0x0E8D, 0x0003)?;

        info!(
            "  VID: {:04x}, PID: {:04x}, EP_OUT=0x{:02X}, EP_IN=0x{:02X}",
            usb_device.vid, usb_device.pid, usb_device.ep_out, usb_device.ep_in
        );

        self.mode = DeviceMode::Brom;
        Ok((Preloader::new(Box::new(usb_device)), DeviceMode::Brom))
    }

    /// 串口连接 + 握手
    /// 
    /// 扫描 COM 端口 → 打开串口 → BROM 握手 → init → 返回 Preloader
    /// 
    /// 对齐 MTKClient Python:
    /// - com.py::detect_port() → 扫描 COM 端口
    /// - Port.py::connect() → 打开串口 + 握手
    /// - mtk_preloader.py::init() → 完整初始化
    fn serial_connect(&self) -> Result<Option<(String, Preloader)>, String> {
        let mut attempt = 0;
        const MAX_ATTEMPTS: usize = 30;

        while attempt < MAX_ATTEMPTS {
            attempt += 1;

            if let Some(port_name) = preloader::detect_serial_preloader() {
                info!("发现 Preloader 串口设备: {}", port_name);

                let transport = preloader::SerialPortTransport::new(&port_name, 115200)
                    .map_err(|e| format!("打开串口失败: {}", e))?;
                let device: Box<dyn BromTransport> = Box::new(transport);
                let mut preloader = Preloader::new(device);

                if preloader.init().unwrap_or(false) {
                    return Ok(Some((port_name, preloader)));
                }
                debug!("串口握手失败，继续轮询...");
            }

            std::thread::sleep(Duration::from_secs(1));
        }

        debug!("串口检测完成，未找到可用设备");
        Ok(None)
    }

    /// libusb 重连循环
    /// 
    /// 对齐 MTKClient Python 行为：
    /// - usblib.py::connect() 循环扫描设备
    /// - mtk_preloader.py::init() 重试机制
    /// 
    /// 关键特性：
    /// - 10 秒超时，200ms 重试间隔
    /// - 支持 USB reset 后重连
    /// - 支持 Kamakiri exploit 后重连
    /// - 支持 DA 加载后重连
    /// - 所有错误自动重试，不信任单次连接
    /// 
    /// 日志输出：
    /// ```
    /// [RECONNECT] retry 1/50...
    /// [RECONNECT] retry 6/50...
    /// [RECONNECT] success on attempt 3/50
    /// ```
    pub fn reconnect_loop(
        &self,
        context: &UsbContext,
        vid: u16,
        pid: u16,
    ) -> Result<usb::UsbDevice, String> {
        const TIMEOUT_MS: u64 = 10_000;
        const INTERVAL_MS: u64 = 200;
        let max_retries = (TIMEOUT_MS / INTERVAL_MS) as usize;
        let mut retry = 0;

        info!("正在通过 libusb 连接设备 (VID=0x{:04X} PID=0x{:04X})...", vid, pid);

        while retry < max_retries {
            retry += 1;

            match usb::UsbDevice::open_by_vid_pid(context, vid, pid) {
                Ok(device) => {
                    info!("[RECONNECT] success on attempt {}/{}", retry, max_retries);
                    return Ok(device);
                }
                Err(e) => {
                    if retry % 5 == 1 || retry == max_retries {
                        debug!("[RECONNECT] retry {}/{}: {}", retry, max_retries, e);
                    }
                    std::thread::sleep(Duration::from_millis(INTERVAL_MS));
                }
            }
        }

        Err(format!(
            "libusb 连接超时 ({}ms)，未找到设备 VID=0x{:04X} PID=0x{:04X}",
            TIMEOUT_MS, vid, pid
        ))
    }

    /// DA 加载后重连（预留）
    /// 
    /// 对齐 MTKClient Python:
    /// - DA 加载后设备会 USB reset
    /// - 需要等待重枚举后重新连接
    /// 
    /// 用法：
    /// ```
    /// conn_mgr.reconnect_after_da(&context)?;
    /// ```
    #[allow(dead_code)]
    pub fn reconnect_after_da(
        &self,
        context: &UsbContext,
    ) -> Result<usb::UsbDevice, String> {
        info!("[DA] 等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(1000));
        self.reconnect_loop(context, 0x0E8D, 0x0003)
    }

    /// Kamakiri exploit 后重连（预留）
    /// 
    /// 对齐 mtkclient stage2.py:
    /// - 发送 payload 后设备 USB reset
    /// - 需要等待重枚举后重新连接
    #[allow(dead_code)]
    pub fn reconnect_after_kamakiri(
        &self,
        context: &UsbContext,
    ) -> Result<usb::UsbDevice, String> {
        info!("[KAMAKIRI] 等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(500));
        self.reconnect_loop(context, 0x0E8D, 0x0003)
    }

    /// 获取当前连接模式
    #[allow(dead_code)]
    pub fn mode(&self) -> &DeviceMode {
        &self.mode
    }

    /// 获取串口名称
    #[allow(dead_code)]
    pub fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }
}

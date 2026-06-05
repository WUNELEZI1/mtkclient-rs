use crate::preloader::{self, BromTransport, Preloader};
use crate::usb;
use crate::usb::UsbContext;
use log::{debug, info};
use std::time::Duration;

/// 设备模式
#[derive(Debug, PartialEq, Clone)]
pub enum DeviceMode {
    /// BROM 模式（通过 libusb 通信）
    Brom,
    /// Preloader 模式（通过串口通信，最终仍切换到 USB）
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
/// 连接优先级：
/// 1. 串口检测（Preloader 模式，快速）
/// 2. 串口握手 → init → 释放串口 → USB 重枚举 → libusb 接管
/// 3. 如果串口不可用 → 直接 libusb 连接
/// 
/// 对齐 MTKClient Python:
/// - usblib.py::connect() → 动态扫描 + detach + claim + endpoint 发现
/// - mtk_preloader.py::init() → 循环重试 + reconnect after exploit
/// - Port.py::run_handshake() → 4 字节握手唤醒设备
pub struct ConnectionManager {
    mode: DeviceMode,
    port_name: Option<String>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        ConnectionManager {
            mode: DeviceMode::Preloader,
            port_name: None,
        }
    }

    /// 统一设备初始化入口
    /// 
    /// 完整流程（对齐刷机匣 + MTKClient Python）：
    /// ```
    /// 1. 扫描 COM 端口（Preloader 模式）
    /// 2. 串口握手：A0 → FA（BROM handshake）
    /// 3. init：get_hw_code → watchdog disable → BROM sync
    /// 4. 释放串口（drop COM 句柄）
    /// 5. 等待 USB 重枚举（设备从 CDC 切换到 BROM VID/PID）
    /// 6. libusb 连接：open → detach → claim → endpoint 扫描
    /// 7. 返回 Preloader(libusb transport)
    /// ```
    pub fn smart_init(
        &mut self,
        context: &UsbContext,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!(
            "等待设备连接 (Preloader: 直接连接 / BROM: Vol+ + Vol- + Power)"
        );

        // === 阶段 1：串口检测 + 握手 ===
        if let Some((port_name, preloader)) = self.serial_connect()? {
            info!("串口握手成功，准备切换到 USB 模式...");

            // 释放串口
            drop(preloader);
            debug!("串口已释放");

            // 等待设备重枚举
            info!("等待设备重枚举...");
            std::thread::sleep(Duration::from_millis(500));

            // === 阶段 2：libusb 重连 ===
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

        // === 阶段 3：串口不可用，直接尝试 libusb ===
        info!("串口检测超时，尝试 USB 直接连接...");

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

        Ok(None)
    }

    /// libusb 重连循环
    /// 
    /// 对齐 MTKClient Python 行为：
    /// - usblib.py::connect() 循环扫描设备
    /// - mtk_preloader.py::init() 重试机制
    /// 
    /// 参数：
    /// - timeout_ms: 总超时（默认 10000ms）
    /// - interval_ms: 重试间隔（默认 200ms）
    fn reconnect_loop(
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
                    info!("libusb 连接成功 (尝试 {}/{})", retry, max_retries);
                    return Ok(device);
                }
                Err(e) => {
                    if retry % 5 == 1 || retry == max_retries {
                        debug!("libusb 连接尝试 {}/{}: {}", retry, max_retries, e);
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

    /// 获取当前连接模式（预留：诊断和状态显示）
    #[allow(dead_code)]
    pub fn mode(&self) -> &DeviceMode {
        &self.mode
    }

    /// 获取串口名称（预留：调试和日志）
    #[allow(dead_code)]
    pub fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }
}

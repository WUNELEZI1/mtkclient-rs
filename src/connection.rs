use crate::preloader::{self, BromTransport, Preloader};
use crate::usb;
use crate::usb::{UsbContext, UsbStage};
use colored::Colorize;
use log::{debug, info};
use std::time::Duration;

/// 设备模式
#[derive(Debug, PartialEq, Clone)]
pub enum DeviceMode {
    /// BROM 模式（串口或 libusb）
    Brom,
    /// Preloader 模式（需要后续操作进入 BROM）
    Preloader,
}

/// 统一连接管理器
///
/// 职责：
/// 1. 统一设备入口（smart_init）
/// 2. 管理连接状态（BROM ↔ Preloader）
/// 3. 自动重连（reconnect_loop）
///
/// 连接流程：
/// ```
/// 尝试 USB BROM（libusb）
/// ↓ 失败
/// 扫描 COM 端口
/// ↓ 发现设备
/// Preloader 握手
/// ↓ 判断 BROM 状态
/// 是 → 直接返回串口 BROM
/// 否 → 返回 Preloader 模式
/// ```
pub struct ConnectionManager {
    mode: DeviceMode,
    stage: UsbStage,
    port_name: Option<String>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        ConnectionManager {
            mode: DeviceMode::Preloader,
            stage: UsbStage::Unknown,
            port_name: None,
        }
    }

    /// 统一设备初始化入口
    ///
    /// 完整流程：
    /// ```
    /// STEP 1: 尝试 USB BROM（libusb 直接打开）
    ///   └─ 成功 → 返回 libusb BROM
    ///   └─ 失败 → STEP 2
    ///
    /// STEP 2: COM 端口扫描 + 握手
    ///   └─ 发现设备 → init → 判断 BROM 状态
    ///   └─ BROM → 返回串口 BROM（不切换 libusb）
    ///   └─ Preloader → 返回 Preloader
    ///   └─ 失败 → STEP 3
    ///
    /// STEP 3: reconnect_loop 循环检测 USB
    /// ```
    pub fn smart_init(
        &mut self,
        context: &UsbContext,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("等待设备连接 (Preloader: 直接连接 / BROM: Vol+ + Vol- + Power)");

        // === STEP 1: 尝试 USB BROM ===
        if let Ok(device) = usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x0003) {
            info!(
                "  VID: {:04x}, PID: {:04x}, stage={:?}",
                device.vid, device.pid, device.stage
            );
            info!("{}", "USB 直接连接成功 (BROM 模式)".green().bold());
            self.mode = DeviceMode::Brom;
            self.stage = UsbStage::Brom;
            return Ok((Preloader::new(Box::new(device)), DeviceMode::Brom));
        }

        info!("USB 直接连接失败，进入串口扫描...");

        // === STEP 2: COM 端口扫描 + 握手 ===
        if let Some((port_name, preloader)) = self.serial_connect()? {
            let is_brom = preloader.is_brom_ready();
            let mode = if is_brom { DeviceMode::Brom } else { DeviceMode::Preloader };
            self.mode = mode.clone();
            self.port_name = Some(port_name);
            return Ok((preloader, mode));
        }

        // === STEP 3: reconnect_loop 循环检测 USB ===
        info!("串口检测超时，尝试 USB 循环连接...");

        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;

        info!(
            "  VID: {:04x}, PID: {:04x}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.stage
        );

        self.mode = DeviceMode::Brom;
        self.stage = UsbStage::Brom;
        Ok((Preloader::new(Box::new(usb_device)), DeviceMode::Brom))
    }

    /// 串口连接 + 握手
    ///
    /// 流程：
    /// 1. 扫描 COM 端口
    /// 2. 打开串口
    /// 3. BROM 握手 + init
    /// 4. 判断是否已是 BROM → 直接返回串口设备
    /// 5. 否 → 返回 Preloader 模式
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
                    info!("COM 口握手成功: {}", port_name);

                    // 判断是否已经是 BROM
                    if preloader.is_brom_ready() {
                        info!("检测到 BROM（串口模式），直接使用串口通信");
                        return Ok(Some((port_name, preloader)));
                    }

                    // 仍在 Preloader 模式
                    info!("当前为 Preloader 模式");
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
    pub fn reconnect_loop(
        &self,
        context: &UsbContext,
        target_stage: UsbStage,
    ) -> Result<usb::UsbDevice, String> {
        const TIMEOUT_MS: u64 = 10_000;
        const INTERVAL_MS: u64 = 200;
        let max_retries = (TIMEOUT_MS / INTERVAL_MS) as usize;
        let mut retry = 0;

        info!("[RECONNECT] scanning for stage={:?}...", target_stage);

        let pids = match target_stage {
            UsbStage::Brom => vec![0x0003u16],
            UsbStage::Preloader => vec![0x2000u16],
            UsbStage::Unknown => vec![0x0003u16, 0x2000u16],
        };

        while retry < max_retries {
            retry += 1;

            for &pid in &pids {
                match usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, pid) {
                    Ok(device) => {
                        info!("[RECONNECT] success on attempt {} (stage={:?}, PID=0x{:04X})",
                              retry, device.stage, device.pid);
                        return Ok(device);
                    }
                    Err(e) => {
                        debug!("[RECONNECT] open_by_vid_pid failed for PID=0x{:04X}: {}", pid, e);
                    }
                }
            }

            if retry % 5 == 1 || retry == max_retries {
                debug!("[RECONNECT] retry {}/{} (scanning {} PIDs)...",
                       retry, max_retries, pids.len());
            }
            std::thread::sleep(Duration::from_millis(INTERVAL_MS));
        }

        let pid_list: String = pids.iter().map(|p| format!("0x{:04X}", p)).collect::<Vec<_>>().join(", ");
        Err(format!(
            "libusb 连接超时 ({}ms)，未找到设备 PID=[{}]",
            TIMEOUT_MS, pid_list
        ))
    }

    /// DA 加载后重连
    #[allow(dead_code)]
    pub fn reconnect_after_da(
        &self,
        context: &UsbContext,
    ) -> Result<usb::UsbDevice, String> {
        info!("[DA] DA 加载完成，等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(500));

        info!("[DA] 尝试 BROM PID (0x0003)...");
        if let Ok(device) = self.try_quick_connect(context, 0x0E8D, 0x0003, 5) {
            info!("[DA] 连接成功: VID=0x{:04X} PID=0x{:04X}", device.vid, device.pid);
            return Ok(device);
        }

        info!("[DA] BROM PID 失败，扫描所有已知 PID...");
        self.reconnect_loop(context, UsbStage::Unknown)
    }

    /// Kamakiri exploit 后重连
    #[allow(dead_code)]
    pub fn reconnect_after_kamakiri(
        &self,
        context: &UsbContext,
    ) -> Result<usb::UsbDevice, String> {
        info!("[KAMAKIRI] payload 已发送，等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(500));
        self.reconnect_loop(context, UsbStage::Brom)
    }

    /// USB reset 后重连
    #[allow(dead_code)]
    pub fn reconnect_after_usb_reset(
        &self,
        context: &UsbContext,
    ) -> Result<usb::UsbDevice, String> {
        info!("[USB] USB reset detected, waiting for re-enumeration...");
        std::thread::sleep(Duration::from_millis(500));
        self.reconnect_loop(context, UsbStage::Brom)
    }

    /// 快速连接尝试
    #[allow(dead_code)]
    fn try_quick_connect(
        &self,
        context: &UsbContext,
        vid: u16,
        pid: u16,
        retries: usize,
    ) -> Result<usb::UsbDevice, String> {
        for _ in 1..=retries {
            match usb::UsbDevice::open_by_vid_pid(context, vid, pid) {
                Ok(device) => return Ok(device),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
        Err(format!("快速连接失败 ({} 次重试)", retries))
    }

    /// 获取当前连接模式
    #[allow(dead_code)]
    pub fn mode(&self) -> &DeviceMode {
        &self.mode
    }

    /// 获取当前 USB 阶段
    #[allow(dead_code)]
    pub fn stage(&self) -> &UsbStage {
        &self.stage
    }

    /// 获取串口名称
    #[allow(dead_code)]
    pub fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }
}

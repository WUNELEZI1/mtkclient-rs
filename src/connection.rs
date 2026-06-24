use crate::preloader::{Preloader, SerialPortTransport};
use crate::usb;
use crate::usb::{UsbContext, UsbStage};
use colored::Colorize;
use log::{debug, info, warn};
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
/// 连接流程：
/// ```
/// STEP 1: COM 口扫描 → 握手 → 关看门狗 → 获取 hw_code
///   └─ 成功 → 释放 COM 口 → 装 filter → sleep → libusb 打开 → 重新握手 → 返回
///   └─ 失败 → STEP 2
///
/// STEP 2: libusb 轮询（10秒/200ms）
///   └─ 成功 → 返回 libusb 设备
///   └─ 失败 → 报错
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
    pub fn smart_init(
        &mut self,
        context: &UsbContext,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("等待设备连接 (BROM: Vol+ + Vol- + Power)");

        // === STEP 0: 无需卸载 filter（install-filter.exe 会直接安装） ===

        // === STEP 1: COM 口前置握手 ===
        if let Some(port_name) = SerialPortTransport::find_brom_port() {
            info!("发现 BROM COM 口: {}", port_name);

            match self.serial_handshake_and_switch(&port_name, context) {
                Ok(preloader) => {
                    info!("{}", "COM 口前置握手成功，已切换到 libusb".green().bold());
                    self.mode = DeviceMode::Brom;
                    self.stage = UsbStage::Brom;
                    return Ok((preloader, DeviceMode::Brom));
                }
                Err(e) => {
                    warn!("COM 口前置握手失败: {}，尝试 libusb 直连", e);
                }
            }
        }

        // === STEP 2: libusb 轮询 ===
        info!("尝试 libusb 直连...");
        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;

        info!(
            "  VID: {:04x}, PID: {:04x}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.stage
        );

        self.mode = DeviceMode::Brom;
        self.stage = UsbStage::Brom;
        Ok((Preloader::new(Box::new(usb_device)), DeviceMode::Brom))
    }

    /// COM 口前置握手 → 释放 → libusb 接管
    ///
    /// 流程：
    /// 1. 打开 COM 口
    /// 2. 握手 + init()（关看门狗 + 获取 hw_code）
    /// 3. 释放 COM 口
    /// 4. 安装 libusb filter
    /// 5. 等待设备重枚举
    /// 6. libusb 打开设备
    /// 7. 重新握手
    /// 8. 返回 libusb Preloader
    fn serial_handshake_and_switch(
        &self,
        port_name: &str,
        context: &UsbContext,
    ) -> Result<Preloader, String> {
        // 1. 打开串口
        let transport = SerialPortTransport::new(port_name, 115200)?;
        let mut serial_preloader = Preloader::new(Box::new(transport));

        // 2. 完整 init：握手 + 关看门狗 + 获取 hw_code + 设置 chip
        if !serial_preloader.init().map_err(|e| format!("串口 init 失败: {}", e))? {
            return Err("串口握手失败".to_string());
        }

        let chip = serial_preloader.chip;
        info!("COM 口 init 成功，chip={:?}", chip.map(|c| c.hw_code));

        // 3. 释放 COM 口
        drop(serial_preloader);
        info!("COM 口已释放");

        // 4. 安装 libusb0 filter
        if let Err(e) = crate::driver::install_winusb_with_wdi(0x0E8D, 0x0003) {
            warn!("安装 libusb0 filter 失败: {}", e);
        }

        // 5. 等待设备重枚举（filter 安装后设备会重新枚举）
        std::thread::sleep(Duration::from_secs(3));

        // 6. libusb 打开设备（轮询 10 秒）
        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;
        info!("libusb 打开成功: VID={:04X} PID={:04X}", usb_device.vid, usb_device.pid);

        // 7. 重新握手
        let mut libusb_preloader = Preloader::new(Box::new(usb_device));
        if !libusb_preloader.init().map_err(|e| format!("libusb 重新握手失败: {}", e))? {
            return Err("libusb 重新握手失败".to_string());
        }

        // 8. 确保 chip 已设置
        if libusb_preloader.chip.is_none() {
            libusb_preloader.chip = chip;
        }

        Ok(libusb_preloader)
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

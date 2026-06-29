//! 设备连接管理器
//!
//! 负责从 USB/串口自动选择最佳路径，构造 [`Preloader`] 实例：
//! 1. 无限等待设备出现（USB 总线检测）
//! 2. WinUSB 已就绪 → 直接走 WinUSB 直连
//! 3. 串口驱动 → 打开串口握手 → 切换 WinUSB
//! 4. 3 次串口失败 → 降级 WinUSB 直连
//!
//! ```text
//! STEP 1: COM 口扫描 → 握手 → 关看门狗 → 获取 hw_code（最多 3 次）
//! STEP 2: WinUSB 直连（降级路径，跳过握手）
//! ```

use crate::连接管理::driver::{UsbBusDetectionResult, detect_brom_driver_from_usb_bus};
use crate::预加载器::{Preloader, SerialPortTransport};
use crate::USB通信;
use crate::USB通信::{USB上下文, USB阶段};
use colored::Colorize;
use log::{info, trace, warn};
use std::time::Duration;

const RECONNECT_INTERVAL_MS: u64 = 200;
const RECONNECT_LOG_INTERVAL: u32 = 25;
const SERIAL_HANDSHAKE_RETRY: u32 = 3;
const SERIAL_HANDSHAKE_RETRY_DELAY_SECS: u64 = 2;
const USB_REENUM_DELAY_SECS: u64 = 3;
const USB_REENUM_DELAY_MS: u64 = 500;
const QUICK_CONNECT_INTERVAL_MS: u64 = 200;

/// 设备模式
#[derive(Debug, PartialEq, Clone)]
pub enum DeviceMode {
    /// BROM 模式（串口或 WinUSB）
    Brom,
    /// Preloader 模式（需要后续操作进入 BROM）
    Preloader,
}

/// 统一连接管理器
pub struct ConnectionManager {
    mode: DeviceMode,
    stage: USB阶段,
    port_name: Option<String>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        ConnectionManager {
            mode: DeviceMode::Preloader,
            stage: USB阶段::未知,
            port_name: None,
        }
    }

    /// 统一设备初始化入口
    pub fn smart_init(&mut self, context: &USB上下文) -> Result<(Preloader, DeviceMode), String> {
        info!("等待设备连接 (BROM: Vol+ + Vol- + Power)");

        // 无限等待设备出现
        loop {
            let detection_result = detect_brom_driver_from_usb_bus();

            match detection_result {
                UsbBusDetectionResult::WinUsbReady => {
                    info!(
                        "{}",
                        "[USB] 检测到 WinUSB 驱动 (libwdi)，直接走 WinUSB 直连"
                            .green()
                            .bold()
                    );
                    return self.fallback_to_winusb(context);
                }
                UsbBusDetectionResult::SerialPort(port_name) => {
                    if port_name.is_empty() {
                        warn!("[COM] 找不到 BROM 设备对应的 COM 口，降级到 WinUSB 直连");
                        return self.fallback_to_winusb(context);
                    }

                    info!("[COM] 发现 BROM COM 口: {}", port_name);

                    for attempt in 1..=SERIAL_HANDSHAKE_RETRY {
                        info!(
                            "[COM] 尝试第 {}/{} 次打开串口...",
                            attempt, SERIAL_HANDSHAKE_RETRY
                        );
                        match self.serial_handshake_and_switch(&port_name, context) {
                            Ok(preloader) => {
                                info!("{}", "COM 口前置握手成功，已切换到 WinUSB".green().bold());
                                self.mode = DeviceMode::Brom;
                                self.stage = USB阶段::Brom;
                                self.port_name = Some(port_name);
                                return Ok((preloader, DeviceMode::Brom));
                            }
                            Err(e) => {
                                warn!(
                                    "[COM] 第 {}/{} 次握手失败: {}",
                                    attempt, SERIAL_HANDSHAKE_RETRY, e
                                );
                                if attempt < SERIAL_HANDSHAKE_RETRY {
                                    info!(
                                        "[COM] 等待 {} 秒后重试...",
                                        SERIAL_HANDSHAKE_RETRY_DELAY_SECS
                                    );
                                    std::thread::sleep(Duration::from_secs(
                                        SERIAL_HANDSHAKE_RETRY_DELAY_SECS,
                                    ));
                                }
                            }
                        }
                    }

                    warn!(
                        "[COM] 连续 {} 次失败，降级到 WinUSB 直连模式",
                        SERIAL_HANDSHAKE_RETRY
                    );
                    return self.fallback_to_winusb(context);
                }
                UsbBusDetectionResult::Unknown(driver_mfg) => {
                    warn!("[USB] 未知驱动: {}，尝试 WinUSB 直连", driver_mfg);
                    return self.fallback_to_winusb(context);
                }
                UsbBusDetectionResult::NotFound => {
                    trace!("[USB] 未找到 BROM 设备，继续等待...");
                    std::thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
                }
            }
        }
    }

    /// WinUSB 直连（降级路径：设备已装 WinUSB 驱动 / COM 口扫描失败）
    fn fallback_to_winusb(
        &mut self,
        context: &USB上下文,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("[USB] 尝试 WinUSB 直连...");
        info!("[USB] 等待 BROM 设备出现 (PID=0x0003, 无限等待)...");

        let usb_device = self.reconnect_loop(context, USB阶段::Brom)?;

        info!(
            "[USB] WinUSB 设备已打开: VID={:04x}, PID={:04x}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.阶段
        );

        // 构造 Preloader 并执行完整 BROM 握手
        let mut preloader = Preloader::new(Box::new(usb_device));
        if !preloader
            .init()
            .map_err(|e| format!("WinUSB BROM 握手失败: {}", e))?
        {
            return Err("WinUSB BROM 握手未完成".to_string());
        }
        info!(
            "{}",
            "[USB] BROM 握手成功（看门狗已关，HW code 已获取）"
                .green()
                .bold()
        );

        self.mode = DeviceMode::Brom;
        self.stage = USB阶段::Brom;
        Ok((preloader, DeviceMode::Brom))
    }

    /// COM 口前置握手 → 释放 → WinUSB 接管
    fn serial_handshake_and_switch(
        &self,
        port_name: &str,
        context: &USB上下文,
    ) -> Result<Preloader, String> {
        // 1. 打开串口
        let transport = SerialPortTransport::new(port_name, 115200)?;
        let mut serial_preloader = Preloader::new(Box::new(transport));

        // 2. 完整 init：握手 + 关看门狗 + 获取 hw_code + 设置 chip
        if !serial_preloader
            .init()
            .map_err(|e| format!("串口 init 失败: {}", e))?
        {
            return Err("串口握手失败".to_string());
        }

        let chip = serial_preloader.chip;
        info!("COM 口 init 成功，chip={:?}", chip.map(|c| c.hw_code));

        // 3. 释放 COM 口
        drop(serial_preloader);
        info!("COM 口已释放");

        // 4. wdi-rs 切换到 WinUSB（卸载 usbser.sys，安装 WinUSB）
        crate::连接管理::driver::switch_to_winusb()
            .map_err(|e| format!("切换 WinUSB 驱动失败: {}", e))?;

        // 5. 等待设备重枚举（驱动切换后设备会重新枚举）
        std::thread::sleep(Duration::from_secs(USB_REENUM_DELAY_SECS));

        // 6. libusb1-sys 打开设备（轮询 10 秒）
        let usb_device = self.reconnect_loop(context, USB阶段::Brom)?;
        info!(
            "WinUSB 打开成功: VID={:04X} PID={:04X}",
            usb_device.vid, usb_device.pid
        );

        // 7. 构造 Preloader（chip 已从 COM 口获取，brom_initialized=true）
        let mut preloader = Preloader::new(Box::new(usb_device));
        preloader.chip = chip;
        preloader.brom_initialized = true;

        Ok(preloader)
    }

    /// WinUSB 重连循环
    /// 无限等待直到设备出现（用户手动按组合键进入 BROM 模式可能需要几十秒到几分钟）
    pub fn reconnect_loop(
        &self,
        context: &USB上下文,
        target_stage: USB阶段,
    ) -> Result<USB通信::USB设备, String> {
        let mut retry = 0;

        info!(
            "[RECONNECT] scanning for stage={:?} (infinite wait)...",
            target_stage
        );

        let pids = match target_stage {
            USB阶段::Brom => vec![0x0003u16],
            USB阶段::Preloader => vec![0x2000u16],
            USB阶段::Da => vec![0x2001u16],
            USB阶段::未知 => vec![0x0003u16, 0x2000u16, 0x2001u16],
        };

        loop {
            retry += 1;

            for &pid in &pids {
                match USB通信::USB设备::按VID_PID打开(context, 0x0E8D, pid) {
                    Ok(device) => {
                        info!(
                            "[RECONNECT] success on attempt {} (stage={:?}, PID=0x{:04X})",
                            retry, device.阶段, device.pid
                        );
                        return Ok(device);
                    }
                    Err(e) => {
                        trace!(
                            "[RECONNECT] open_by_vid_pid failed for PID=0x{:04X}: {}",
                            pid, e
                        );
                    }
                }
            }

            if retry % RECONNECT_LOG_INTERVAL == 1 {
                trace!(
                    "[RECONNECT] retry {} (scanning {} PIDs)...",
                    retry,
                    pids.len()
                );
            }
            std::thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
        }
    }

    /// DA 加载后重连
    #[allow(dead_code)] // 预留：DA 加载后设备重枚举流程
    pub fn reconnect_after_da(&self, context: &USB上下文) -> Result<USB通信::USB设备, String> {
        info!("[DA] DA 加载完成，等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(USB_REENUM_DELAY_MS));

        info!("[DA] 尝试 BROM PID (0x0003)...");
        if let Ok(device) = self.try_quick_connect(context, 0x0E8D, 0x0003, 5) {
            info!(
                "[DA] 连接成功: VID=0x{:04X} PID=0x{:04X}",
                device.vid, device.pid
            );
            return Ok(device);
        }

        info!("[DA] BROM PID 失败，扫描所有已知 PID...");
        self.reconnect_loop(context, USB阶段::未知)
    }

    /// Kamakiri exploit 后重连
    #[allow(dead_code)] // 预留：Kamakiri2 exploit 后设备重枚举流程
    pub fn reconnect_after_kamakiri(&self, context: &USB上下文) -> Result<USB通信::USB设备, String> {
        info!("[KAMAKIRI] payload 已发送，等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(USB_REENUM_DELAY_MS));
        self.reconnect_loop(context, USB阶段::Brom)
    }

    /// USB reset 后重连
    #[allow(dead_code)] // 预留：USB reset 后设备重枚举流程
    pub fn reconnect_after_usb_reset(
        &self,
        context: &USB上下文,
    ) -> Result<USB通信::USB设备, String> {
        info!("[USB] USB reset detected, waiting for re-enumeration...");
        std::thread::sleep(Duration::from_millis(USB_REENUM_DELAY_MS));
        self.reconnect_loop(context, USB阶段::Brom)
    }

    /// 快速连接尝试
    #[allow(dead_code)] // 预留：DA 后快速重连场景（被 reconnect_after_da 内部调用）
    fn try_quick_connect(
        &self,
        context: &USB上下文,
        vid: u16,
        pid: u16,
        retries: usize,
    ) -> Result<USB通信::USB设备, String> {
        for _ in 1..=retries {
            match USB通信::USB设备::按VID_PID打开(context, vid, pid) {
                Ok(device) => return Ok(device),
                Err(_) => std::thread::sleep(Duration::from_millis(QUICK_CONNECT_INTERVAL_MS)),
            }
        }
        Err(format!("快速连接失败 ({} 次重试)", retries))
    }

    /// 获取当前连接模式
    #[allow(dead_code)] // 预留：查询当前连接状态（串口/USB）
    pub fn mode(&self) -> &DeviceMode {
        &self.mode
    }

    /// 获取当前 USB 阶段
    #[allow(dead_code)] // 预留：查询设备当前所处阶段（BROM/Preloader）
    pub fn stage(&self) -> &USB阶段 {
        &self.stage
    }

    /// 获取串口名称
    #[allow(dead_code)] // 预留：调试/日志输出当前使用的串口名
    pub fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }

    /// DA 会话复用入口：直接连接到已处于 DA 模式的设备（PID=0x2000）
    pub fn connect_to_da_mode(
        &mut self,
        context: &USB上下文,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("[DA_SESSION] 直接连接 DA 模式设备 (PID=0x2000)...");
        info!("[DA_SESSION] 等待 Preloader 设备出现 (PID=0x2000)...");

        let usb_device = self.reconnect_loop(context, USB阶段::Preloader)?;

        info!(
            "[DA_SESSION] DA 设备已连接: VID={:04X}, PID={:04X}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.阶段
        );

        let mut preloader = Preloader::new(Box::new(usb_device));
        preloader.is_preloader_mode = true;
        preloader.brom_initialized = true;

        // 从 .state 恢复 chip 配置（如果 .state 中有 hw_code）
        #[allow(clippy::collapsible_if)]
        if let Some(state) = crate::连接管理::SessionState::load() {
            if let Some(chip) = crate::config::CHIP_CONFIGS
                .iter()
                .find(|c| c.hw_code == state.hw_code)
            {
                preloader.chip = Some(*chip);
                info!(
                    "[DA_SESSION] 从 .state 恢复 chip 配置: HW code=0x{:04X}",
                    state.hw_code
                );
            }
        }

        self.mode = DeviceMode::Brom;
        self.stage = USB阶段::Preloader;

        info!("{}", "[DA_SESSION] DA 会话复用成功".green().bold());
        Ok((preloader, DeviceMode::Brom))
    }
}

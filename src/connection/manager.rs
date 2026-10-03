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

use crate::color::Colorize;
use crate::connection::driver::{UsbBusDetectionResult, detect_brom_driver_from_usb_bus};
use crate::preloader::{Preloader, SerialPortTransport};
use crate::system::config::WorkMode;
use crate::usb::{UsbContext, UsbStage};
use log::{error, info, trace, warn};
use std::time::Duration;

pub(crate) const RECONNECT_INTERVAL_MS: u64 = 200;
pub(crate) const RECONNECT_LOG_INTERVAL: u32 = 25;
// 串口握手重试参数仅供 `UsbBusDetectionResult::SerialPort` 分支使用，而该分支
// 仅 Windows 存在（非 Windows 由 nusb 直连探测，不产生 SerialPort），故同步门控。
#[cfg(target_os = "windows")]
const SERIAL_HANDSHAKE_RETRY: u32 = 2; // 从 3 次降至 2 次（串口握手成功率很高）
#[cfg(target_os = "windows")]
const SERIAL_HANDSHAKE_RETRY_DELAY_SECS: u64 = 1; // 保持 1s（串口设备需要时间重置）
const USB_REENUM_DELAY_SECS: u64 = 1; // 从 2s 降至 1s（WinUSB 驱动切换后设备重枚举很快）
/// 连续握手失败上限：超过此次数后删除 .state 并退出程序
const MAX_CONSECUTIVE_HANDSHAKE_FAILURES: u32 = 5;

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
    pub(crate) mode: DeviceMode,
    pub(crate) stage: UsbStage,
    pub(crate) port_name: Option<String>,
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
        work_mode: WorkMode,
    ) -> Result<(Preloader, DeviceMode), String> {
        if work_mode == WorkMode::Preloader {
            // --mode preloader：始终走 Preloader 串口握手 + 重载 DA。
            // Preloader 串口的 DA 不跨进程存活，进程退出即复位、
            // DA 已死，且 PID 恒为 0x2000，旧 fallback 仅凭 .state(da_loaded) 打开 0x2000 构造
            // 假 DA 会话 -> 误复用 -> 后续命令失败/卡死。故不再走任何 DA 会话复用。
            return self.smart_init_preloader(context, false);
        }

        if work_mode == WorkMode::Auto {
            info!("[AUTO] 自动检测模式：等待 BROM 或 Preloader 设备出现...");
            // auto 模式检测顺序：BROM(PID=0x0003) 优先，Preloader(PID=0x2000) 兜底。
            // 设备若处于 BROM 下载态应直接用 BROM，避免被 Preloader 分支抢先/忽略。
            loop {
                if crate::cancel::requested() {
                    return Err("用户取消等待".to_string());
                }

                // 1. 先检查 BROM 设备 (PID=0x0003)
                let detection_result = detect_brom_driver_from_usb_bus();
                match detection_result {
                    UsbBusDetectionResult::WinUsbReady => {
                        info!("{}", "[AUTO] 检测到 BROM WinUSB 设备".green().bold());
                        return self.fallback_to_winusb_with_retry(context, 0);
                    }
                    // `SerialPort` 变体仅由 Windows SetupAPI 驱动分类产生，
                    // 非 Windows 下该变体不存在，故同步门控此匹配分支。
                    #[cfg(target_os = "windows")]
                    UsbBusDetectionResult::SerialPort(ref port_name) => {
                        if !port_name.is_empty() {
                            info!("[AUTO] 检测到 BROM COM 口: {}", port_name);
                            match self.serial_handshake_and_switch(port_name, context) {
                                Ok(preloader) => {
                                    info!("{}", "[AUTO] BROM 串口握手成功".green().bold());
                                    self.mode = DeviceMode::Brom;
                                    self.stage = UsbStage::Brom;
                                    self.port_name = Some(port_name.clone());
                                    return Ok((preloader, DeviceMode::Brom));
                                }
                                Err(e) => {
                                    trace!("[AUTO] BROM 串口握手失败: {}", e);
                                }
                            }
                        }
                    }
                    // `Unknown` 变体同样仅由 Windows SetupAPI 驱动分类产生，同步门控。
                    #[cfg(target_os = "windows")]
                    UsbBusDetectionResult::Unknown(driver_mfg) => {
                        warn!(
                            "[AUTO] BROM 设备驱动未知: {}，主动安装 WinUSB 驱动...",
                            driver_mfg
                        );
                        if let Err(e) = crate::connection::driver::switch_to_winusb() {
                            warn!("[AUTO] WinUSB 驱动安装失败: {}，将继续重试", e);
                        }
                    }
                    _ => {}
                }

                // 2. 检查 Preloader VCOM (PID=0x2000)（仅在 BROM 未出现时尝试）
                if let Ok(ports) = serialport::available_ports() {
                    for p in &ports {
                        if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                            && info.vid == 0x0E8D
                            && info.pid == 0x2000
                        {
                            // 注意：本分支不再尝试“串口 DA 会话复用”。
                            //
                            // 根因（与 reconnect.rs smart_init_preloader 注释一致）：
                            // Preloader 串口的 DA 不跨进程存活——进程退出会触发设备复位、
                            // 重新枚举回 Preloader 握手态，DA 早已死掉。旧逻辑盲开 COM 口并
                            // 置 daext=true，会把 DA SHUTDOWN 等命令发往无 DA 的设备，导致
                            // 阶段 1 直接返回 0x40040005（典型报错：reboot 日志“SHUTDOWN
                            // 命令状态: 0x40040005”后 watchdog 兜底也失效）。
                            //
                            // 修正：统一改为每次重新 Preloader 握手 + 重载 DA（与
                            // --mode preloader 同一条已被验证可用的新鲜路径），不再有任何
                            // .state DA 会话复用（DA 不跨进程存活，PID 恒定导致误命中）。
                            //
                            // 所有模式均统一重新握手 + 重载 DA，不再走 .state DA 会话复用
                            // （DA 不跨进程存活，PID 恒定导致 device_online 误命中）。
                            info!(
                                "[AUTO] 检测到 Preloader 设备: {} (PID={:04X})",
                                p.port_name, info.pid
                            );
                            match self.preloader_serial_handshake(&p.port_name) {
                                Ok(preloader) => {
                                    self.mode = DeviceMode::Preloader;
                                    self.stage = UsbStage::Preloader;
                                    self.port_name = Some(p.port_name.clone());
                                    return Ok((preloader, DeviceMode::Preloader));
                                }
                                Err(e) => {
                                    warn!("[AUTO] Preloader 握手失败: {}", e);
                                }
                            }
                        }
                    }
                }

                std::thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
            }
        }

        info!("等待设备连接 (BROM: Vol+ + Vol- + Power)");

        // 启动诊断（一次性，不改变后续检测/连接流程）：用 nusb 枚举当前是否已有
        // 联发科 BROM 设备（VID=0x0E8D, PID=0x0003），便于在“设备已插入但驱动/阶段
        // 未就绪”时快速定位问题；后续循环仍以 detect_brom_driver_from_usb_bus 为准。
        if let Some((vid, pid, device_type)) = crate::usb::get_first_mtk_vid_pid() {
            info!(
                "[USB] 启动诊断：nusb 已枚举到 MTK 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
                vid, pid, device_type
            );
        }

        // 连续握手失败计数器：用于检测 DA 会话失效。
        // 累加只发生在 SerialPort 分支（仅 Windows 存在），故按平台区分是否需要 `mut`，
        // 避免非 Windows 下出现 unused_mut 警告。
        #[cfg(target_os = "windows")]
        let mut consecutive_handshake_failures: u32 = 0;
        #[cfg(not(target_os = "windows"))]
        let consecutive_handshake_failures: u32 = 0;

        // 无限等待设备出现
        loop {
            // Ctrl+C 检查：用户取消时退出等待
            if crate::cancel::requested() {
                return Err("用户取消等待".to_string());
            }

            let detection_result = detect_brom_driver_from_usb_bus();

            match detection_result {
                UsbBusDetectionResult::WinUsbReady => {
                    info!(
                        "{}",
                        "[USB] 检测到 WinUSB 驱动 (libwdi)，直接走 WinUSB 直连"
                            .green()
                            .bold()
                    );
                    return self
                        .fallback_to_winusb_with_retry(context, consecutive_handshake_failures);
                }
                // `SerialPort` 变体仅由 Windows SetupAPI 产生，非 Windows 下不存在，同步门控。
                #[cfg(target_os = "windows")]
                UsbBusDetectionResult::SerialPort(port_name) => {
                    if port_name.is_empty() {
                        // 设备在 USB 总线上且使用串口驱动，但通过注册表匹配不到 COM 口名
                        // 解决方案：枚举所有 COM 口，逐个尝试 BROM 握手
                        warn!("[COM] 注册表匹配不到 COM 口，枚举所有 COM 口逐个尝试握手...");

                        let all_ports =
                            crate::connection::driver::detect::enumerate_all_com_ports();
                        if all_ports.is_empty() {
                            warn!(
                                "[COM] 系统中没有任何 COM 口；当前仍是串口驱动，继续等待串口设备"
                            );
                            std::thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
                            continue;
                        }

                        info!(
                            "[COM] 系统中有 {} 个 COM 口: {:?}",
                            all_ports.len(),
                            all_ports
                        );

                        for port in &all_ports {
                            info!("[COM] 尝试 {} ...", port);
                            match self.serial_handshake_and_switch(port, context) {
                                Ok(preloader) => {
                                    info!(
                                        "{}",
                                        format!("[COM] {} 串口握手+WinUSB切换成功", port)
                                            .green()
                                            .bold()
                                    );
                                    self.mode = DeviceMode::Brom;
                                    self.stage = UsbStage::Brom;
                                    self.port_name = Some(port.clone());
                                    return Ok((preloader, DeviceMode::Brom));
                                }
                                Err(e) => {
                                    trace!("[COM] {} 不是 BROM 设备: {}", port, e);
                                }
                            }
                        }

                        warn!(
                            "[COM] 所有 COM 口均握手失败；当前仍是串口驱动，不进入 WinUSB 直连，继续等待"
                        );
                        consecutive_handshake_failures += all_ports.len() as u32;
                        std::thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
                        continue;
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
                                self.stage = UsbStage::Brom;
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
                        "[COM] 连续 {} 次失败；当前仍是串口驱动，不进入 WinUSB 直连，继续等待",
                        SERIAL_HANDSHAKE_RETRY
                    );
                    consecutive_handshake_failures += SERIAL_HANDSHAKE_RETRY;
                    std::thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
                    continue;
                }
                // `Unknown` 变体同样仅由 Windows SetupAPI 产生，非 Windows 不存在，同步门控。
                #[cfg(target_os = "windows")]
                UsbBusDetectionResult::Unknown(driver_mfg) => {
                    warn!(
                        "[USB] 未知/未安装驱动: {}，主动安装 WinUSB 驱动...",
                        driver_mfg
                    );
                    // Unknown 表示设备未绑定 WinUSB（可能根本没装驱动），
                    // 必须安装而非直接尝试 libusb 打开（否则永远打不开）。
                    // switch_to_winusb 基于硬件 ID 强制安装，装完设备重枚举，
                    // 下一轮循环检测为 WinUsbReady 后自动直连。
                    if let Err(e) = crate::connection::driver::switch_to_winusb() {
                        warn!("[USB] WinUSB 驱动安装失败: {}，将继续重试", e);
                    }
                }
                UsbBusDetectionResult::NotFound => {
                    trace!("[USB] 未找到 BROM 设备，尝试枚举 COM 口...");

                    // libusb 可能看不到使用串口驱动的设备，直接枚举 COM 口尝试
                    let all_ports = crate::connection::driver::detect::enumerate_all_com_ports();
                    if !all_ports.is_empty() {
                        info!(
                            "[COM] 系统中有 {} 个 COM 口: {:?}",
                            all_ports.len(),
                            all_ports
                        );

                        for port in &all_ports {
                            info!("[COM] 尝试 {} ...", port);
                            match self.serial_handshake_and_switch(port, context) {
                                Ok(preloader) => {
                                    info!(
                                        "{}",
                                        format!("[COM] {} 串口握手+WinUSB切换成功", port)
                                            .green()
                                            .bold()
                                    );
                                    self.mode = DeviceMode::Brom;
                                    self.stage = UsbStage::Brom;
                                    self.port_name = Some(port.clone());
                                    return Ok((preloader, DeviceMode::Brom));
                                }
                                Err(e) => {
                                    trace!("[COM] {} 不是 BROM 设备: {}", port, e);
                                }
                            }
                        }

                        warn!("[COM] 所有 COM 口均握手失败，继续等待设备...");
                    }

                    std::thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
                }
            }
        }
    }

    /// WinUSB 直连（降级路径：设备已装 WinUSB 驱动 / COM 口扫描失败）
    fn fallback_to_winusb(
        &mut self,
        context: &UsbContext,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("[USB] 尝试 WinUSB 直连...");
        info!("[USB] 等待 BROM 设备出现 (PID=0x0003, 无限等待)...");

        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;

        info!(
            "[USB] WinUSB 设备已打开: VID={:04x}, PID={:04x}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.stage
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
        self.stage = UsbStage::Brom;
        Ok((preloader, DeviceMode::Brom))
    }

    /// WinUSB 直连（带握手失败计数）
    /// 如果累计握手失败次数达到上限，删除 .state 并退出程序
    fn fallback_to_winusb_with_retry(
        &mut self,
        context: &UsbContext,
        consecutive_failures: u32,
    ) -> Result<(Preloader, DeviceMode), String> {
        match self.fallback_to_winusb(context) {
            Ok(result) => Ok(result),
            Err(e) => {
                // 累计失败次数 = 之前串口握手失败 + 本次 WinUSB 失败(计1次)
                let total_failures = consecutive_failures + 1;
                if total_failures >= MAX_CONSECUTIVE_HANDSHAKE_FAILURES {
                    error!(
                        "连续 {} 次握手失败，DA 会话可能已失效。已删除会话状态文件，请重启设备后重试。",
                        total_failures
                    );
                    crate::connection::reset_session();
                    std::process::exit(1);
                }
                Err(e)
            }
        }
    }

    /// COM 口前置握手 → 释放 → WinUSB 接管
    fn serial_handshake_and_switch(
        &self,
        port_name: &str,
        context: &UsbContext,
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
        crate::connection::driver::switch_to_winusb()
            .map_err(|e| format!("切换 WinUSB 驱动失败: {}", e))?;

        // 5. 等待设备重枚举（驱动切换后设备会重新枚举）
        std::thread::sleep(Duration::from_secs(USB_REENUM_DELAY_SECS));

        // 6. libusb1-sys 打开设备（轮询 10 秒）
        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;
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
}

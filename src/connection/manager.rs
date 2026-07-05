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

use crate::connection::driver::{UsbBusDetectionResult, detect_brom_driver_from_usb_bus};
use crate::preloader::{Preloader, SerialPortTransport};
use crate::system::config::工作模式;
use crate::usb::{USB上下文, USB阶段};
use colored::Colorize;
use log::{error, info, trace, warn};
use std::time::Duration;

pub(crate) const RECONNECT_INTERVAL_MS: u64 = 200;
pub(crate) const RECONNECT_LOG_INTERVAL: u32 = 25;
const SERIAL_HANDSHAKE_RETRY: u32 = 3;
const SERIAL_HANDSHAKE_RETRY_DELAY_SECS: u64 = 2;
const USB_REENUM_DELAY_SECS: u64 = 3;
pub(crate) const USB_REENUM_DELAY_MS: u64 = 500;
pub(crate) const QUICK_CONNECT_INTERVAL_MS: u64 = 200;
/// 连续握手失败上限：超过此次数后删除 .state 并退出程序
const MAX_CONSECUTIVE_HANDSHAKE_FAILURES: u32 = 5;

#[derive(Debug, PartialEq, Eq)]
enum SerialFailureAction {
    RetrySerial,
    TryWinUsb,
}

fn serial_failure_action(confirmed_serial_driver: bool) -> SerialFailureAction {
    if confirmed_serial_driver {
        SerialFailureAction::RetrySerial
    } else {
        SerialFailureAction::TryWinUsb
    }
}

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
    pub(crate) stage: USB阶段,
    pub(crate) port_name: Option<String>,
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
    pub fn smart_init(
        &mut self,
        context: &USB上下文,
        工作模式: 工作模式,
    ) -> Result<(Preloader, DeviceMode), String> {
        if 工作模式 == 工作模式::Preloader {
            return self.smart_init_preloader(context);
        }

        info!("等待设备连接 (BROM: Vol+ + Vol- + Power)");

        // 连续握手失败计数器：用于检测 DA 会话失效
        let mut consecutive_handshake_failures: u32 = 0;

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
                    return self
                        .fallback_to_winusb_with_retry(context, consecutive_handshake_failures);
                }
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
                                    self.stage = USB阶段::Brom;
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
                        let _ = serial_failure_action(true);
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
                        "[COM] 连续 {} 次失败；当前仍是串口驱动，不进入 WinUSB 直连，继续等待",
                        SERIAL_HANDSHAKE_RETRY
                    );
                    consecutive_handshake_failures += SERIAL_HANDSHAKE_RETRY;
                    let _ = serial_failure_action(true);
                    std::thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
                    continue;
                }
                UsbBusDetectionResult::Unknown(driver_mfg) => {
                    warn!("[USB] 未知驱动: {}，尝试 WinUSB 直连", driver_mfg);
                    return self
                        .fallback_to_winusb_with_retry(context, consecutive_handshake_failures);
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
                                    self.stage = USB阶段::Brom;
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

    /// WinUSB 直连（带握手失败计数）
    /// 如果累计握手失败次数达到上限，删除 .state 并退出程序
    fn fallback_to_winusb_with_retry(
        &mut self,
        context: &USB上下文,
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
        crate::connection::driver::switch_to_winusb()
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_serial_driver_must_not_fallback_to_winusb_direct() {
        assert_eq!(
            serial_failure_action(true),
            SerialFailureAction::RetrySerial
        );
    }

    #[test]
    fn non_serial_path_may_try_winusb_direct() {
        assert_eq!(serial_failure_action(false), SerialFailureAction::TryWinUsb);
    }
}

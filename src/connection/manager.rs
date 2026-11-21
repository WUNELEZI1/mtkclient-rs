//! Device connection manager.
//!
//! Picks the best path (USB or serial) automatically and builds a [`Preloader`]:
//! 1. Wait indefinitely for the device to appear (USB bus detection)
//! 2. WinUSB ready → connect through WinUSB directly
//! 3. Serial driver → open the port, handshake, then switch to WinUSB
//! 4. Three serial failures → fall back to a direct WinUSB connection
//!
//! ```text
//! STEP 1: COM scan → handshake → disable watchdog → fetch hw_code (up to 3 times)
//! STEP 2: direct WinUSB connection (fallback, skips the handshake)
//! ```

use crate::color::Colorize;
use crate::connection::driver::{UsbBusDetectionResult, detect_brom_driver_from_usb_bus};
use crate::preloader::{Preloader, SerialPortTransport};
use crate::system::config::WorkMode;
use crate::usb::{UsbContext, UsbStage};
use log::{error, info, trace, warn};
use std::time::Duration;

#[path = "manager/decision.rs"]
mod decision;
use decision::{ConnectedState, SmartInitDecision};

pub(crate) const RECONNECT_INTERVAL_MS: u64 = 200;
pub(crate) const RECONNECT_LOG_INTERVAL: u32 = 25;
// Serial handshake retry settings are used only by the `UsbBusDetectionResult::SerialPort`
// branch, which exists only on Windows (elsewhere nusb probe never yields SerialPort), so gate likewise.
#[cfg(target_os = "windows")]
const SERIAL_HANDSHAKE_RETRY: u32 = 2; // lowered from 3 to 2 (serial handshake rarely fails)
#[cfg(target_os = "windows")]
const SERIAL_HANDSHAKE_RETRY_DELAY_SECS: u64 = 1; // kept at 1s (serial devices need time to reset)
const USB_REENUM_DELAY_SECS: u64 = 1; // lowered from 2s to 1s (re-enumeration is fast after the WinUSB switch)
/// Max consecutive handshake failures: beyond this, delete .state and exit.
const MAX_CONSECUTIVE_HANDSHAKE_FAILURES: u32 = 5;

/// Device mode
#[derive(Debug, PartialEq, Clone)]
pub enum DeviceMode {
    /// BROM mode (serial or WinUSB)
    Brom,
    /// Preloader mode (a later step is needed to reach BROM)
    Preloader,
}

/// Unified connection manager
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

    /// Unified device initialization entry point
    pub fn smart_init(
        &mut self,
        context: &UsbContext,
        work_mode: WorkMode,
    ) -> Result<(Preloader, DeviceMode), String> {
        if work_mode == WorkMode::Preloader {
            // --mode preloader: always run the Preloader serial handshake and reload the DA.
            // A DA on a Preloader serial port does not survive process exit: the device resets,
            // the DA is gone, while the PID stays 0x2000. The old fallback opened 0x2000 using
            // only .state(da_loaded) and built a fake DA session -> wrong reuse -> later commands
            // failed or hung. Hence no DA session reuse is attempted any more.
            return self.smart_init_preloader(context, false);
        }

        if work_mode == WorkMode::Auto {
            info!("{}", crate::i18n::t("log_auto_detect"));
            // auto detection order: BROM (PID=0x0003) first, Preloader (PID=0x2000) as fallback.
            // A device in BROM download mode must be used as BROM, so the Preloader branch
            // must not grab or mask it.
            loop {
                if crate::cancel::requested() {
                    return Err("用户取消等待".to_string());
                }

                // 1. Check the BROM device first (PID=0x0003)
                let detection_result = detect_brom_driver_from_usb_bus();
                match detection_result {
                    UsbBusDetectionResult::WinUsbReady => {
                        info!("{}", crate::i18n::t("log_brom_usb_found").green().bold());
                        return self.fallback_to_winusb_with_retry(context, 0);
                    }
                    // The `SerialPort` variant is produced only by Windows SetupAPI driver
                    // classification and does not exist elsewhere, so gate this match arm too.
                    #[cfg(target_os = "windows")]
                    UsbBusDetectionResult::SerialPort(ref port_name) => {
                        if !port_name.is_empty() {
                            info!("[AUTO] 检测到 BROM COM 口: {}", port_name);
                            match self.serial_handshake_and_switch(port_name, context) {
                                Ok(preloader) => {
                                    info!("{}", crate::i18n::t("log_brom_com_ok").green().bold());
                                    ConnectedState::new(DeviceMode::Brom, Some(port_name.clone()))
                                        .apply(self);
                                    return Ok((preloader, DeviceMode::Brom));
                                }
                                Err(e) => {
                                    trace!("[AUTO] BROM 串口握手失败: {}", e);
                                }
                            }
                        }
                    }
                    // The `Unknown` variant likewise comes only from Windows SetupAPI classification; gate it too.
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

                // 2. Check the Preloader VCOM (PID=0x2000); tried only when BROM did not appear
                if let Ok(ports) = serialport::available_ports() {
                    for p in &ports {
                        if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                            && info.vid == 0x0E8D
                            && info.pid == 0x2000
                        {
                            // Note: this branch no longer attempts "serial DA session reuse".
                            //
                            // Root cause (same as the smart_init_preloader comment in reconnect.rs):
                            // a DA on a Preloader serial port does not survive process exit — exiting
                            // resets the device, which re-enumerates back to the Preloader handshake
                            // state, and the DA is long dead. The old logic blindly opened the COM port
                            // and set daext=true, sending commands such as DA SHUTDOWN to a device
                            // without a DA, so stage 1 returned 0x40040005 directly (typically the
                            // reboot log "SHUTDOWN command status: 0x40040005", after which even the
                            // watchdog fallback failed).
                            //
                            // Fix: always redo the Preloader handshake and reload the DA (the same
                            // proven fresh path as --mode preloader); no .state DA session reuse remains
                            // (a DA does not survive process exit, and the constant PID caused false
                            // matches).
                            //
                            // All modes redo the handshake and reload the DA; no .state DA session
                            // reuse (a DA does not survive process exit, and the constant PID made
                            // device_online match falsely).
                            info!(
                                "[AUTO] 检测到 Preloader 设备: {} (PID={:04X})",
                                p.port_name, info.pid
                            );
                            match self.preloader_serial_handshake(&p.port_name) {
                                Ok(preloader) => {
                                    ConnectedState::new(
                                        DeviceMode::Preloader,
                                        Some(p.port_name.clone()),
                                    )
                                    .apply(self);
                                    return Ok((preloader, DeviceMode::Preloader));
                                }
                                Err(e) => {
                                    warn!("[AUTO] Preloader 握手失败: {}", e);
                                }
                            }
                        }
                    }
                }

                std::thread::sleep(SmartInitDecision::ReconnectInterval.sleep_duration());
            }
        }

        info!("{}", crate::i18n::t("log_wait_device"));

        // Startup diagnostic (one-shot, does not alter the detection/connection flow): use nusb
        // to check whether an MTK BROM device (VID=0x0E8D, PID=0x0003) is already present, which
        // helps diagnose "device plugged in but driver/stage not ready"; the loop below still
        // relies on detect_brom_driver_from_usb_bus.
        if let Some((vid, pid, device_type)) = crate::usb::get_first_mtk_vid_pid() {
            info!(
                "[USB] 启动诊断：nusb 已枚举到 MTK 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
                vid, pid, device_type
            );
        }

        // Consecutive handshake failure counter: detects a dead DA session.
        // Incrementing happens only in the SerialPort branch (Windows only), so `mut` is gated
        // per platform to avoid an unused_mut warning elsewhere.
        #[cfg(target_os = "windows")]
        let mut consecutive_handshake_failures: u32 = 0;
        #[cfg(not(target_os = "windows"))]
        let consecutive_handshake_failures: u32 = 0;

        // Wait indefinitely for the device to appear
        loop {
            // Ctrl+C check: stop waiting when the user cancels
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
                // The `SerialPort` variant comes only from Windows SetupAPI and does not exist elsewhere; gate it.
                #[cfg(target_os = "windows")]
                UsbBusDetectionResult::SerialPort(port_name) => {
                    if port_name.is_empty() {
                        // The device is on the USB bus with a serial driver, but no COM port name
                        // matched via the registry. Solution: enumerate all COM ports and try the
                        // BROM handshake on each.
                        warn!("[COM] 注册表匹配不到 COM 口，枚举所有 COM 口逐个尝试握手...");

                        let all_ports =
                            crate::connection::driver::detect::enumerate_all_com_ports();
                        if all_ports.is_empty() {
                            warn!(
                                "[COM] 系统中没有任何 COM 口；当前仍是串口驱动，继续等待串口设备"
                            );
                            std::thread::sleep(
                                SmartInitDecision::ReconnectInterval.sleep_duration(),
                            );
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
                                    ConnectedState::new(DeviceMode::Brom, Some(port.clone()))
                                        .apply(self);
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
                        std::thread::sleep(SmartInitDecision::ReconnectInterval.sleep_duration());
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
                                ConnectedState::new(DeviceMode::Brom, Some(port_name)).apply(self);
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
                                    std::thread::sleep(
                                        SmartInitDecision::SerialRetryDelay.sleep_duration(),
                                    );
                                }
                            }
                        }
                    }

                    warn!(
                        "[COM] 连续 {} 次失败；当前仍是串口驱动，不进入 WinUSB 直连，继续等待",
                        SERIAL_HANDSHAKE_RETRY
                    );
                    consecutive_handshake_failures += SERIAL_HANDSHAKE_RETRY;
                    std::thread::sleep(SmartInitDecision::ReconnectInterval.sleep_duration());
                    continue;
                }
                // The `Unknown` variant likewise comes only from Windows SetupAPI; gate it as well.
                #[cfg(target_os = "windows")]
                UsbBusDetectionResult::Unknown(driver_mfg) => {
                    warn!(
                        "[USB] 未知/未安装驱动: {}，主动安装 WinUSB 驱动...",
                        driver_mfg
                    );
                    // Unknown means the device is not bound to WinUSB (possibly no driver at all),
                    // so install instead of trying to open it via libusb (which would never work).
                    // switch_to_winusb force-installs by hardware ID; the device re-enumerates and
                    // the next loop iteration sees WinUsbReady and connects directly.
                    if let Err(e) = crate::connection::driver::switch_to_winusb() {
                        warn!("[USB] WinUSB 驱动安装失败: {}，将继续重试", e);
                    }
                }
                UsbBusDetectionResult::NotFound => {
                    trace!("[USB] 未找到 BROM 设备，尝试枚举 COM 口...");

                    // libusb may not see devices using the serial driver, so enumerate COM ports directly
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
                                    ConnectedState::new(DeviceMode::Brom, Some(port.clone()))
                                        .apply(self);
                                    return Ok((preloader, DeviceMode::Brom));
                                }
                                Err(e) => {
                                    trace!("[COM] {} 不是 BROM 设备: {}", port, e);
                                }
                            }
                        }

                        warn!("[COM] 所有 COM 口均握手失败，继续等待设备...");
                    }

                    std::thread::sleep(SmartInitDecision::ReconnectInterval.sleep_duration());
                }
            }
        }
    }

    /// Direct WinUSB connection (fallback: WinUSB driver already installed / COM scan failed)
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

        // Build the Preloader and run the full BROM handshake
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

    /// Direct WinUSB connection (with handshake failure counting).
    /// Once the accumulated failures reach the limit, delete .state and exit.
    fn fallback_to_winusb_with_retry(
        &mut self,
        context: &UsbContext,
        consecutive_failures: u32,
    ) -> Result<(Preloader, DeviceMode), String> {
        match self.fallback_to_winusb(context) {
            Ok(result) => Ok(result),
            Err(e) => {
                // Total failures = earlier serial handshake failures + this WinUSB failure (counted as 1)
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

    /// COM port pre-handshake → release → WinUSB takes over
    fn serial_handshake_and_switch(
        &self,
        port_name: &str,
        context: &UsbContext,
    ) -> Result<Preloader, String> {
        // 1. Open the serial port
        let transport = SerialPortTransport::new(port_name, 115200)?;
        let mut serial_preloader = Preloader::new(Box::new(transport));

        // 2. Full init: handshake + disable watchdog + fetch hw_code + set chip
        if !serial_preloader
            .init()
            .map_err(|e| format!("串口 init 失败: {}", e))?
        {
            return Err("串口握手失败".to_string());
        }

        let chip = serial_preloader.chip;
        info!("COM 口 init 成功，chip={:?}", chip.map(|c| c.hw_code));

        // 3. Release the COM port
        drop(serial_preloader);
        info!("COM 口已释放");

        // 4. wdi-rs switches to WinUSB (uninstall usbser.sys, install WinUSB)
        crate::connection::driver::switch_to_winusb()
            .map_err(|e| format!("切换 WinUSB 驱动失败: {}", e))?;

        // 5. Wait for re-enumeration (the device re-enumerates after the driver switch)
        std::thread::sleep(Duration::from_secs(USB_REENUM_DELAY_SECS));

        // 6. Open the device via libusb1-sys (poll for 10 seconds)
        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;
        info!(
            "WinUSB 打开成功: VID={:04X} PID={:04X}",
            usb_device.vid, usb_device.pid
        );

        // 7. Build the Preloader (chip already obtained from the COM port, brom_initialized=true)
        let mut preloader = Preloader::new(Box::new(usb_device));
        preloader.chip = chip;
        preloader.brom_initialized = true;

        Ok(preloader)
    }
}

#[cfg(test)]
#[path = "manager/tests.rs"]
mod tests;

use crate::color::Colorize;
use crate::preloader::Preloader;
use crate::usb;
use crate::usb::{UsbContext, UsbStage};
use log::{debug, info, trace, warn};
use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::manager::{ConnectionManager, DeviceMode};

use crate::preloader::SerialPortTransport;

impl ConnectionManager {
    /// WinUSB reconnect loop
    /// Waits indefinitely until the device appears (entering BROM manually can take seconds to minutes)
    pub fn reconnect_loop(
        &self,
        context: &UsbContext,
        target_stage: UsbStage,
    ) -> Result<usb::UsbDevice, String> {
        let mut retry = 0;
        let mut context_refreshed = false;
        // Overall wait cap: re-enumeration usually takes < 2s; a timeout means the device never
        // reached the target stage, avoiding an endless hang.
        const RECONNECT_TOTAL_WAIT_SECS: u64 = 60;
        let start = Instant::now();

        info!(
            "[RECONNECT] scanning for stage={:?} (最多等待 {}s)...",
            target_stage, RECONNECT_TOTAL_WAIT_SECS
        );

        let pids = match target_stage {
            UsbStage::Brom => vec![0x0003u16],
            UsbStage::Preloader => vec![0x2000u16],
            UsbStage::Da => vec![0x2001u16],
            UsbStage::Unknown => vec![0x0003u16, 0x2000u16, 0x2001u16],
        };

        loop {
            retry += 1;

            // Ctrl+C check: leave the wait when the user cancels
            if crate::cancel::requested() {
                return Err("用户取消等待".to_string());
            }

            // Overall wait timeout: avoid hanging in port scanning if the device never re-enumerates
            if start.elapsed().as_secs() >= RECONNECT_TOTAL_WAIT_SECS {
                return Err(format!(
                    "[RECONNECT] 等待设备重连超时 ({}s)：未检测到 {:?}。请确认设备已回到目标模式后重试。",
                    RECONNECT_TOTAL_WAIT_SECS, target_stage
                ));
            }

            for &pid in &pids {
                match usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, pid) {
                    Ok(device) => {
                        info!(
                            "[RECONNECT] success on attempt {} (stage={:?}, PID=0x{:04X})",
                            retry, device.stage, device.pid
                        );
                        return Ok(device);
                    }
                    Err(e) => {
                        trace!(
                            "[RECONNECT] open_by_vid_pid failed for PID=0x{:04X}: {}（诊断: {}）",
                            pid,
                            e,
                            crate::usb::diag::classify_libusb_error(&e)
                        );
                    }
                }
            }

            // After a driver switch, the old libusb context may fail to enumerate new devices;
            // every 50 retries (~10s) try opening with a fresh context
            if retry % 50 == 0 && !context_refreshed {
                trace!("[RECONNECT] 尝试刷新 libusb context...");
                if let Ok(new_ctx) = UsbContext::new() {
                    for &pid in &pids {
                        if let Ok(device) = usb::UsbDevice::open_by_vid_pid(&new_ctx, 0x0E8D, pid) {
                            info!("[RECONNECT] 使用新 context 成功连接 (attempt {})", retry);
                            return Ok(device);
                        }
                    }
                }
                context_refreshed = true;
            }

            if retry % super::manager::RECONNECT_LOG_INTERVAL == 1 {
                trace!(
                    "[RECONNECT] retry {} (scanning {} PIDs)...",
                    retry,
                    pids.len()
                );
            }
            std::thread::sleep(Duration::from_millis(super::manager::RECONNECT_INTERVAL_MS));
        }
    }

    // Note: the former reconnect_after_da / reconnect_after_kamakiri / reconnect_after_usb_reset /
    // try_quick_connect / mode / stage / port_name interfaces were historical placeholders (DA session
    // reuse is fully disabled, see comments in main.rs / session.rs) with no real call sites on any
    // platform, and were removed under the "delete explicitly dead code" rule; their dependent
    // USB_REENUM_DELAY_MS / QUICK_CONNECT_INTERVAL_MS constants were also cleaned from manager.rs.

    /// Preloader mode init: wait for Preloader VCOM (PID=0x2000), return right after the handshake
    /// allow_brom_fallback: whether to fall back to BROM when Preloader is not found (true for --mode auto, false for --mode preloader)
    ///
    /// Note: DA session reuse is fully disabled (see device_online in main.rs / session.rs); there is no allow_da_fallback branch.
    pub(crate) fn smart_init_preloader(
        &mut self,
        context: &UsbContext,
        allow_brom_fallback: bool,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("等待 Preloader VCOM 设备连接 (PID=0x2000)，无需按任何按键...");

        // 0. DA session reuse is disabled in Preloader mode (no longer attempted).
        //
        // Root cause: on process exit the Preloader serial port (VCOM, PID=0x2000) drops DTR, which
        // resets the device and it re-enumerates into the same PID=0x2000 Preloader handshake state.
        // DA is long dead by then, but the PID is identical to that of "DA mode serial", so PID alone
        // cannot distinguish them. The old logic blindly reused .state(da_loaded) + PID=0x2000 and sent
        // DA SHUTDOWN to a device in Preloader handshake state (no DA); the misread status happened to
        // return 0 → false success → the device did nothing (typical symptom: the reboot log says
        // "DA SHUTDOWN succeeded" but the device does not reboot).
        //
        // Since the Preloader serial DA does not survive across processes, session reuse is inherently
        // useless in this mode; always re-handshake and reload DA so traffic goes to a live, verified
        // DA link (the same fresh path already proven by printgpt in run 3).
        // Note: BROM-mode DA also does not survive across processes (after DA load the PID is still
        // 0x0003, and after a device reset it returns to 0x0003 with DA dead), so BROM also skips
        // .state DA session reuse — smart_init(Brom) always re-handshakes and reloads DA.
        // This only documents the historical design; all reuse logic is disabled.

        let mut retry_count = 0u32;
        let mut reported_ports = HashSet::new();
        const PRELOADER_MAX_RETRY: u32 = 50; // ~10s (50 * 200ms)
        // Wait policy (matching the brom path in the manager.rs main loop, exit only on Ctrl+C):
        // 1) When no Preloader port is detected, wait indefinitely — entering the Preloader handshake
        //    manually may take seconds to minutes, so there should be no 30s cap (bug #1 fix: wait forever).
        // 2) When the port appears but the handshake keeps failing (device in DA mode rather than Preloader
        //    handshake), endless retries are useless; give clear guidance once the window expires
        //    (bug #3 fix: avoid "stuck after the port appears").
        const PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS: u64 = 25;
        let mut handshake_fail_start: Option<Instant> = None;

        loop {
            retry_count += 1;

            // Ctrl+C check: leave the wait when the user cancels
            if crate::cancel::requested() {
                return Err("用户取消等待".to_string());
            }
            // Note: Preloader mode has no overall wait cap (matching the brom path); wait indefinitely when no port is detected.

            // 1. Enumerate COM ports and look for the Preloader VCOM with PID=0x2000
            if let Ok(ports) = serialport::available_ports() {
                for p in &ports {
                    if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                        && info.vid == 0x0E8D
                        && info.pid == 0x2000
                    {
                        if reported_ports.insert(p.port_name.clone()) {
                            info!(
                                "[PRELOADER] 发现 Preloader COM 口: {} (VID={:04X} PID={:04X})",
                                p.port_name, info.vid, info.pid
                            );
                        } else {
                            trace!(
                                "[PRELOADER] 发现 Preloader COM 口: {} (VID={:04X} PID={:04X})",
                                p.port_name, info.vid, info.pid
                            );
                        }
                        match self.preloader_serial_handshake(&p.port_name) {
                            Ok(preloader) => {
                                self.mode = DeviceMode::Preloader;
                                self.stage = UsbStage::Preloader;
                                self.port_name = Some(p.port_name.clone());
                                return Ok((preloader, DeviceMode::Preloader));
                            }
                            Err(e) => {
                                warn!("[PRELOADER] {} 握手失败: {}", p.port_name, e);
                                // Port present but handshake keeps failing: the device is likely in DA mode
                                // (not Preloader handshake); endless retries will not recover it, so give
                                // clear guidance once the window expires (bug #3 fix: avoid "stuck after the port appears").
                                match handshake_fail_start {
                                    None => handshake_fail_start = Some(Instant::now()),
                                    Some(t)
                                        if t.elapsed().as_secs()
                                            >= PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS =>
                                    {
                                        return Err(format!(
                                            "已检测到 Preloader 端口 {} 但握手持续失败 ({}s)：设备可能处于 DA 模式而非 Preloader 握手态，或串口被其他程序占用。请断开重连设备（使其回到 Preloader 握手态），或改用 --mode brom。",
                                            p.port_name, PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS
                                        ));
                                    }
                                    Some(_) => {}
                                }
                            }
                        }
                    }
                }
            }

            // 2. Serial port not found; try WinUSB (PID=0x2000)
            if let Ok(usb_device) = usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x2000) {
                info!(
                    "[PRELOADER] WinUSB 设备已连接: VID={:04X} PID={:04X}",
                    usb_device.vid, usb_device.pid
                );
                let mut preloader = Preloader::new(Box::new(usb_device));
                match preloader.init_preloader() {
                    Ok(true) => {
                        self.mode = DeviceMode::Preloader;
                        self.stage = UsbStage::Preloader;
                        return Ok((preloader, DeviceMode::Preloader));
                    }
                    Ok(false) => {
                        warn!("[PRELOADER] init 未成功，继续等待...");
                    }
                    Err(e) => {
                        warn!("[PRELOADER] init 失败: {}，继续等待...", e);
                        // Port present but init keeps failing: same as serial handshake failure; give clear guidance after the window.
                        match handshake_fail_start {
                            None => handshake_fail_start = Some(Instant::now()),
                            Some(t)
                                if t.elapsed().as_secs()
                                    >= PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS =>
                            {
                                return Err(format!(
                                    "已检测到 Preloader WinUSB 设备 (PID=0x2000) 但 init 持续失败 ({}s)：设备可能处于 DA 模式而非 Preloader 握手态。请断开重连设备，或改用 --mode brom。",
                                    PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS
                                ));
                            }
                            Some(_) => {}
                        }
                    }
                }
            }

            // 3. Preloader device still not found after the given number of attempts
            if retry_count >= PRELOADER_MAX_RETRY {
                if allow_brom_fallback {
                    trace!(
                        "[PRELOADER] 等待 {} 次 (约 {} 秒) 未找到 Preloader 设备，尝试检测 BROM 设备...",
                        retry_count,
                        (retry_count * 200) / 1000
                    );

                    // Detect a BROM COM port
                    if let Ok(ports) = serialport::available_ports() {
                        for p in &ports {
                            if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                                && info.vid == 0x0E8D
                                && info.pid == 0x0003
                            {
                                warn!(
                                    "[PRELOADER] 检测到 BROM COM 口: {}，自动切换到 BROM 模式",
                                    p.port_name
                                );
                                return self
                                    .smart_init(context, crate::system::config::WorkMode::Brom);
                            }
                        }
                    }

                    // Detect a BROM WinUSB device
                    if let Ok(_usb_device) =
                        usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x0003)
                    {
                        warn!(
                            "[PRELOADER] 检测到 BROM WinUSB 设备 (PID=0x0003)，自动切换到 BROM 模式"
                        );
                        return self.smart_init(context, crate::system::config::WorkMode::Brom);
                    }

                    trace!("[PRELOADER] 未检测到 BROM 设备，继续等待 Preloader 设备...");
                } else {
                    trace!(
                        "[PRELOADER] 等待 {} 次 (约 {} 秒) 未找到 Preloader 设备，继续等待...",
                        retry_count,
                        (retry_count * 200) / 1000
                    );
                }

                // Reset the counter and keep waiting (give the user more time)
                retry_count = 0;
            }

            std::thread::sleep(Duration::from_millis(super::manager::RECONNECT_INTERVAL_MS));
        }
    }

    /// Preloader serial handshake: open the COM port → init_preloader
    pub(crate) fn preloader_serial_handshake(&self, port_name: &str) -> Result<Preloader, String> {
        // Must use open_raw instead of new: new first calls verify_port_exists (open→close the COM port),
        // and opening the serial port clears the Preloader "READY" sync signal from the receive buffer;
        // the Preloader continuously sends READY while in handshake state (flasher success log
        // 0x52/45/41/44/59 = "READY"), and the open-close in verify consumes those bytes, so the
        // following handshake reports "no response bytes received".
        // Matching the working approach in preloader_boot_mode.rs (open_raw does not clear the buffer).
        let transport = SerialPortTransport::open_raw(port_name, 115200)?;
        let mut preloader = Preloader::new(Box::new(transport));
        if !preloader
            .init_preloader()
            .map_err(|e| format!("串口 init_preloader 失败: {}", e))?
        {
            return Err("串口 Preloader 握手失败".to_string());
        }
        debug!(
            "{}",
            format!("[PRELOADER] {} 握手成功", port_name).green().bold()
        );
        Ok(preloader)
    }
}

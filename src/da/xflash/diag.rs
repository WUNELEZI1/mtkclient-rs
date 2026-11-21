//! DA device diagnostics and status query
//!
//! Contains methods used after DAXFlash loading for device info queries and session state management:
//! - `reinit`: query SRAM/DRAM/EMMC chip info
//! - `get_emmc_info`: query EMMC Boot1/Boot2 sizes
//! - `save_session_state`: write the DA session to a .state file
//! - `get_emi_data` / `get_extensions_data` / `get_last_gpt_data`: debug getters
//!
//! Split rationale: these methods are all "read-only / metadata" operations that do not participate in the DA loading main flow.

use log::{info, trace, warn};
use std::time::Duration;

use crate::da::xflash::protocol::GET_DA_VER_CMD;
use crate::da::xflash::{DAXFlash, EmmcInfo};

// =============================================================================
// Device info queries
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// Reinitialize (DA reuse path: heartbeat only, no operation that could change DA state)
    ///
    /// When reusing DA the device is already fully initialized; the sole purpose of reinit is to confirm DA is still alive.
    /// Send GET_DA_VER_CMD as a heartbeat, without sending GET_EMMC_INFO or other state-changing commands.
    /// Note: USB high-speed reconnect (`try_usb_high_speed_reconnect`, two-stage HS→FS fallback) is an independent
    /// optional optimization triggered explicitly by the caller after DA loading; it is not done here.
    pub(crate) fn reinit(&mut self) -> Result<(), String> {
        // Heartbeat: GET_CHIP_ID (confirm DA is alive)
        match self.send_devctrl(GET_DA_VER_CMD, None) {
            Ok(data) if data.len() >= 10 => {
                let hw_code = u16::from_le_bytes([data[0], data[1]]);
                info!("  芯片 HW Code: 0x{:04X}", hw_code);
            }
            Ok(_) => trace!("[reinit] GET_DA_VER_CMD 返回空数据"),
            Err(e) => trace!("[reinit] GET_DA_VER_CMD 失败 (可能不支持): {}", e),
        }

        Ok(())
    }

    /// USB high-speed reconnect: detect the current speed and, if full-speed, switch and reconnect
    /// Automatically skipped in serial mode (no USB speed concept).
    ///
    /// Note: skip set_usb_speed only for platforms **empirically confirmed to stall when switching to high speed on WinUSB**.
    /// The criterion is "whether os error 31 (ERROR_GEN_FAILURE) occurs under WinUSB",
    /// not the chip release era — the newer MT6768(0x707) has been confirmed to reconnect at high speed
    /// fine and must not be excluded, losing read speedup; MT6771(0x788, 2018) is older than MT6768(0x707, 2019)
    /// and is exactly the platform that triggers os error 31 on WinUSB, so only it and earlier platforms go on the skip list.
    fn chip_skips_usb_speed_switch(hw_code: u16) -> bool {
        matches!(
            hw_code,
            // Platforms that stall only when switching to high speed on WinUSB (observed os error 31)
            0x788 | // MT6771/MT8385/MT8183/MT8666 (Helio P60/P70/G80, 2018) — confirmed stall
            0x676 | // MT6765/MT6762 (Helio A25/P22, 2018)
            0x762 | // MT6763 (Helio P23, 2017)
            0x0699 // MT6799 and earlier platforms
                   // Note: MT6768/0x707 (2019) is not in this list; confirmed to reconnect at high speed fine
        )
    }

    pub(crate) fn try_usb_high_speed_reconnect(&mut self) {
        if !self.preloader.device.is_libusb() {
            trace!("[RECONNECT] 串口模式，跳过 USB 高速重连");
            return;
        }

        // Skip USB speed switching on platforms that stall when switching to high speed on WinUSB, avoiding clear_halt triggering os error 31
        let hw_code = self.preloader.chip.map(|c| c.hw_code).unwrap_or(0);
        if Self::chip_skips_usb_speed_switch(hw_code) {
            trace!(
                "[RECONNECT] HW Code 0x{:04X} 在 WinUSB 上切高速会 stall，跳过 USB 速度切换（避免 os error 31）",
                hw_code
            );
            return;
        }

        let speed = match self.get_usb_speed() {
            Ok(s) => s,
            Err(e) => {
                trace!("[RECONNECT] get_usb_speed 失败: {}，跳过重连", e);
                return;
            }
        };

        if speed != "full-speed" {
            info!("[RECONNECT] 当前 USB 速度: {}，无需重连", speed);
            return;
        }

        info!("[RECONNECT] 检测到 full-speed，执行高速重连...");

        // 1. Command the device to switch to high speed
        if let Err(e) = self.set_usb_speed() {
            warn!("[RECONNECT] set_usb_speed 失败: {}，跳过重连", e);
            return;
        }

        // 2. USB bus reset (triggers the device to re-enumerate at the new speed)
        if let Err(e) = self.preloader.device.reset_device() {
            warn!("[RECONNECT] USB reset 失败: {}，跳过重连", e);
            return;
        }

        // 3. Release the old USB handle
        if let Err(e) = self.preloader.device.close_device() {
            warn!("[RECONNECT] 关闭旧 USB 句柄失败: {}，跳过重连", e);
            return;
        }
        info!("[RECONNECT] 已关闭旧 USB 句柄，等待设备重新枚举...");

        // 4. Wait for the device to re-enumerate: a short pause to let the bus settle is enough; the actual
        //    re-enumeration is captured by the reopen retry loop below (HS 5×500ms / FS 5×1000ms) in an
        //    event-driven way, with no blind fixed 2-second wait (matching the intent of Python time.sleep(2)
        //    but saving ~1.5s).
        std::thread::sleep(Duration::from_millis(500));

        // 5. Reopen the USB device. High-speed reconnect is a "best-effort optional optimization" and must
        //    **never discard the already-loaded DA results**: try high speed first; if that fails, fall back
        //    to a full-speed reconnect — DA is still resident in device memory, so if the device has fallen
        //    back to full-speed re-enumeration it can be reused, preserving the results and continuing work.
        let usb_context = match crate::usb::UsbContext::new() {
            Ok(ctx) => ctx,
            Err(e) => {
                warn!(
                    "[RECONNECT] 创建 USB 上下文失败: {}，放弃高速重连（保留 DA 会话）",
                    e
                );
                return;
            }
        };

        const HS_RETRIES: usize = 5;
        const FS_RETRIES: usize = 5;
        let mut connected_speed: Option<&str> = None;

        // 5a. High-speed reconnect attempts
        for attempt in 1..=HS_RETRIES {
            match self.preloader.device.reopen_device(&usb_context) {
                Ok(_) => {
                    connected_speed = Some("high-speed");
                    info!(
                        "[RECONNECT] USB 高速重连成功 (第 {} 次)，读取速度将显著提升",
                        attempt
                    );
                    break;
                }
                Err(e) => {
                    trace!(
                        "[RECONNECT] 高速重连第 {}/{} 次失败: {}",
                        attempt, HS_RETRIES, e
                    );
                    if attempt < HS_RETRIES {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }

        // 5b. Fallback full-speed reconnect: DA is still resident, the device may have re-enumerated back at full-speed
        if connected_speed.is_none() {
            warn!(
                "[RECONNECT] 高速重连失败，降级尝试 full-speed 重连（保留 DA 成果，放弃高速提速）"
            );
            for attempt in 1..=FS_RETRIES {
                match self.preloader.device.reopen_device(&usb_context) {
                    Ok(_) => {
                        connected_speed = Some("full-speed");
                        info!(
                            "[RECONNECT] 已降级 full-speed 重连成功 (第 {} 次)，继续以 full-speed 工作",
                            attempt
                        );
                        break;
                    }
                    Err(e) => {
                        trace!(
                            "[RECONNECT] full-speed 重连第 {}/{} 次失败: {}",
                            attempt, FS_RETRIES, e
                        );
                        if attempt < FS_RETRIES {
                            std::thread::sleep(Duration::from_millis(1000));
                        }
                    }
                }
            }
        }

        if connected_speed == Some("high-speed") {
            // High-speed reconnect succeeded; no extra message needed
        } else if connected_speed == Some("full-speed") {
            warn!("[RECONNECT] 最终以 full-speed 工作（高速重连未成功，DA 成果已保留）");
        } else {
            warn!(
                "[RECONNECT] 高速与 full-speed 重连均失败，设备可能已物理断开（DA 成果无法保留）"
            );
        }
    }

    /// Get full EMMC info (Boot1/Boot2/RPMB/User Size/Block Size/CID)
    ///
    /// Matches the flashing-box MtkClient log protocol:
    ///   1. DEVICE_CTRL(0x010009) → status
    ///   2. 0x00040001 → status → xread(104 bytes) → status
    ///
    /// Return format (104 bytes):
    ///   [0..4]   emmc_type (u32)
    ///   [4..8]   block_size (u32)
    ///   [8..72]  8 x qword: boot1, boot2, rpmb, gp1..gp4, user
    ///   [72..88] CID (16 bytes)
    ///
    /// Short 200ms timeout; fails fast when unsupported
    pub fn get_emmc_info(&mut self) -> Result<EmmcInfo, String> {
        let data = self.with_short_timeout(200, |da| da.send_emmc_query(0x00040001))?;
        Self::parse_emmc_info(&data)
    }

    /// Get simplified EMMC info (short 50ms timeout; fails fast when unsupported)
    pub fn get_emmc_info_simple(&mut self) -> Result<EmmcInfo, String> {
        let data = self.with_short_timeout(50, |da| da.send_emmc_query(0x00040001))?;
        if data.len() < 8 {
            return Err("EMMC info 数据太短".to_string());
        }
        Self::parse_emmc_info(&data)
    }

    /// Send the EMMC query command (DEVICE_CTRL prefix + sub-command)
    ///
    /// Matches the flashing box: send DEVICE_CTRL before every 0x0004XXXX command
    fn send_emmc_query(&mut self, cmd: u32) -> Result<Vec<u8>, String> {
        use crate::da::xflash::protocol::{CMD_MAGIC, pack3};

        // Step 1: DEVICE_CTRL prefix
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;
        let st1 = self.status()?;
        if st1 != 0 {
            return Err(format!("EMMC query DEVICE_CTRL status: 0x{:08X}", st1));
        }

        // Step 2: Send the sub-command
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&cmd.to_le_bytes())?;
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!(
                "EMMC query cmd=0x{:08X} status: 0x{:08X}",
                cmd, st2
            ));
        }

        // Step 3: Read the response data
        let resp = self.xread_data()?;
        let _ = self.status();
        Ok(resp)
    }

    /// Parse EMMC info data (104-byte format)
    fn parse_emmc_info(data: &[u8]) -> Result<EmmcInfo, String> {
        if data.len() < 8 {
            return Err(format!(
                "EMMC info 数据太短（需要至少 8 字节，实际 {} 字节）",
                data.len()
            ));
        }

        let emmc_type_code = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let block_size = u32::from_le_bytes(data[4..8].try_into().unwrap());

        let read_qword = |offset: usize| -> u64 {
            if data.len() >= offset + 8 {
                u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
            } else {
                0
            }
        };

        let boot1_size = read_qword(8);
        let boot2_size = read_qword(16);
        let rpmb_size = read_qword(24);
        let user_size = read_qword(64);

        let cid = if data.len() >= 88 {
            data[72..88].to_vec()
        } else {
            Vec::new()
        };

        let emmc_type = match emmc_type_code {
            0 => "Unknown".to_string(),
            1 => "EMMC".to_string(),
            2 => "UFS".to_string(),
            3 => "SD".to_string(),
            4 => "NAND".to_string(),
            5 => "NAND_PARALLEL".to_string(),
            6 => "UFS_CARD".to_string(),
            n => format!("Type#{}", n),
        };

        Ok(EmmcInfo {
            boot1_size,
            boot2_size,
            rpmb_size,
            user_size,
            block_size,
            emmc_type,
            cid,
        })
    }
}

// =============================================================================
// Session state saving
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// Save the DA session state to a .state file
    ///
    /// Written content:
    /// - usb_vid / usb_pid: VID/PID of the current USB device
    /// - hw_code: HW code of the current chip
    /// - target_config: security config of the current device
    /// - da_loaded=true: marks DA as loaded
    ///
    /// If fetching VID/PID or HW code fails, it is silently ignored (does not affect the successful DA load result)
    pub(crate) fn save_session_state(&mut self) {
        let (vid, pid) = match (
            self.preloader.device.get_vid(),
            self.preloader.device.get_pid(),
        ) {
            (Some(v), Some(p)) => (v, p),
            _ => {
                // Serial mode: get_vid/pid return None, use the MTK default VID/PID
                trace!("save_session_state: 串口模式，使用默认 VID/PID");
                (0x0E8D, 0x2000)
            }
        };

        // Get HW code (read from chip config, avoiding another USB command)
        let hw_code_result = self
            .preloader
            .chip
            .map(|c| c.hw_code)
            .ok_or_else(|| "chip not set".to_string());

        // Get target_config (read local state directly, no USB command)
        // In the actual implementation target_config is read into chip config during init()
        let target_config = 0u32; // placeholder — the target_config field in session.rs currently does not participate in reuse decisions

        if let Ok(hw_code) = hw_code_result {
            let preloader_path = self.preloader_path.as_deref();
            let init_mode = if self.preloader.is_preloader_mode {
                crate::connection::session::InitMode::Preloader
            } else {
                crate::connection::session::InitMode::Brom
            };
            crate::connection::save_da_session(
                vid,
                pid,
                hw_code,
                target_config,
                preloader_path,
                init_mode,
            );
            for name in &self.optional_query_failures {
                crate::connection::session::mark_optional_query_failed(name);
            }
        } else {
            warn!("save_session_state: chip 未初始化，跳过 .state 保存");
        }
    }
}

// =============================================================================
// DA heartbeat / session keep-alive
// =============================================================================

impl<'a> DAXFlash<'a> {}

// =============================================================================
// Debug / getter methods
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// Get EMI data (for debug mode)
    pub fn get_emi_data(&self) -> Option<&Vec<u8>> {
        self.emi.as_ref()
    }

    /// Get DA extensions data (for debug mode)
    pub fn get_extensions_data(&self) -> Option<Vec<u8>> {
        if self.da2_data.is_empty() {
            None
        } else {
            self.generate_da_extensions()
        }
    }

    /// Get the raw data of the last GPT read (for debug mode)
    pub fn get_last_gpt_data(&self) -> Result<&Vec<u8>, String> {
        self.last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据".to_string())
    }
}

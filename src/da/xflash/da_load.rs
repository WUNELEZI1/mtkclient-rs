//! Main DA loading flow
//!
//! Implements the full DAXFlash::upload_da() flow, matching Python mtkclient xflash_lib.py:
//!   upload_da1 → get_expire_date → set_reset_key → set_checksum_level
//!     → get_connection_agent → send_emi → upload_da2 → get_sla_status
//!     → reinit → drain USB buffer → boot_to(0x4FFF0000, daextdata)
//!     → CUSTOM_ACK → CUSTOM_SET_STORAGE
//!
//! This module only orchestrates the flow — every step lives in the
//! `da_xflash_setup` / `da_xflash_protocol` / `da_extension` submodules.

use log::{info, trace, warn};
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use crate::connection::session::{
    mark_optional_query_failed as save_optional_query_failure, optional_query_failed,
};
use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::DA_EXT_BOOT_ADDR;

// =============================================================================
// Public constants — reuse DA extensions magic / devctrl values from submodules
// =============================================================================

/// Response magic expected by the DA extensions CUSTOM_ACK (matches flasher xflash_lib.py:1253)
pub const DA_EXTENSIONS_ACK_MAGIC: u32 = 0xA1A2A3A4;
/// CUSTOM_ACK devctrl id
const DA_EXTENSIONS_DEVCTRL_ACK: u32 = 0x0F0000;
/// CUSTOM_SET_STORAGE devctrl id (matches flasher xflash_lib.py:1256)
const DA_EXTENSIONS_DEVCTRL_SET_STORAGE: u32 = 0x0F0005;
const QUERY_EXPIRE_DATE: &str = "get_expire_date";
const QUERY_CONNECTION_AGENT: &str = "get_connection_agent";
const QUERY_SLA_STATUS: &str = "get_sla_status";

// =============================================================================
// Main DA loading flow
// =============================================================================

impl<'a> DAXFlash<'a> {
    fn optional_query_should_skip(&self, name: &str) -> bool {
        self.optional_query_failures.iter().any(|item| item == name) || optional_query_failed(name)
    }

    fn mark_optional_query_failed(&mut self, name: &str) {
        if !self.optional_query_failures.iter().any(|item| item == name) {
            self.optional_query_failures.push(name.to_string());
        }
        save_optional_query_failure(name);
    }

    /// Upload DA (full flow, matching Python upload_da)
    pub fn upload_da(&mut self) -> Result<bool, String> {
        trace!("开始 DA 加载流程...");

        if !self.upload_da1()? {
            return Err("Stage1 上传失败".to_string());
        }

        // --- Speed level 1 = full protocol, 2/3 = skip optional queries ---
        if self.da_x_speed == 1 {
            if self.optional_query_should_skip(QUERY_EXPIRE_DATE) {
                trace!("跳过已知失败的可选查询: {}", QUERY_EXPIRE_DATE);
            } else {
                match self.get_expire_date() {
                    Ok(d) if !d.is_empty() => trace!("  过期日期: {:02X?}", d),
                    Err(e) => {
                        warn!("get_expire_date 失败 (可能不支持): {}", e);
                        self.mark_optional_query_failed(QUERY_EXPIRE_DATE);
                    }
                    _ => {}
                }
            }

            if let Err(e) = self.set_reset_key(0x68) {
                warn!("set_reset_key 失败 (可能不支持): {}", e);
            }

            if let Err(e) = self.set_checksum_level(0x0) {
                warn!("set_checksum_level 失败 (可能不支持): {}", e);
            }
        } else {
            trace!(
                "[SPEED{}] 跳过 get_expire_date / set_reset_key / set_checksum_level",
                self.da_x_speed
            );
        }

        // Connection agent decision (level 3 infers it directly from the known mode)
        let is_brom_conn = if self.da_x_speed >= 3 {
            // Turbo mode: skip get_connection_agent and infer from the mode
            let brom = !self.preloader.is_preloader_mode;
            trace!(
                "[SPEED3] 跳过 get_connection_agent，推断 conn_agent={}",
                if brom { "brom" } else { "preloader" }
            );
            brom
        } else {
            let conn_agent = if self.optional_query_should_skip(QUERY_CONNECTION_AGENT) {
                trace!("跳过已知失败的可选查询: {}", QUERY_CONNECTION_AGENT);
                "brom".to_string()
            } else {
                match self.get_connection_agent() {
                    Ok(agent) => agent,
                    Err(e) => {
                        warn!("get_connection_agent 失败: {}", e);
                        self.mark_optional_query_failed(QUERY_CONNECTION_AGENT);
                        "brom".to_string()
                    }
                }
            };
            trace!("  连接代理: {}", conn_agent);
            conn_agent == "brom"
        };

        if is_brom_conn {
            if let Some(emi_data) = self.emi.clone() {
                trace!("发送 EMI 数据...");
                self.send_emi(&emi_data)?;
            } else {
                warn!("未找到 EMI 数据，跳过发送");
            }
        }

        if !self.upload_da2()? {
            return Err("Stage2 上传失败".to_string());
        }

        // DA2 sends a SYNC signal after boot (arrives in ~0.5-1s); consume it so it does not
        // pollute later commands. Event-driven drain (replaces a blind sleep(800)): poll with a
        // short timeout and consume SYNC, stopping once the pipe stays quiet — an early SYNC
        // saves time, a late SYNC still is not dropped.
        self.drain_until_quiet(1500);

        if self.da_x_speed == 1 {
            if self.optional_query_should_skip(QUERY_SLA_STATUS) {
                trace!("跳过已知失败的可选查询: {}", QUERY_SLA_STATUS);
            } else {
                match self.get_sla_status() {
                    Ok(sla) => {
                        if sla != 0 {
                            trace!("  DA SLA 已启用: 0x{:08X}", sla);
                        } else {
                            trace!("  DA SLA 未启用");
                        }
                    }
                    Err(e) => {
                        warn!("get_sla_status 失败: {}", e);
                        self.mark_optional_query_failed(QUERY_SLA_STATUS);
                    }
                }
            }
        } else {
            trace!("[SPEED{}] 跳过 get_sla_status", self.da_x_speed);
        }

        if !self.preloader.device.is_libusb() {
            // Serial mode: skip the USB high-speed reconnect and run a minimal reinit
            trace!("[PRELOADER] 串口模式，跳过 USB 高速重连");
            if let Err(e) = self.reinit() {
                warn!("reinit 失败: {}", e);
            }
        } else if self.da_x_speed >= 3 {
            // Turbo mode: skip the device info query but still run the USB high-speed reconnect
            trace!("[SPEED3] 跳过设备信息查询，直接执行 USB 高速重连");
            self.try_usb_high_speed_reconnect();
        } else if let Err(e) = self.reinit() {
            warn!("reinit 失败: {}", e);
        }

        // 10. Load DA extensions (matches Python xflash_lib.py lines 1247-1258)
        //      Python: daextdata = self.xft.patch()
        //              if self.boot_to(addr=0x4FFF0000, da=daextdata):
        //                  ret = self.send_devctrl(XCmd.CUSTOM_ACK)
        //                  status = self.status()
        //                  if status == 0x0 and unpack("<I", ret)`0` == 0xA1A2A3A4:
        trace!("正在加载 DA extensions...");

        // Flush the USB input buffer: the device may send leftover data after reinit(),
        // and any residue would pollute the BOOT_TO command response.
        self.drain_usb_input();

        if self.patch_da
            && let Some(ext_data) = self.generate_da_extensions()
        {
            match self.boot_to(DA_EXT_BOOT_ADDR, &ext_data, true, 0.5) {
                Ok(_) => {
                    // boot_to already read the SYNC status, so the DA extension code is running.
                    // Wait for DA extensions to fully initialize, then flush the USB buffer
                    // so residue does not pollute the CUSTOM_ACK response.
                    sleep(Duration::from_millis(100));
                    self.drain_usb_input();
                    self.load_da_extensions_after_boot();
                }
                Err(e) => {
                    warn!("boot_to(extensions) 失败: {}", e);
                }
            }
        }
        if !self.daext {
            warn!("DA extensions 未启用");
        }

        info!("{}", crate::i18n::t("log_da_loaded"));

        // === Save DA session state ===
        // After a successful DA load, write the current device state to the .state file so the
        // next launch can skip the BROM→DA flow if the device is already in DA mode and .state is valid.
        self.save_session_state();

        Ok(true)
    }

    /// CUSTOM_ACK / CUSTOM_SET_STORAGE flow after a successful BOOT_TO
    fn load_da_extensions_after_boot(&mut self) {
        // Python line 1251: send CUSTOM_ACK immediately after boot_to succeeds.
        // boot_to already waited for the response; only a tiny buffer is needed for the device
        // to switch context.
        std::thread::sleep(Duration::from_millis(20));
        if let Ok(ack) = self.send_devctrl(DA_EXTENSIONS_DEVCTRL_ACK, None) {
            // send_devctrl already consumed the post-xread status packet (4 packets in total:
            //   stage1_status → stage2_status → xread_data → status_after_data).
            // Do not read status again — otherwise the 5th packet never arrives, a 5s timeout
            // follows, and that leads to: DA extensions disabled → HACC software fallback →
            // DA state machine desync → write timeout.
            // A magic match is treated as success (status is always 0 when DA returns 0xA1A2A3A4).
            if ack.len() >= 4 {
                let magic = u32::from_le_bytes([ack[0], ack[1], ack[2], ack[3]]);
                if magic == DA_EXTENSIONS_ACK_MAGIC {
                    // Detect device storage type: 0=eMMC, 1=UFS, 2=SD, 3=MMC, 6=UFS_CARD
                    let storage_type = self
                        .get_emmc_info()
                        .map(|info| match info.emmc_type.as_str() {
                            "UFS" => 1u32,
                            "SD" => 2u32,
                            "MMC" => 3u32,
                            "UFS_CARD" => 6u32,
                            _ => 0u32, // default eMMC
                        })
                        .unwrap_or(0u32);
                    // Python line 1256: call custom_set_storage right after CUSTOM_ACK succeeds
                    if self
                        .send_devctrl(
                            DA_EXTENSIONS_DEVCTRL_SET_STORAGE,
                            Some(&storage_type.to_le_bytes()),
                        )
                        .is_ok()
                    {
                        let type_name = match storage_type {
                            1 => "UFS",
                            2 => "SD",
                            3 => "MMC",
                            6 => "UFS_CARD",
                            _ => "eMMC",
                        };
                        info!("{}", crate::i18n::tr("log_da_ext_ready", &[&type_name]));
                        self.daext = true;
                    } else {
                        warn!("custom_set_storage 失败，extensions 功能可能受限");
                        self.daext = true;
                    }
                } else {
                    warn!(
                        "DA extensions CUSTOM_ACK 验证失败 (magic=0x{:08X}, 期望=0x{:08X})",
                        magic, DA_EXTENSIONS_ACK_MAGIC
                    );
                }
            } else {
                warn!("DA extensions CUSTOM_ACK 响应为空 (len={})", ack.len());
            }
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// Flush the USB input buffer (50ms short timeout), discarding residue left after reinit()
    ///
    /// Matches the manual cleanup around line 1240 of Python xflash_lib.py:
    /// the device may send residue after reinit(); if not flushed it pollutes the BOOT_TO response.
    fn drain_usb_input(&mut self) {
        let mut drain_buf = [0u8; 512];
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader.device.set_timeout(Duration::from_millis(50));
        loop {
            match self.preloader.device.read(&mut drain_buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    trace!("[DRAIN] discarded {} bytes", n);
                }
            }
        }
        self.preloader.device.set_timeout(orig_timeout);
    }

    /// Event-driven IN-pipe flush: poll with a 50ms short timeout and consume all data
    /// until the pipe stays quiet (`QUIET_TICKS` timeouts in a row) or `max_wait_ms` is reached.
    ///
    /// Replaces the blind `sleep(800)` after DA2 boot: DA2 sends a SYNC packet after ~0.5-1s,
    /// and a blind wait either wastes time (early SYNC) or exits early and drops packets
    /// (late SYNC, polluting later commands). Consume SYNC, keep a short quiet period, then stop.
    fn drain_until_quiet(&mut self, max_wait_ms: u64) {
        const TICK_MS: u64 = 50;
        const QUIET_TICKS: u32 = 3; // ~150ms of no data in a row counts as quiet
        let start = Instant::now();
        let mut quiet = 0u32;
        let mut buf = [0u8; 512];
        let orig = self.preloader.device.get_timeout();
        loop {
            self.preloader
                .device
                .set_timeout(Duration::from_millis(TICK_MS));
            match self.preloader.device.read(&mut buf) {
                Ok(0) | Err(_) => {
                    quiet += 1;
                    if quiet >= QUIET_TICKS {
                        break;
                    }
                }
                Ok(_) => {
                    quiet = 0;
                }
            }
            if start.elapsed().as_millis() as u64 >= max_wait_ms {
                break;
            }
        }
        self.preloader.device.set_timeout(orig);
    }
}

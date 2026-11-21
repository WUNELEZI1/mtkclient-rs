//! Protocol primitives - block 3: sync / devctrl

use super::{CMD_MAGIC, CMD_SYNC_SIGNAL, SHORT_QUERY_TIMEOUT_MS, SLA_QUERY_TIMEOUT_MS, pack3};
use log::{info, trace, warn};
use std::time::Duration;

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// XFlash sync command.
    /// Python: sync() -> only xsend(CMD_SYNC_SIGNAL), reads neither status nor response
    ///
    /// Warning: this method only sends SYNC_SIGNAL and reads no response. When called
    /// during DA loading (upload_da1), the DA may not return a status and a forced read
    /// would fail with a timeout. Callers that need to clear stale data should call
    /// drain_pending() manually at an appropriate point.
    pub(crate) fn xflash_sync(&mut self) -> Result<bool, String> {
        trace!("执行 XFlash 同步命令...");

        // Python: self.sync() -> self.xsend(self.Cmd.SYNC_SIGNAL) = 0x434E5953
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_SYNC_SIGNAL.to_le_bytes())?;

        trace!("[SYNC] 已发送 SYNC_SIGNAL (0x434E5953)");
        Ok(true)
    }

    /// Sends a devctrl command.
    /// Python: returns b"" at every stage, never raises
    ///
    /// Critical fix: even when status is nonzero (e.g. 0x00010009) the full 3-step
    /// handshake (DEVICE_CTRL -> cmd -> param/status) must still complete, otherwise
    /// the DA state machine stalls waiting and every later command times out.
    pub(crate) fn send_devctrl(
        &mut self,
        cmd: u32,
        param: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        trace!(
            "[send_devctrl] cmd=0x{:06X} param={}",
            cmd,
            param.map_or(0, |p| p.len())
        );

        // xsend(Cmd.DEVICE_CTRL) - DEVICE_CTRL = 0x010009
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;

        let st = self.status()?;
        let stage1_ok = st == 0;
        if !stage1_ok {
            // SYNC (0x434E5953) = DA init signal left over in the USB buffer
            // Mirrors Python send_devctrl: when stage1 != 0, do not send cmd, return empty
            // This keeps the cmd response out of the queue so later commands do not read stale data
            if st == CMD_SYNC_SIGNAL {
                trace!(
                    "[send_devctrl] stage1=SYNC (DA init residual), 跳过 cmd 0x{:06X}",
                    cmd
                );
                // Read and discard exactly one leftover packet (no drain loop, to avoid consuming later responses)
                let _ = self.status();
                return Ok(vec![]);
            }
            // Known benign status codes: handled silently
            // 0x00010009 = DEVICE_CTRL unsupported (some devices/DA versions)
            // 0xC0010004 = command unsupported
            if st != 0xC0010004 && st != 0x00010009 {
                warn!("send_devctrl DEVICE_CTRL 阶段1 状态: 0x{:08X}", st);
            }
            trace!("[send_devctrl] stage1 status=0x{:08X}, 继续完成握手...", st);
        }

        // xsend(cmd) - sent when stage1 succeeds (SYNC returned early above)
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&cmd.to_le_bytes())?;

        let st2 = self.status()?;
        let stage2_ok = st2 == 0;
        if !stage2_ok {
            if st2 != 0xC0010004 && st2 != 0x00010009 {
                warn!("send_devctrl(0x{:06X}) 阶段2 状态: 0x{:08X}", cmd, st2);
            }
            trace!(
                "[send_devctrl] stage2 status=0x{:08X}, 继续完成握手...",
                st2
            );
        }

        // Only param mode requires both stages to succeed before continuing.
        // read mode: attempt xread only when both stage1/stage2 succeed.
        // If stage2 returns 0xC0010004 (command unsupported) or 0x00010009, the DA sends no xread data.
        if param.is_none() {
            if stage2_ok {
                let resp = self.xread_data()?;
                let _ = self.status();
                trace!(
                    "[send_devctrl] cmd=0x{:06X} xread returned {} bytes",
                    cmd,
                    resp.len()
                );
                return Ok(resp);
            } else {
                // Command or device unsupported: return empty data (mirrors Python: always b"")
                trace!(
                    "[send_devctrl] cmd=0x{:06X} stage2 不支持(0x{:08X})，返回空",
                    cmd, st2
                );
                return Ok(vec![]);
            }
        }

        // param mode: send the param only when both stages succeed
        if stage1_ok && stage2_ok {
            if let Some(p) = param {
                let pkt3 = pack3(CMD_MAGIC, 0x01, p.len() as u32);
                self.preloader.device.write(&pkt3)?;
                self.preloader.device.write(p)?;
                let st3 = self.status()?;
                if st3 != 0 {
                    // A nonzero param status means the command failed; return an error for the caller
                    return Err(format!(
                        "send_devctrl(0x{:06X}) param status: 0x{:08X}",
                        cmd, st3
                    ));
                }
            }
        } else {
            // read mode was handled above; in param mode the param must still be sent to complete the handshake on stage failure
            if let Some(p) = param {
                let pkt3 = pack3(CMD_MAGIC, 0x01, p.len() as u32);
                let _ = self.preloader.device.write(&pkt3);
                let _ = self.preloader.device.write(p);
                let _ = self.status(); // read and discard status3 to complete the handshake
            }
        }

        Ok(vec![])
    }

    /// Runs a callback with a temporary short timeout, then restores the original one.
    /// Used for optional queries: fail fast when the command is unsupported instead of waiting 5s.
    pub(crate) fn with_short_timeout<T>(
        &mut self,
        ms: u64,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let orig = self.preloader.device.get_timeout();
        self.preloader.device.set_timeout(Duration::from_millis(ms));
        let result = f(self);
        self.preloader.device.set_timeout(orig);
        result
    }

    /// Gets the connection agent (brom or preloader).
    /// Python: b"" or None is treated as failure
    pub(crate) fn get_connection_agent(&mut self) -> Result<String, String> {
        let data =
            self.with_short_timeout(SHORT_QUERY_TIMEOUT_MS, |da| da.send_devctrl(0x010102, None))?;
        if data.is_empty() {
            return Err("get_connection_agent returned empty".to_string());
        }
        Ok(String::from_utf8_lossy(&data).to_string())
    }

    /// DA extension command: CUSTOM_READREGISTER (0x0F0002).
    /// Mirrors Python xflash.py: custom_readregister(addr)
    /// Flow: cmd(0x0F0002) -> xsend(addr) -> xread() -> status()
    pub(crate) fn custom_readregister(&mut self, addr: u32) -> Result<u32, String> {
        // step 1: cmd(0x0F0002) - send DEVICE_CTRL + CUSTOM_READREGISTER
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;
        let st1 = self.status()?;
        if st1 != 0 {
            return Err(format!(
                "custom_readregister: DEVICE_CTRL status=0x{:08X}",
                st1
            ));
        }

        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&0x0F0002u32.to_le_bytes())?;
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!(
                "custom_readregister: CUSTOM_READREGISTER status=0x{:08X}",
                st2
            ));
        }

        // step 2: xsend(addr)
        let pkt3 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt3)?;
        self.preloader.device.write(&addr.to_le_bytes())?;

        // step 3: xread() - read the 4-byte register value
        let data = self.xread_data()?;

        // step 4: status()
        let st3 = self.status()?;
        if st3 != 0 {
            return Err(format!("custom_readregister: read status=0x{:08X}", st3));
        }

        if data.len() < 4 {
            return Err(format!(
                "custom_readregister: response too short: {} bytes",
                data.len()
            ));
        }
        Ok(u32::from_le_bytes(data[0..4].try_into().unwrap()))
    }

    /// DA extension command: CUSTOM_WRITEREGISTER (0x0F0004).
    /// Mirrors Python xflash.py: custom_writeregister(addr, data)
    /// Flow: cmd(0x0F0004) -> xsend(addr) -> xsend(data) -> status()
    pub(crate) fn custom_writeregister(&mut self, addr: u32, value: u32) -> Result<(), String> {
        // step 1: cmd(0x0F0004) - send DEVICE_CTRL + CUSTOM_WRITEREGISTER
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;
        let st1 = self.status()?;
        if st1 != 0 {
            return Err(format!(
                "custom_writeregister: DEVICE_CTRL status=0x{:08X}",
                st1
            ));
        }

        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&0x0F0004u32.to_le_bytes())?;
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!(
                "custom_writeregister: CUSTOM_WRITEREGISTER status=0x{:08X}",
                st2
            ));
        }

        // step 2: xsend(addr)
        let pkt3 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt3)?;
        self.preloader.device.write(&addr.to_le_bytes())?;

        // step 3: xsend(value)
        let pkt4 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt4)?;
        self.preloader.device.write(&value.to_le_bytes())?;

        // step 4: status()
        let st3 = self.status()?;
        if st3 != 0 {
            return Err(format!("custom_writeregister: write status=0x{:08X}", st3));
        }

        Ok(())
    }

    /// Sets the reset key.
    pub(crate) fn set_reset_key(&mut self, key: u32) -> Result<(), String> {
        self.send_devctrl(0x010103, Some(&key.to_le_bytes()))?;
        Ok(())
    }

    /// Sets the checksum level.
    pub(crate) fn set_checksum_level(&mut self, level: u32) -> Result<(), String> {
        self.send_devctrl(0x010104, Some(&level.to_le_bytes()))?;
        Ok(())
    }

    /// Gets the expire date (short timeout: 200ms, skipped quickly when unsupported).
    pub(crate) fn get_expire_date(&mut self) -> Result<Vec<u8>, String> {
        let data =
            self.with_short_timeout(SHORT_QUERY_TIMEOUT_MS, |da| da.send_devctrl(0x010105, None))?;
        if data.is_empty() {
            return Err("get_expire_date returned empty".to_string());
        }
        Ok(data)
    }

    /// Gets the SLA status (short timeout: 200ms, skipped quickly when unsupported).
    pub(crate) fn get_sla_status(&mut self) -> Result<u32, String> {
        let data =
            self.with_short_timeout(SLA_QUERY_TIMEOUT_MS, |da| da.send_devctrl(0x01010E, None))?; // SLA_ENABLED_STATUS
        if data.len() >= 4 {
            Ok(u32::from_le_bytes(data[..4].try_into().unwrap()))
        } else {
            Err("sla_status empty".to_string())
        }
    }

    /// Sets Meta Boot Mode (mirrors Python set_meta_boot_mode).
    /// Command: CMD_DEVICE_CTRL(0x010009) + SET_META_BOOT_MODE(0x020006)
    /// Parameters: boot_mode (u32, 0=fastboot, 1=meta)
    pub(crate) fn set_meta_boot_mode(&mut self, boot_mode: u32) -> Result<(), String> {
        self.send_devctrl(0x020006, Some(&boot_mode.to_le_bytes()))?;
        Ok(())
    }

    /// Queries the current USB speed (mirrors Python get_usb_speed).
    /// Returns "full-speed" / "high-speed" / "super-speed" or "unknown"
    pub(crate) fn get_usb_speed(&mut self) -> Result<String, String> {
        let data = self.send_devctrl(0x010115, None)?; // GET_USB_SPEED
        if data.is_empty() {
            return Err("get_usb_speed 返回空".to_string());
        }
        // The device may return a numeric value (u32 LE) or a string; support both formats
        let speed = if data.len() >= 4 {
            let val = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            match val {
                0 => "full-speed",
                1 => "high-speed",
                2 => "super-speed",
                _ => {
                    // Not a known numeric value; try parsing as a UTF-8 string
                    let s = String::from_utf8_lossy(&data).trim().to_string();
                    if s.is_empty() || !s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                        trace!("  USB 速度: 未知值 0x{:08X}, raw={:02X?}", val, &data);
                        return Ok("unknown".to_string());
                    }
                    // Valid string: return it directly
                    return Ok(s);
                }
            }
            .to_string()
        } else {
            // Fewer than 4 bytes: try string parsing
            let s = String::from_utf8_lossy(&data).trim().to_string();
            if s.is_empty() || !s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                trace!("  USB 速度: raw={:02X?}", &data);
                "unknown".to_string()
            } else {
                s
            }
        };
        trace!("  USB 速度: {}", speed);
        Ok(speed)
    }

    /// Asks the device to switch to a higher USB speed (mirrors Python set_usb_speed).
    /// Sends SWITCH_USB_SPEED + magic 0x0E8D2001
    pub(crate) fn set_usb_speed(&mut self) -> Result<(), String> {
        // Python: self.xsend(self.Cmd.SWITCH_USB_SPEED) + status
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010114u32.to_le_bytes())?; // SWITCH_USB_SPEED
        let st = self.status()?;
        if st != 0 {
            return Err(format!("SWITCH_USB_SPEED 状态: 0x{:08X}", st));
        }

        // Python: self.xsend(pack("<I", 0x0E8D2001)) + status
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&0x0E8D2001u32.to_le_bytes())?; // MediaTek USB VID magic
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("set_usb_speed magic 状态: 0x{:08X}", st2));
        }

        info!("已发送 USB 速度切换命令");
        Ok(())
    }
}

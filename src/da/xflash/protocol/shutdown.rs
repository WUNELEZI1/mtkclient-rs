//! Protocol primitives — block 4: SHUTDOWN / XML DA commands (Layer 2 + Layer 3)

use super::{CMD_MAGIC, CMD_SHUTDOWN, SHUTDOWN_BOOTMODE_HOME_SCREEN, hex_str, pack3};
use log::{info, trace};

use crate::da::xml::XmlSession;
use crate::error::ProtocolError;

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// Layer 2: XFlash DA SHUTDOWN command (matches xflash_lib.py shutdown)
    ///
    /// Sends SHUTDOWN over the XFlash binary protocol with a bootmode that controls reboot behavior.
    ///
    /// Command format:
    ///   header: MAGIC(4) + SHUTDOWN_CMD(4) = 8 bytes
    ///   param body: 32 bytes (hasflags + enablewdt + async_mode + bootmode + reserved)
    ///   response: status(4)
    ///
    /// bootmode:
    ///   1 = HOME_SCREEN (reboot to system) — this project always uses this mode
    pub fn da_shutdown(&mut self) -> Result<(), String> {
        let bootmode = SHUTDOWN_BOOTMODE_HOME_SCREEN;
        info!("[DA SHUTDOWN] bootmode={} (重启到系统)", bootmode);

        // Send command header: MAGIC + CMD_SHUTDOWN
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader
            .device
            .write(&hdr)
            .map_err(|e| format!("SHUTDOWN write hdr: {}", e))?;
        self.preloader
            .device
            .write(&CMD_SHUTDOWN.to_le_bytes())
            .map_err(|e| format!("SHUTDOWN write cmd: {}", e))?;

        // Read stage 1 status
        let st = self.status()?;
        if st != 0 {
            return Err(format!(
                "SHUTDOWN 命令 {}",
                ProtocolError::StatusNonZero(st)
            ));
        }

        // Build the 32-byte param body — strictly matching mtkclient xflash_lib.py shutdown:
        //   pack("<IIIIIIII", hasflags, enablewdt, async_mode, bootmode,
        //        dl_bit, dont_resetrtc, leaveusb, 0)
        // Key fixes versus the old implementation (verified against mtkclient source):
        //   - enablewdt = 0 (watchdog disabled). MTK DA reboot-to-system is driven by bootmode:
        //     on SHUTDOWN the DA jumps straight to the next boot stage (preloader → system)
        //     and needs no watchdog hard reset. mtkclient fixes enablewdt=0 and reboots fine;
        //     the old code wrongly used enablewdt=1, so the watchdog hard reset sent the device
        //     (USB still connected) back to Preloader/download mode instead of system — the log
        //     said "reboot ok" but the device never reached the system.
        //   - Param body is 32 bytes (8×u32), field order strictly matching mtkclient (trailing reserved included).
        // Field order (all u32 LE):
        //   0x00 hasflags      1 for non-NORMAL mode / async / dl_bit, else 0
        //   0x04 enablewdt     0 = WDT disabled (DA jumps to reboot, no watchdog)
        //   0x08 async_mode    0
        //   0x0C bootmode      1 = HOME_SCREEN (reboot to system)
        //   0x10 dl_bit        0
        //   0x14 dont_resetrtc 0
        //   0x18 leaveusb      0 (drop USB after reboot so the device re-enumerates)
        //   0x1C reserved      0
        let async_mode: u32 = 0;
        let dl_bit: u32 = 0;
        let hasflags: u32 = if bootmode != 0 || async_mode != 0 || dl_bit != 0 {
            1
        } else {
            0
        };
        let enablewdt: u32 = 0; // WDT disabled: on SHUTDOWN the DA jumps straight to reboot-to-system (mtkclient fixes enablewdt=0)
        let mut param = [0u8; 32];
        param[0x00..0x04].copy_from_slice(&hasflags.to_le_bytes());
        param[0x04..0x08].copy_from_slice(&enablewdt.to_le_bytes());
        param[0x08..0x0C].copy_from_slice(&async_mode.to_le_bytes());
        param[0x0C..0x10].copy_from_slice(&bootmode.to_le_bytes());
        param[0x10..0x14].copy_from_slice(&dl_bit.to_le_bytes());
        param[0x14..0x18].copy_from_slice(&0u32.to_le_bytes()); // dont_resetrtc
        param[0x18..0x1C].copy_from_slice(&0u32.to_le_bytes()); // leaveusb
        // 0x1C..0x20 reserved 0

        trace!("[DA SHUTDOWN] param (32B): {}", hex_str(&param));

        // Send param body (32 bytes)
        let param_pkt = pack3(CMD_MAGIC, 0x01, 32);
        self.preloader
            .device
            .write(&param_pkt)
            .map_err(|e| format!("SHUTDOWN write param hdr: {}", e))?;
        self.preloader
            .device
            .write(&param)
            .map_err(|e| format!("SHUTDOWN write param: {}", e))?;

        // Read stage 2 status
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!(
                "SHUTDOWN 参数 {}",
                ProtocolError::StatusNonZero(st2)
            ));
        }

        info!(
            "[DA SHUTDOWN] 成功 (bootmode={}, enablewdt=0x{:02X})",
            bootmode, enablewdt
        );
        Ok(())
    }

    /// Layer 3: reboot into a given mode via the XML DA protocol (new platforms MT6789+)
    ///
    /// Flow: send `SET-BOOT-MODE(<mode>)` first, then `REBOOT(IMMEDIATE)`.
    /// Session primitives (CMD:START/END lifecycle, frame codec, ACK) are handled by
    /// [`crate::da::xml::XmlSession`], the same implementation used by the `xml` subcommand.
    fn da_xml_reboot_mode(&mut self, mode: &str) -> Result<(), String> {
        info!("[XML DA] SET-BOOT-MODE: {}", mode);
        let mut session = XmlSession::new(&mut *self.preloader.device);
        session
            .set_boot_mode(mode, "USB", "ON", "ON")
            .map_err(|e| format!("XML SET-BOOT-MODE 失败: {}", e))?;

        info!("[XML DA] REBOOT: IMMEDIATE");
        session
            .reboot(false)
            .map_err(|e| format!("XML REBOOT 失败: {}", e))?;

        info!("[XML DA] 设备将重启到 {}", mode);
        Ok(())
    }

    /// Layer 3: reboot to fastboot via the XML DA protocol (new platforms MT6789+)
    pub fn da_xml_reboot_fastboot(&mut self) -> Result<(), String> {
        self.da_xml_reboot_mode("FASTBOOT")
    }

    /// Layer 3: reboot to meta via the XML DA protocol (new platforms)
    pub fn da_xml_reboot_meta(&mut self) -> Result<(), String> {
        self.da_xml_reboot_mode("META")
    }
}

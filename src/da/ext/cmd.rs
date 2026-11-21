//! DA mode control interface
//!
//! - `set_meta`             — set the meta boot mode (usb / off)
//! - `enable_adb_and_reboot`— enable ADB in DA mode, then reboot into the system
//! - `cmd_peek`             — read device memory via a DA Extension
//! - `cmd_poke`             — write device memory via a DA Extension

use log::info;

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{DA_EXT_BOOT_ADDR, SET_META_BOOT_MODE};

impl<'a> DAXFlash<'a> {
    /// Set the meta boot mode
    /// Matches Python xflash_lib.py set_meta()
    pub fn set_meta(&mut self, mode: &str) -> Result<(), String> {
        let (boot_mode, com_type, com_id) = match mode {
            "usb" => (0x01u8, 0x02u8, 0x00u8),
            "off" => (0x00u8, 0x00u8, 0x00u8),
            _ => return Err(format!("Unknown meta mode: {}", mode)),
        };
        let data = vec![boot_mode, com_type, com_id];
        self.send_devctrl(SET_META_BOOT_MODE, Some(&data))?;
        info!("Meta boot 模式已设置为: {}", mode);
        Ok(())
    }

    /// Enable ADB in DA mode, then reboot into the system
    pub fn enable_adb_and_reboot(&mut self) -> Result<(), String> {
        info!("在 DA 模式下开启 ADB...");
        self.set_meta("usb")?;
        info!("重启设备进入系统...");
        self.boot_to(DA_EXT_BOOT_ADDR, &[], false, 0.0)?;
        Ok(())
    }

    /// Read device memory via a DA Extension
    ///
    /// Uses the XFlash READ_DATA protocol to read data directly at the flash address.
    /// Requires DA to be loaded (daext = true).
    ///
    /// Parameters:
    /// - `addr`: starting physical address to read
    /// - `size`: number of bytes to read
    pub fn cmd_peek(&mut self, addr: u64, size: u32) -> Result<Vec<u8>, String> {
        if !self.daext {
            return Err(format!(
                "{}，无法执行 peek。请确保 DA 已成功加载。",
                crate::error::SessionError::DaNotLoaded
            ));
        }

        info!("peek: 读取地址 0x{:08X}, 大小 {} 字节", addr, size);
        let data = self.readflash_data(addr, size as u64)?;
        info!("peek: 成功读取 {} 字节", data.len());
        Ok(data)
    }

    /// Write device memory via a DA Extension
    ///
    /// Uses the XFlash WRITE_DATA protocol to write directly to the flash address.
    /// Requires DA to be loaded (daext = true).
    ///
    /// Parameters:
    /// - `addr`: starting physical address to write
    /// - `data`: byte data to write
    pub fn cmd_poke(&mut self, addr: u64, data: &[u8]) -> Result<(), String> {
        if !self.daext {
            return Err(format!(
                "{}，无法执行 poke。请确保 DA 已成功加载。",
                crate::error::SessionError::DaNotLoaded
            ));
        }

        info!("poke: 写入地址 0x{:08X}, 大小 {} 字节", addr, data.len());
        self.write_flash_data(addr, data, 1, 8)?;
        info!("poke: 成功写入 {} 字节", data.len());
        Ok(())
    }
}

//! DA 模式控制接口
//!
//! - `set_meta`             — 设置 meta boot 模式（usb / off）
//! - `enable_adb_and_reboot`— 在 DA 模式下开启 ADB，然后重启进系统
//! - `cmd_peek`             — 通过 DA Extension 读取设备内存
//! - `cmd_poke`             — 通过 DA Extension 写入设备内存

use log::info;

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::SET_META_BOOT_MODE;

impl<'a> DAXFlash<'a> {
    /// 设置 meta boot 模式
    /// 对齐 Python xflash_lib.py set_meta()
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

    /// 在 DA 模式下开启 ADB，然后重启进系统
    pub fn enable_adb_and_reboot(&mut self) -> Result<(), String> {
        info!("在 DA 模式下开启 ADB...");
        self.set_meta("usb")?;
        info!("重启设备进入系统...");
        self.boot_to(0x4FFF0000, &[], false, 0.0)?;
        Ok(())
    }

    /// 通过 DA Extension 读取设备内存
    ///
    /// 使用 XFlash READ_DATA 协议直接读取 flash 地址处的数据。
    /// 需要 DA 已加载（daext = true）。
    ///
    /// 参数:
    /// - `addr`: 读取的起始物理地址
    /// - `size`: 读取的字节数
    pub fn cmd_peek(&mut self, addr: u64, size: u32) -> Result<Vec<u8>, String> {
        if !self.daext {
            return Err("DA Extension 未加载，无法执行 peek。请确保 DA 已成功加载。".to_string());
        }

        info!("peek: 读取地址 0x{:08X}, 大小 {} 字节", addr, size);
        let data = self.readflash_data(addr, size as u64)?;
        info!("peek: 成功读取 {} 字节", data.len());
        Ok(data)
    }

    /// 通过 DA Extension 写入设备内存
    ///
    /// 使用 XFlash WRITE_DATA 协议直接写入 flash 地址。
    /// 需要 DA 已加载（daext = true）。
    ///
    /// 参数:
    /// - `addr`: 写入的起始物理地址
    /// - `data`: 要写入的字节数据
    pub fn cmd_poke(&mut self, addr: u64, data: &[u8]) -> Result<(), String> {
        if !self.daext {
            return Err("DA Extension 未加载，无法执行 poke。请确保 DA 已成功加载。".to_string());
        }

        info!(
            "poke: 写入地址 0x{:08X}, 大小 {} 字节",
            addr,
            data.len()
        );
        self.write_flash_data(addr, data, 1, 8)?;
        info!("poke: 成功写入 {} 字节", data.len());
        Ok(())
    }
}
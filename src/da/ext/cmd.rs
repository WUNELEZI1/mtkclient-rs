//! DA 模式控制接口
//!
//! - `set_meta`             — 设置 meta boot 模式（usb / off）
//! - `enable_adb_and_reboot`— 在 DA 模式下开启 ADB，然后重启进系统

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
}

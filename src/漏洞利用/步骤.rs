//! Kamakiri2 单步 + da_setup
//!
//! - [`Preloader::kamakiri2_step`]：BROM 内存访问指针设置（libusb 走 ctrl_transfer exploit，串口 NO-OP）
//! - [`Preloader::da_setup`]：flush_input + brom_register_access 唤醒 + 3 次 kamakiri2_step

use log::trace;
use std::time::Duration;

use crate::预加载器::Preloader;

impl Preloader {
    /// Kamakiri2 单步：设置 BROM 内存访问指针
    ///
    /// libusb 路径：USB control transfer exploit（标准 kamakiri2）
    /// 串口路径：NO-OP（跳过 setup steps）
    ///
    /// 原因：刷机匣在串口模式下不执行 kamakiri2 setup steps，
    /// 直接使用 brom_register_access 完成内存读写。
    pub(crate) fn kamakiri2_step(
        &mut self,
        lc: &[u8],
        _ptr_da_bra: u32,
        addr: u32,
    ) -> Result<(), String> {
        trace!("[STEP] addr=0x{:08X}", addr);
        if self.device.is_libusb() {
            let mut d = lc.to_vec();
            d.extend(&addr.to_le_bytes());
            trace!("[STEP] payload({} bytes): {:02X?}", d.len(), d);
            trace!("[STEP]   linecode: {:02X?}", lc);
            trace!("[STEP]   addr_le: {:02X?}", addr.to_le_bytes());
            // 忽略错误，对齐 Python 的 try-except（允许 step 失败）
            // 注意：Python 的 kamakiri2 函数没有 sleep，这里也移除以对齐
            let _ = self.device.ctrl_transfer_out(0x21, 0x20, 0, 0, &d);
            let _ = self.device.ctrl_transfer_in(0x80, 0x06, 0x02FF, 0, 9);
        } else {
            trace!("[STEP] serial mode — skipping kamakiri2 setup step");
        }
        Ok(())
    }

    pub(crate) fn da_setup(
        &mut self,
        lc: &[u8],
        ptr_da_bra: u32,
        watchdog: u32,
    ) -> Result<(), String> {
        trace!(
            "[da_setup] ENTER: ptr_da_bra=0x{:08X}, watchdog=0x{:08X}",
            ptr_da_bra, watchdog
        );

        // 新增：多次彻底清理
        self.flush_input();
        std::thread::sleep(Duration::from_millis(30));

        // 对齐 Python da_read_write：先尝试 brom_register_access(0, 1) 和 read32(watchdog+0x50)
        // Python 用 try-except 包裹，失败时忽略
        // 这些调用可能会"唤醒"设备的 BROM 协议处理或清除某些状态
        trace!("[da_setup] trying brom_register_access(0, 1) and read32(watchdog+0x50)");
        let _ = self.brom_register_access(0, 0, 1, None, false);
        let _ = self.read32_brom(watchdog.wrapping_add(0x50), 1);
        self.flush_input(); // 新增

        // 再次清理
        self.flush_input();

        // libusb 路径：执行 kamakiri2 steps 设置指针
        if self.device.is_libusb() {
            trace!("[da_setup] executing 3 kamakiri2 steps");
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_add(5))?;
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_add(6))?;
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_add(7))?;
            trace!("[da_setup] 3 kamakiri2 steps completed");
        }
        trace!("[da_setup] EXIT");
        Ok(())
    }
}

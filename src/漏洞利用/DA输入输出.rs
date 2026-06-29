//! Kamakiri2 DA 内存读写
//!
//! - [`Preloader::da_read`]：通过 kamakiri2 漏洞从 BROM 内存读取
//! - [`Preloader::da_write`]：通过 kamakiri2 漏洞向 BROM 内存写入
//! - [`Preloader::read_payload_address`]：读 ptr_send 寄存器

use log::trace;
use std::time::Duration;

use crate::预加载器::Preloader;

/// 4 字节小端 → u32
fn unpack_u32(data: &[u8]) -> u32 {
    if data.len() < 4 {
        return 0;
    }
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}

impl Preloader {
    pub(crate) fn da_read(
        &mut self,
        lc: &[u8],
        ptr_da_bra: u32,
        _ptr_da: u32,
        watchdog: u32,
        addr: u32,
        len: u32,
    ) -> Result<Vec<u8>, String> {
        trace!("[da_read] addr=0x{:08X} len={}", addr, len);

        // 串口路径：调用 da_setup（reset + watchdog），不执行 kamakiri2 steps
        if !self.device.is_libusb() {
            self.da_setup(lc, ptr_da_bra, watchdog)?;
            if addr < 0x40 {
                let r = self.brom_register_access(0, addr, len, None, true)?;
                Ok(r.unwrap_or_default())
            } else {
                let bra_addr = addr.wrapping_sub(0x40); // 对齐 Python：addr - 0x40
                trace!("[da_read] bra_addr=0x{:08X}", bra_addr);
                let r = self.brom_register_access(0, bra_addr, len, None, true)?;
                Ok(r.unwrap_or_default())
            }
        } else {
            // libusb 路径：da_setup 前清理 + da_setup 后清理
            self.flush_input();
            std::thread::sleep(Duration::from_millis(50));
            self.da_setup(lc, ptr_da_bra, watchdog)?;
            self.flush_input(); // 加强清理

            if addr < 0x40 {
                // addr < 0x40: 4 additional steps
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(2))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(3))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(4))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(5))?;
                // 对齐 Python：steps 后直接调用 brom_register_access，没有 sleep
                let r = self.brom_register_access(0, addr, len, None, true)?;
                Ok(r.unwrap_or_default())
            } else {
                // addr >= 0x40: 3 additional steps (-2, -3, -4), then use addr - 0x40
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(2))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(3))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(4))?;
                let bra_addr = addr.wrapping_sub(0x40);
                trace!(
                    "[da_read] using bra_addr=0x{:08X} (libusb path, addr-0x40)",
                    bra_addr
                );
                // 对齐 Python：steps 后直接调用 brom_register_access，没有 sleep
                let r = self.brom_register_access(0, bra_addr, len, None, true)?;
                Ok(r.unwrap_or_default())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn da_write(
        &mut self,
        lc: &[u8],
        ptr_da_bra: u32,
        _ptr_da: u32,
        watchdog: u32,
        addr: u32,
        data: &[u8],
        check_status: bool,
    ) -> Result<(), String> {
        trace!(
            "[da_write] addr=0x{:08X} len={} data_head={:02X?}",
            addr,
            data.len(),
            &data[..std::cmp::min(data.len(), 16)]
        );

        // 串口路径：调用 da_setup（reset + watchdog），但跳过 kamakiri2 steps
        if !self.device.is_libusb() {
            self.da_setup(lc, ptr_da_bra, watchdog)?;
            if addr < 0x40 {
                trace!("[da_write] bra_addr=0x{:08X} (no offset)", addr);
                self.brom_register_access(1, addr, data.len() as u32, Some(data), check_status)?;
                Ok(())
            } else {
                let bra_addr = addr.wrapping_sub(0x40); // 对齐 Python：addr - 0x40
                trace!("[da_write] bra_addr=0x{:08X}", bra_addr);
                self.brom_register_access(
                    1,
                    bra_addr,
                    data.len() as u32,
                    Some(data),
                    check_status,
                )?;
                Ok(())
            }
        } else {
            // libusb 路径：da_setup 前清理 + da_setup 后清理
            self.flush_input();
            std::thread::sleep(Duration::from_millis(50));
            self.da_setup(lc, ptr_da_bra, watchdog)?;
            self.flush_input(); // 加强清理

            if addr < 0x40 {
                // addr < 0x40：4 步
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(2))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(3))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(4))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(5))?;
                trace!("[da_write] bra_addr=0x{:08X} (no offset)", addr);
                std::thread::sleep(Duration::from_millis(50));
                self.brom_register_access(1, addr, data.len() as u32, Some(data), check_status)?;
                Ok(())
            } else {
                // addr >= 0x40：额外 3 步 (-2, -3, -4)，然后使用 addr - 0x40
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(2))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(3))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(4))?;
                let bra_addr = addr.wrapping_sub(0x40);
                trace!(
                    "[da_write] using bra_addr=0x{:08X} (libusb path, addr-0x40)",
                    bra_addr
                );
                std::thread::sleep(Duration::from_millis(50));
                self.brom_register_access(
                    1,
                    bra_addr,
                    data.len() as u32,
                    Some(data),
                    check_status,
                )?;
                Ok(())
            }
        }
    }

    pub(crate) fn read_payload_address(
        &mut self,
        lc: &[u8],
        ptr_da_bra: u32,
        ptr_da: u32,
        watchdog: u32,
    ) -> Result<u32, String> {
        let send_ptr_addr = self.ptr_send_addr();
        trace!(
            "[read_payload_address] send_ptr_addr=0x{:08X}",
            send_ptr_addr
        );
        trace!("[read_payload_address] calling da_read to read from device memory");
        let ptr_send_data = self.da_read(lc, ptr_da_bra, ptr_da, watchdog, send_ptr_addr, 4)?;
        let raw_value = unpack_u32(&ptr_send_data);
        let ptr_send = raw_value + 8;
        trace!(
            "[read_payload_address] raw=0x{:08X}, +8=0x{:08X}, bra_addr will be 0x{:08X}",
            raw_value,
            ptr_send,
            ptr_send.wrapping_sub(0x40)
        );
        Ok(ptr_send)
    }
}

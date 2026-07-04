//! BROM read32 协议实现
//!
//! - `read32_brom`      — 单次读取（含 flush + 延迟）
//! - `read32_brom_batch`— 批量读取（跳过 flush）
//! - `read32_brom_inner`— 核心协议：cmd → addr → len → status1 → data → status2

use super::core::Preloader;
use log::trace;
use std::time::Duration;

const READ32_PRE_DELAY_MS: u64 = 5;
const READ32_LIBUSB_DELAY_MS: u64 = 5;

impl Preloader {
    /// 读 32 位值（BROM 模式）
    /// 使用 0xD1 协议（无 mode 参数）：cmd → addr → len(dwords) → status1 → data → status2
    pub fn read32_brom(&mut self, addr: u32, dwords: usize) -> Result<Vec<u8>, String> {
        // 读数据前强制 flush + 小延迟
        self.flush_input();
        std::thread::sleep(Duration::from_millis(READ32_PRE_DELAY_MS));

        if self.device.is_libusb() {
            // 注意：不要在此处调用 clear_halt（会触发驱动超时）
            std::thread::sleep(Duration::from_millis(READ32_LIBUSB_DELAY_MS));
        }

        self.read32_brom_inner(addr, dwords)
    }

    /// 批量版 read32_brom，跳过 flush_input 和延迟。
    /// 用于已确认设备状态正常的连续循环读取（如 dump_preloader）。
    pub fn read32_brom_batch(&mut self, addr: u32, dwords: usize) -> Result<Vec<u8>, String> {
        self.read32_brom_inner(addr, dwords)
    }

    /// read32_brom 的核心实现，不含 flush/delay 前置步骤
    fn read32_brom_inner(&mut self, addr: u32, dwords: usize) -> Result<Vec<u8>, String> {
        trace!("[read32_brom] 发送命令 0xD1");
        if !self.echo_1byte(0xD1)? {
            trace!("read32_brom: echo 0xD1 不匹配（继续尝试）");
        }
        trace!("[read32_brom] 发送地址 0x{:08X}", addr);
        if !self.echo_4byte(addr)? {
            trace!("read32_brom: echo addr 不匹配（继续尝试）");
        }
        trace!("[read32_brom] 发送长度 {} dwords", dwords);
        if !self.echo_4byte(dwords as u32)? {
            trace!("read32_brom: echo len 不匹配（继续尝试）");
        }

        let mut st = [0u8; 2];
        self.device
            .read_exact(&mut st)
            .map_err(|e| format!("read32_brom status1: {}", e))?;
        trace!("read32_brom status1: {:02X?}", st);

        let bytes = dwords * 4;
        let mut rdata = vec![0u8; bytes];
        self.device
            .read_exact(&mut rdata)
            .map_err(|e| format!("read32_brom data ({} bytes): {}", bytes, e))?;
        trace!("read32_brom read data: {} bytes", rdata.len());

        let mut st2 = [0u8; 2];
        self.device
            .read_exact(&mut st2)
            .map_err(|e| format!("read32_brom status2: {}", e))?;
        trace!("read32_brom status2: {:02X?}", st2);
        Ok(rdata)
    }
}

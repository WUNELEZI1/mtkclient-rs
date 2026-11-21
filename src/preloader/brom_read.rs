//! BROM read32 protocol implementation
//!
//! - `read32_brom`      — single read (includes flush + delay, used for the first scan)
//! - `read32_brom_batch`— batch read (skips flush, used for consecutive reads)
//! - `read32_brom_inner`— core protocol: cmd → addr → len → status1 → data → status2
//!
//! # Performance notes
//! - `read32_brom` performs flush + delay only on the first call; later reads use `read32_brom_batch`
//! - `flush_input` timeout was reduced from 200ms to 50ms to cut idle waiting
//! - batch reads skip all flush/latency overhead

use super::core::Preloader;
use log::trace;

impl Preloader {
    /// Read a 32-bit value (BROM mode, for the first scan)
    /// Uses the 0xD1 protocol (no mode parameter): cmd → addr → len(dwords) → status1 → data → status2
    ///
    /// A quick flush runs only on the first call, ensuring the device is in a clean state.
    /// For subsequent consecutive reads use `read32_brom_batch` to skip flush overhead.
    ///
    /// ⚠️ No sleep delay is inserted any more:
    ///    - USB devices respond in microseconds; a 2ms delay adds up (9 addresses × 2ms = 18ms lost)
    ///    - Serial devices rely on a 200ms timeout and need no extra delay
    pub fn read32_brom(&mut self, addr: u32, dwords: usize) -> Result<Vec<u8>, String> {
        // Quick flush: discard residual data with a short timeout (~20ms × 4 = 80ms)
        self.flush_input_quick();
        // No sleep delay — USB devices respond very fast and serial has a timeout fallback
        self.read32_brom_inner(addr, dwords)
    }

    /// Batch version of read32_brom; skips flush_input and the delay.
    /// Used for consecutive polling loops on a device already known to be healthy (e.g. dump_preloader).
    pub fn read32_brom_batch(&mut self, addr: u32, dwords: usize) -> Result<Vec<u8>, String> {
        self.read32_brom_inner(addr, dwords)
    }

    /// Core implementation of read32_brom without the flush/delay preamble
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

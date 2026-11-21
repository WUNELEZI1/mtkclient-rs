//! Remaining primitives of the `Preloader` initialization flow
//!
//! The second `impl Preloader` block split out of `mod.rs`, containing:
//! - `get_hw_subcode` reads the HW Subcode
//! - `flush_input` / `flush_input_quick` / `flush_input_poll` input buffer cleanup
//! - `rword` basic 16-bit word reader

use super::{
    FLUSH_INPUT_CHUNK, FLUSH_INPUT_MAX_ITER, FLUSH_INPUT_QUICK_CHUNK, FLUSH_INPUT_QUICK_MAX_ITER,
    FLUSH_INPUT_QUICK_TIMEOUT_MS, FLUSH_INPUT_TIMEOUT_MS, POST_FLUSH_TIMEOUT_MS,
};
use crate::preloader::core::Preloader;
use log::trace;
use std::time::Duration;

impl Preloader {
    /// Read the HW Subcode
    /// Python: echo(0xDB) → rbyte(2)
    pub fn get_hw_subcode(&mut self) -> Result<u16, String> {
        if !self.sendcmd(0xDB)? {
            return Err("获取 HW subcode 失败: echo 0xDB 不匹配".into());
        }
        let mut buf = [0u8; 2];
        self.device
            .read_exact(&mut buf)
            .map_err(|e| format!("read hw subcode: {}", e))?;
        Ok(u16::from_be_bytes(buf))
    }

    /// Flush the input buffer (in serial mode, discard all pending data to avoid misalignment after an echo mismatch)
    pub fn flush_input(&mut self) {
        self.device
            .set_timeout(Duration::from_millis(FLUSH_INPUT_TIMEOUT_MS));
        let mut trash = [0u8; FLUSH_INPUT_CHUNK];
        let mut total = 0;
        for _ in 0..FLUSH_INPUT_MAX_ITER {
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
        self.device
            .set_timeout(Duration::from_millis(POST_FLUSH_TIMEOUT_MS));
        if total > 0 {
            trace!("[FLUSH] total discarded {} bytes", total);
        }
    }

    /// Fast flush: short timeout (50ms), used before read32_brom
    /// 4x faster than the regular flush; suits cases where the device state is known to be healthy
    pub(crate) fn flush_input_quick(&mut self) {
        self.device
            .set_timeout(Duration::from_millis(FLUSH_INPUT_QUICK_TIMEOUT_MS));
        let mut trash = [0u8; FLUSH_INPUT_QUICK_CHUNK];
        let mut total = 0;
        for _ in 0..FLUSH_INPUT_QUICK_MAX_ITER {
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
        self.device
            .set_timeout(Duration::from_millis(POST_FLUSH_TIMEOUT_MS));
        if total > 0 {
            trace!("[FLUSH_QUICK] discarded {} bytes", total);
        }
    }

    /// Polling flush: read repeatedly with a short timeout until the USB buffer is empty (device ready)
    /// Faster than a fixed sleep — return as soon as the device is ready
    pub fn flush_input_poll(&mut self, interval: Duration, max_iters: u32) -> Result<(), String> {
        self.device.set_timeout(interval);
        let mut trash = [0u8; FLUSH_INPUT_CHUNK];
        let mut total = 0;
        for i in 0..max_iters {
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => {
                    // Buffer empty, device ready
                    break;
                }
            }
            if i == max_iters - 1 {
                trace!("[FLUSH_POLL] 轮询 {} 次, 丢弃 {} bytes", max_iters, total);
            }
        }
        self.device
            .set_timeout(Duration::from_millis(POST_FLUSH_TIMEOUT_MS));
        Ok(())
    }

    /// Read a 16-bit word (2 bytes, big-endian, matching Python DeviceHandler.rword(little=False))
    pub fn rword(&mut self) -> Result<u16, String> {
        let mut buf = [0u8; 2];
        self.device.read_exact(&mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }
}

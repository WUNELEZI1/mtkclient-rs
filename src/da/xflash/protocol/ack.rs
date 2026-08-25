//! 协议原语 — 块 2: ACK 读写

use super::{AckResult, CMD_MAGIC, pack3};
use log::trace;

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// 发送 ACK 并读取设备响应
    /// 写入 12B header(CMD_MAGIC + 0x01 + 4) + 4B 零值 → 读取 status
    /// 返回 AckResult::Continue（可继续）或 AckResult::Terminated（终止）
    pub(crate) fn ack(&mut self) -> AckResult {
        if let Err(e) = self.send_ack() {
            trace!("[ack] send_ack failed: {}", e);
            return AckResult::Terminated(1);
        }
        match self.status() {
            Ok(0) => AckResult::Continue,
            Ok(n) => AckResult::Terminated(n),
            Err(_) => AckResult::Terminated(3),
        }
    }

    fn send_ack(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        let zero = 0u32.to_le_bytes();

        // 所有芯片统一使用 16B 单包发送 ACK，避免两次 USB OUT transfer 的调度开销
        let mut buf = [0u8; 16];
        buf[..12].copy_from_slice(&hdr);
        buf[12..16].copy_from_slice(&zero);
        self.preloader
            .device
            .write(&buf)
            .map_err(|e| format!("send_ack write 16B: {}", e))?;
        Ok(())
    }
}

//! Protocol primitives — block 2: ACK read/write

use super::{AckResult, CMD_MAGIC, pack3};
use crate::error::ProtocolError;
use log::trace;

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// Send an ACK and read the device response.
    /// Writes a 12B header (CMD_MAGIC + 0x01 + 4) + 4B zero → reads status.
    /// Returns AckResult::Continue (may proceed) or AckResult::Terminated (abort).
    pub(crate) fn ack(&mut self) -> AckResult {
        if let Err(e) = self.send_ack() {
            trace!("[ack] send_ack failed: {}", e);
            return AckResult::Terminated(1);
        }
        match self.status() {
            Ok(0) => AckResult::Continue,
            Ok(n) => {
                trace!("[ack] {}", ProtocolError::AckRejected(n));
                AckResult::Terminated(n)
            }
            Err(_) => AckResult::Terminated(3),
        }
    }

    fn send_ack(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        let zero = 0u32.to_le_bytes();

        // All chips send the ACK as a single 16B packet to avoid the overhead of two USB OUT transfers
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

use super::{PortError, StreamPort, VirtualDevice};

impl StreamPort for VirtualDevice {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError> {
        if self.dead {
            return Err(PortError::Disconnected);
        }
        if self.silent {
            // Silence = cable unplugged, semantically equivalent to "disconnected": fail immediately so the upper layer triggers resume / prompts replug,
            // it must not be treated as a retryable transient timeout (otherwise it would retry endlessly on a dead device and wait in vain).
            return Err(PortError::Disconnected);
        }
        // Chaos: transient timeout (not latched, recovers on next read) / silence (latched until replug).
        let mut go_silent = false;
        if let Some(c) = self.chaos.as_mut() {
            if c.rng.hit(c.cfg.glitch_permille) {
                c.glitches += 1;
                return Err(PortError::Timeout);
            }
            if c.rng.hit(c.cfg.silence_permille) {
                c.silences += 1;
                go_silent = true;
            }
        }
        if go_silent {
            self.silent = true;
            return Err(PortError::Disconnected);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        if self.out.is_empty() {
            return Err(PortError::Timeout);
        }
        let m = buf.len().min(self.out.len());
        // Chaos: packet drop — m bytes are still "sent on the wire", but the last d bytes are lost and the host receives only deliver bytes.
        //
        // Only applies to data blocks (≥16 bytes): a real USB short packet does not "drop trailing bytes"; control frames
        // (handshake / command echo / status word, 1~8 bytes) losing bytes would only make the host wait forever,
        // which is physically unrealistic. Packet drop models a large data transfer being truncated by a bad cable / interference.
        let mut deliver = m;
        if let Some(c) = self.chaos.as_mut() {
            if m >= 16 && c.rng.hit(c.cfg.drop_permille) {
                c.drops += 1;
                let d = 1 + (c.rng.next_u64() as usize % (m - 1));
                deliver = m - d;
            }
        }
        for (i, b) in self.out.drain(..m).enumerate() {
            if i < deliver {
                buf[i] = b;
            }
        }
        Ok(deliver)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, PortError> {
        for &b in buf {
            self.feed(b)?;
        }
        if self.dead {
            return Err(PortError::Disconnected);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), PortError> {
        Ok(())
    }

    /// Discard unconsumed input bytes (resync after frame misalignment).
    fn drain(&mut self) -> Result<(), PortError> {
        self.out.clear();
        Ok(())
    }
}

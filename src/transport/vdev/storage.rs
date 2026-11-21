use super::wire;
use super::{DeviceFault, PAGE, PortError, VirtualDevice};

impl VirtualDevice {
    /// Read a flash range: check sparse pages (written/erased content) first, then fall back to the GPT image.
    pub(super) fn read_window(&self, addr: u64, size: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(size as usize);
        let mut pos = addr;
        let end = addr + size;
        while pos < end {
            let page = pos / PAGE;
            let off = (pos % PAGE) as usize;
            let take = ((PAGE - off as u64).min(end - pos)) as usize;
            let buf = self.page_bytes(page);
            out.extend_from_slice(&buf[off..off + take]);
            pos += take as u64;
        }
        out
    }

    /// Whether the address falls within an erased range.
    fn is_erased(&self, addr: u64) -> bool {
        self.erased.iter().any(|(s, e)| addr >= *s && addr < *e)
    }

    /// Get the current content of one page (4 KiB): sparse page first → erased range (0xFF) → GPT image → 0x00.
    fn page_bytes(&self, page: u64) -> Vec<u8> {
        if let Some(v) = self.pages.get(&page) {
            return v.clone();
        }
        let mut v = vec![0u8; PAGE as usize];
        if self.is_erased(page * PAGE) {
            v.fill(0xFF);
            return v;
        }
        let base = page * PAGE;
        if base < self.image.len() as u64 {
            let end = (base + PAGE).min(self.image.len() as u64);
            v[..(end - base) as usize].copy_from_slice(&self.image[base as usize..end as usize]);
        }
        v
    }

    /// Write data into sparse pages (automatic read-modify-write, keeping other bytes in the page intact).
    pub(super) fn write_range(&mut self, addr: u64, data: &[u8]) {
        let mut pos = addr;
        let end = addr + data.len() as u64;
        let mut src = 0usize;
        while pos < end {
            let page = pos / PAGE;
            let off = (pos % PAGE) as usize;
            let take = ((PAGE - off as u64).min(end - pos)) as usize;
            let mut buf = self.page_bytes(page);
            buf[off..off + take].copy_from_slice(&data[src..src + take]);
            self.pages.insert(page, buf);
            pos += take as u64;
            src += take;
        }
    }

    /// Erase a range: real hardware reads back 0xFF after erase.
    ///
    /// First drop any written pages in the range, then record one erased range (merging adjacent ranges),
    /// so erasing 1 GiB takes only a few records instead of 260,000 4 KiB pages.
    pub(super) fn erase_range(&mut self, addr: u64, size: u64) {
        let end = addr + size;
        let (p0, p1) = (addr / PAGE, end.div_ceil(PAGE));
        self.pages.retain(|p, _| *p < p0 || *p >= p1);
        self.erased.push((addr, end));
        // Merge adjacent/overlapping ranges
        self.erased.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.erased.len());
        for (s, e) in self.erased.drain(..) {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.erased = merged;
    }

    /// Total number of bytes written (for diagnostics).
    #[allow(dead_code)]
    pub fn written_bytes(&self) -> u64 {
        self.total_written
    }

    /// Write transaction: handle the three parameter packets `[0u32][checksum][data]`.
    /// Returns Ok(true) if the payload was consumed by the write transaction.
    pub(super) fn on_write_param(&mut self, payload: &[u8]) -> Result<bool, PortError> {
        let Some(mut ws) = self.write.take() else {
            return Ok(false);
        };
        match ws.stage {
            0 => {
                if payload.len() == 4 && payload.iter().all(|b| *b == 0) {
                    ws.stage = 1;
                    self.write = Some(ws);
                    return Ok(true);
                }
            }
            1 => {
                if payload.len() == 4 {
                    ws.checksum = u32::from_le_bytes(payload[..4].try_into().unwrap());
                    ws.stage = 2;
                    self.write = Some(ws);
                    return Ok(true);
                }
            }
            _ => {
                if payload.len() % 512 != 0 {
                    self.ev(format!(
                        "WRITE_DATA: 分块未对齐 512（{} 字节）",
                        payload.len()
                    ));
                    self.push_status(wire::XFLASH_STATUS_CHECKSUM);
                    return Ok(true);
                }
                // Checksum (aligned with mtkclient: sum(data) & 0xFFFF)
                let sum = (payload.iter().map(|b| *b as u32).sum::<u32>()) & 0xFFFF;
                if sum != ws.checksum {
                    self.ev(format!(
                        "WRITE_DATA: checksum 不匹配 期望 0x{:04X} 实际 0x{:04X}",
                        ws.checksum, sum
                    ));
                    self.push_status(wire::XFLASH_STATUS_CHECKSUM);
                    return Ok(true);
                }
                // Mid-write fault injection
                self.total_written += payload.len() as u64;
                let want_dead = self
                    .faults
                    .iter()
                    .find_map(|(_, k)| match k {
                        DeviceFault::WriteDead(at) if self.total_written >= *at => Some(true),
                        _ => None,
                    })
                    .unwrap_or(false);
                if want_dead {
                    self.dead = true;
                    self.ev(format!("fault: write dead at {} bytes", self.total_written));
                    return Err(PortError::Disconnected);
                }
                let want_silent = self.faults.iter().any(
                    |(_, k)| matches!(k, DeviceFault::WriteSilent(at) if self.total_written >= *at),
                );
                if want_silent {
                    self.silent = true;
                    self.ev(format!(
                        "fault: write silent at {} bytes",
                        self.total_written
                    ));
                    return Ok(true);
                }

                // Chaos: bit flip. Deliberately placed after the checksum passes — simulates "transferred correctly, corrupted on disk",
                // so only host-side read-back verification / CRC can catch it; a tool that silently accepts is exposed by this injection.
                match self.chaos.as_mut().and_then(|c| c.dirty(payload.len())) {
                    Some((idx, bit)) => {
                        let mut dirty = payload.to_vec();
                        dirty[idx] ^= 1 << bit;
                        self.ev(format!("chaos: bitflip @写块偏移 {} 位 {}", idx, bit));
                        self.write_range(ws.addr + ws.written, &dirty);
                    }
                    None => self.write_range(ws.addr + ws.written, payload),
                }
                ws.written += payload.len() as u64;
                ws.stage = 0;
                self.push_status(0);
                if ws.written >= ws.size {
                    // Transaction-final status (read by the host end_write)
                    self.push_status(0);
                    self.ev(format!(
                        "WRITE_DATA 完成: 0x{:X} + {} 字节",
                        ws.addr, ws.written
                    ));
                    self.write = None;
                } else {
                    self.write = Some(ws);
                }
                return Ok(true);
            }
        }
        // Unexpected: reject the payload and terminate the write transaction
        self.ev(format!(
            "WRITE_DATA: 非预期参数 len={} stage={}",
            payload.len(),
            ws.stage
        ));
        self.push_status(wire::XFLASH_STATUS_CHECKSUM);
        Ok(true)
    }

    /// FORMAT ack handling: first ack → COMPLETE; second ack → final 0.
    pub(super) fn on_format_ack(&mut self, payload: &[u8]) -> Result<(), PortError> {
        if payload.len() != 4 {
            return Ok(());
        }
        match self.format_stage {
            1 => {
                self.push_status(wire::XFLASH_STATUS_COMPLETE);
                self.format_stage = 2;
            }
            _ => {
                self.push_status(0);
                self.format_stage = 0;
                self.ev("FORMAT 完成");
            }
        }
        Ok(())
    }

    pub(super) fn emmc_info_payload(&self) -> Vec<u8> {
        // 104 bytes, strictly aligned with mtkclient `get_emmc_info` parsing order:
        //   0 type(4) | 4 block(4) | 8 boot1(8) | 16 boot2(8) | 24 rpmb(8)
        //   32 gp1 | 40 gp2 | 48 gp3 | 56 gp4 | **64 user(8)** | 72 cid(16) | 88 fwver(8)
        // Note: user_size is at 0x40, not 0x20 (0x20 is gp1),
        // early implementations wrote both at 0x20, "self-consistent but not matching real hardware".
        let mut v = vec![0u8; 104];
        v[0..4].copy_from_slice(&self.profile.storage.emmc_type.to_le_bytes());
        v[4..8].copy_from_slice(&self.profile.storage.block.to_le_bytes());
        v[8..16].copy_from_slice(&self.profile.storage.boot1.to_le_bytes());
        v[16..24].copy_from_slice(&self.profile.storage.boot2.to_le_bytes());
        v[24..32].copy_from_slice(&self.profile.storage.rpmb.to_le_bytes());
        v[64..72].copy_from_slice(&self.profile.storage.user.to_le_bytes());
        v
    }
}

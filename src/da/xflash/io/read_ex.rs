//! In-memory read variant (readflash_data_ex, reads everything into memory).
//!
//! See the [`crate::da::xflash::io`] module docs.

use log::trace;

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{CMD_MAGIC, GET_PKT_LEN};

use super::{
    DA_READ_PER_CHUNK_TRAILER, READFLASH_READ_TIMEOUT_MS, parse_packet_length, with_read_timeout,
};

impl<'a> DAXFlash<'a> {
    /// Read flash data fully into memory; for small partitions or in-memory processing.
    pub(crate) fn readflash_data_ex(
        &mut self,
        addr: u64,
        size: u64,
        parttype: u32,
    ) -> Result<Vec<u8>, String> {
        // Guard: never read a huge partition entirely into memory (avoids OOM crashes).
        // Use streaming reads for large partitions (readflash_to_file / r `<part>` `<file>` / rl `<dir>`).
        const MAX_IN_MEMORY_READ: u64 = 256 * 1024 * 1024; // 256 MiB
        if size > MAX_IN_MEMORY_READ {
            return Err(format!(
                "分区过大 ({:.2} GiB)，无法整块读入内存。请使用流式读取：r <分区> <文件> 或 rl <目录>",
                size as f64 / 1024.0 / 1024.0 / 1024.0
            ));
        }

        // 1. get_packet_length — send_devctrl already performs the full xread + status handshake.
        //    Matches Python readflash: **GET only, never SET**.
        //    * 0x040008 is actually GET_RANDOM_ID (see the mtkclient Cmd table); an older
        //      implementation treated it as SET_PKT_LEN and sent it with arguments, corrupting
        //      the DA state machine so the following READ_DATA returned 0xC0010003 (Unsupported
        //      command) — this is what made printgpt fail on real devices.
        //    Some devices/DA return an unexpected status for GET_PKT_LEN, but the read loop
        //    follows the actual packet split returned by the device, so do not abort on failure;
        //    this avoids needless failures of read-only commands such as printgpt.
        if let Ok(data) = self.send_devctrl(GET_PKT_LEN, None) {
            if let Some(read_plen) = parse_packet_length(&data) {
                trace!("[readflash_data_ex] DA 读包长度 {} 字节", read_plen);
            }
        }

        // 2. Send the READ_DATA command and its parameters
        self.send_read_data_cmd(addr, size, parttype)?;

        // 3. Data read loop (into memory).
        // Wrap the whole read with a longer timeout (the serial default is only 1s; streaming would time out).
        with_read_timeout(self, READFLASH_READ_TIMEOUT_MS, |da| {
            let mut buffer = Vec::with_capacity(size as usize);
            let mut remaining = size as usize;

            while remaining > 0 {
                let mut hdr = [0u8; 12];
                match da.preloader.device.read_exact(&mut hdr) {
                    Ok(0) => {
                        trace!("[readflash_data] ZLP on header read, ending loop");
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        trace!("[readflash_data] read header error: {}", e);
                        break;
                    }
                }

                let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                let slength = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);

                if magic != CMD_MAGIC {
                    trace!(
                        "[readflash_data] bad magic: 0x{:08X} at offset {}, ending loop",
                        magic,
                        buffer.len()
                    );
                    break;
                }

                if slength == 4 {
                    // 4-byte small packet/heartbeat: stack buffer to avoid per-packet heap allocation; all-zero heartbeats are skipped and not counted
                    let mut tiny = [0u8; 4];
                    if let Err(e) = da.preloader.device.read_exact(&mut tiny) {
                        trace!("[readflash_data] read data(small) error: {}", e);
                        break;
                    }
                    if tiny.iter().all(|&b| b == 0) {
                        trace!("[readflash_data] 心跳包，跳过");
                        continue;
                    }
                    buffer.extend_from_slice(&tiny);
                    remaining = remaining.saturating_sub(4);
                } else if slength == 0 {
                    // Zero-length data/terminator packet: mtkclient ends the read when length==0; do the
                    // same here so a non-advancing `remaining` cannot loop forever.
                    trace!("[readflash_data] 零长数据包，结束读取");
                    break;
                } else {
                    // Normal data: read straight into the buffer tail, eliminating a per-packet vec! allocation
                    let slen = slength as usize;
                    // The DA appends a fixed-length trailer to every data chunk (DA_READ_PER_CHUNK_TRAILER bytes of status/checksum).
                    // * Truncate by "real data length = slen - trailer" and drop the trailer for EVERY chunk
                    //   (last or not): if an intermediate chunk's trailer is written as data, the whole stream
                    //   shifts by 8 bytes per chunk, breaking GPT entry CRC and making printgpt read empty data.
                    // Commit 03071dd only fixed the last chunk ("slen > remaining"); multi-chunk transfers stayed
                    // misaligned and are corrected here.
                    let data_len = slen.saturating_sub(DA_READ_PER_CHUNK_TRAILER);
                    let take = data_len.min(remaining);
                    let discard = slen - take;
                    let old_len = buffer.len();
                    buffer.resize(old_len + take, 0);
                    if take > 0
                        && let Err(e) = da.preloader.device.read_exact(&mut buffer[old_len..])
                    {
                        trace!("[readflash_data] read data error: {}", e);
                        buffer.truncate(old_len);
                        break;
                    }
                    if discard > 0 {
                        let mut junk = vec![0u8; discard];
                        if let Err(e) = da.preloader.device.read_exact(&mut junk) {
                            trace!("[readflash_data] drain trailer error: {}", e);
                            break;
                        }
                    }
                    remaining = remaining.saturating_sub(take);
                }

                if let Err(e) = da.ack_silent() {
                    trace!("[readflash_data] send_ack failed: {}", e);
                    break;
                }
                if remaining == 0 {
                    trace!("[readflash_data] 最后一包 ACK 已发送，等待最终状态");
                    break;
                }
            }

            // Integrity check: (1) the device may end early (ZLP / read error / bad magic / ACK failure);
            // (2) heartbeat packets (slength==4, all zero) and zero-length packets are skipped via
            // continue and never counted into the buffer.
            // If the real byte count received is < size, returning silently would hand upper layers
            // (GPT parsing, preloader extraction, partition verification, ...) an incomplete
            // "false success", hence the explicit error. If > size (trailer not fully dropped in an
            // extreme case), truncate to size to avoid dirty data.
            if buffer.len() < size as usize {
                return Err(format!(
                    "读取数据不完整: 期望 {} 字节，实际仅收到 {} 字节（设备提前结束数据传输）",
                    size,
                    buffer.len()
                ));
            }
            if buffer.len() > size as usize {
                buffer.truncate(size as usize);
            }

            da.readflash_final_status()?;
            trace!("[readflash_data] total read {} bytes", buffer.len());
            Ok(buffer)
        })
    }
}

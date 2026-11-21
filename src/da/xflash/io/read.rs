//! Read-path methods (readflash_* pipeline)
//!
//! See the [`crate::da::xflash::io`] module docs. Shared private free fns are provided by `super`.

use log::{info, trace, warn};
use std::time::Duration;

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{CMD_MAGIC, CMD_READ_DATA, GET_PKT_LEN, pack3};

use super::{
    DA_READ_PER_CHUNK_TRAILER, READFLASH_READ_TIMEOUT_MS, acquire_dump_buffer,
    active_resume_matches, ensure_output_file_path, final_read_status_from_payload,
    finish_dump_writer, parse_packet_length, quiet_usb_reads_temporarily,
    read_header_with_optional_queue, remove_resume_file, spawn_dump_writer, with_read_timeout,
    write_resume_file,
};

impl<'a> DAXFlash<'a> {
    /// Read flash data and return the raw bytes (defaults to USER partition type)
    pub(crate) fn readflash_data(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        self.readflash_data_ex(addr, size, 8)
    }

    /// Send the READ_DATA command and its 56B parameters (shared logic to avoid duplication)
    pub(crate) fn send_read_data_cmd(
        &mut self,
        addr: u64,
        size: u64,
        parttype: u32,
    ) -> Result<(), String> {
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "readflash xsend")?;
        self.write_with_retry(&CMD_READ_DATA.to_le_bytes(), "readflash CMD")?;
        let st = self.status()?;
        if st != 0 {
            return Err(format!("READ_DATA status=0x{:08X}", st));
        }

        // Build the 56B parameters on the stack to avoid a heap Vec per read
        let mut param = [0u8; 56];
        param[..4].copy_from_slice(&1u32.to_le_bytes());
        param[4..8].copy_from_slice(&parttype.to_le_bytes());
        param[8..16].copy_from_slice(&addr.to_le_bytes());
        param[16..24].copy_from_slice(&size.to_le_bytes());
        // param[24..56] stays zero-filled ([0u8; 56] initial value)
        let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.write_with_retry(&param_pkt, "readflash param_hdr")?;
        self.write_with_retry(&param, "readflash param")?;
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("send_param status=0x{:08X}", st2));
        }
        Ok(())
    }

    /// Read flash data to a file (multi-threaded pipeline: USB reads overlap disk writes)
    ///
    /// Architecture:
    /// - Main thread: USB read loop (read header → read data → push to channel → send ACK)
    /// - Writer thread: pop data from the channel → write to file (64MB BufWriter)
    ///
    /// Channel capacity is 128 (~512MB at 4MB/packet); when full the main thread blocks until the
    /// writer consumes, so USB reads are never blocked by disk writes, achieving true pipelining.
    /// Packet buffers are recycled through a channel to avoid per-packet heap allocation (heartbeat packets short-circuit with a stack buffer and do not use the pool).
    pub(crate) fn readflash_to_file<F>(
        &mut self,
        addr: u64,
        size: u64,
        parttype: u32,
        output_file: &str,
        start_offset: u64,
        on_packet: F,
    ) -> Result<u64, String>
    where
        F: Fn(u64),
    {
        trace!(
            "[readflash_to_file] addr=0x{:08X} size={} parttype={} output={} start_offset={}",
            addr, size, parttype, output_file, start_offset
        );
        let _quiet_guard = quiet_usb_reads_temporarily();

        // The serial default timeout is only 1s, but streaming readflash lasts seconds; a >1s gap
        // between packets would time out. Wrap the whole read in a longer read timeout (RAII, covers
        // every early return) and restore it automatically afterwards.
        with_read_timeout(self, READFLASH_READ_TIMEOUT_MS, |da| {
            const PROGRESS_INTERVAL: u64 = 4 * 1024 * 1024; // 4MB progress updates (balance accuracy and overhead)
            const MAX_PACKET_SIZE: usize = 0x1000000; // 16MB preallocated buffer

            if start_offset > size {
                return Err(format!(
                    "续传偏移 {} 超过分区大小 {}，resume 文件可能损坏",
                    start_offset, size
                ));
            }
            let target_remaining = size - start_offset;
            ensure_output_file_path(output_file)?;

            let active_resume =
                start_offset > 0 && active_resume_matches(output_file, start_offset);
            let mut packet_len = None;

            if active_resume {
                info!(
                    "检测到活跃读取流，发送 ACK 后从 {} 字节继续接收",
                    start_offset
                );
                // Extra retries for the resume ACK: the device may have just recovered, so the first ACK can time out
                let mut ack_ok = false;
                for attempt in 0..3 {
                    match da.ack_silent() {
                        Ok(()) => {
                            ack_ok = true;
                            break;
                        }
                        Err(e) => {
                            if attempt < 2 {
                                warn!("[RESUME] ACK 尝试 {} 失败，200ms 后重试...", attempt + 1);
                                std::thread::sleep(Duration::from_millis(200));
                            } else {
                                return Err(format!("活跃读取流续接 ACK 失败: {}", e));
                            }
                        }
                    }
                }
                if !ack_ok {
                    // All ACKs failed: the device may be disconnected; remove the resume file to avoid an endless loop
                    remove_resume_file(output_file);
                    return Err("活跃读取流续接 ACK 全部失败".to_string());
                }
            } else {
                // Matching Python readflash: query get_packet_length before cmd_read_data,
                // and **GET only, never SET** (send_devctrl already includes the full xread + status handshake).
                // * 0x040008 is GET_RANDOM_ID in the mtkclient Cmd table, not SET_PKT_LEN;
                //   the old implementation sent it with parameters, desynchronizing the DA state machine so
                //   the following READ_DATA returned 0xC0010003 (Unsupported command) — this caused printgpt
                //   failures on real devices.
                //   The packet length comes from the DA's reported value (fall back to 1 MiB on power loss or
                //   abnormal report) and serves only as resume metadata; actual reception always chunks by each
                //   packet header's slength.
                let reported = match da.send_devctrl(GET_PKT_LEN, None) {
                    Ok(data) => parse_packet_length(&data),
                    Err(e) => {
                        trace!("获取 DA 读包长度失败: {}", e);
                        None
                    }
                };
                let target = match reported {
                    Some(v) if v > 0 => v.min(0x400000),
                    _ => 0x100000,
                };
                trace!(
                    "DA 读包长度 {} 字节 ({:.2} MiB)，按 DA 自报值读取",
                    target,
                    target as f64 / 1024.0 / 1024.0
                );
                packet_len = Some(target);

                // Send the READ_DATA command and its parameters
                // Key fix: in the inactive-resume branch (start_offset>0 but resume does not match) the file
                // is opened in append mode (spawn_dump_writer), so reading must resume from addr+start_offset;
                // otherwise the partition head is read and appended after the existing prefix, producing a corrupted image.
                da.send_read_data_cmd(addr + start_offset, target_remaining, parttype)?;
                write_resume_file(
                    output_file,
                    addr,
                    size,
                    parttype,
                    start_offset,
                    true,
                    packet_len,
                )?;
            }

            let (writer_tx, recycle_rx, writer_handle) =
                spawn_dump_writer(output_file, addr, size, parttype, start_offset, packet_len)?;

            let mut total_read: u64 = start_offset;
            let mut bytes_received: u64 = 0;
            let mut last_progress_pos: u64 = start_offset;
            let mut queued_header = false;

            while bytes_received < target_remaining {
                let mut hdr = [0u8; 12];
                match read_header_with_optional_queue(
                    da.preloader.device.as_mut(),
                    &mut hdr,
                    &mut queued_header,
                ) {
                    Ok(()) => {}
                    Err(e) => {
                        write_resume_file(
                            output_file,
                            addr,
                            size,
                            parttype,
                            total_read,
                            false,
                            packet_len,
                        )?;
                        return Err(format!("read header: {}", e));
                    }
                }

                let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                let slength = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);

                if magic != CMD_MAGIC {
                    write_resume_file(
                        output_file,
                        addr,
                        size,
                        parttype,
                        total_read,
                        false,
                        packet_len,
                    )?;
                    return Err(format!(
                        "readflash bad magic: 0x{:08X} at offset {}",
                        magic, total_read
                    ));
                }

                if slength == 4 {
                    // Detect heartbeats with a stack buffer first: heartbeat packets (all zero) are skipped
                    // without taking or leaking a recycle buffer; otherwise repeated heartbeats during a long
                    // read would exhaust the pool and degrade to per-packet allocation.
                    let mut tiny = [0u8; 4];
                    da.preloader
                        .device
                        .read_exact(&mut tiny)
                        .map_err(|e| format!("read data: {}", e))?;
                    if tiny.iter().all(|&b| b == 0) {
                        trace!("[readflash] 心跳包，跳过");
                        continue;
                    }
                    // Non-heartbeat real 4-byte packets are rare; use a small allocation from the pool here
                    writer_tx
                        .send(Some(tiny.to_vec()))
                        .map_err(|_| "写入线程已退出".to_string())?;
                    bytes_received += 4;
                    total_read += 4;
                } else if slength == 0 {
                    continue;
                } else if (slength as usize) <= MAX_PACKET_SIZE {
                    let slen = slength as usize;
                    let mut data = if da.da_x_speed >= 3 {
                        da.preloader
                            .device
                            .read_exact_vec(slen)
                            .map_err(|e| format!("read data fast: {}", e))?
                    } else {
                        let mut data = acquire_dump_buffer(&recycle_rx, slen);
                        da.preloader
                            .device
                            .read_exact(&mut data)
                            .map_err(|e| format!("read data: {}", e))?;
                        data
                    };
                    // DA may append a fixed-length trailer at the end of each data chunk (DA_READ_PER_CHUNK_TRAILER bytes).
                    // * For **every chunk**, take `real data = slen - trailer` and truncate the trailer, so the
                    // file on disk is exactly the partition size and the stream aligns with the final packet.
                    // Leaving the trailer in middle chunks shifts the whole segment by 8 bytes per chunk
                    // (same bug as read_ex.rs).
                    let take = (slen.saturating_sub(DA_READ_PER_CHUNK_TRAILER))
                        .min((target_remaining - bytes_received) as usize);
                    data.truncate(take);
                    writer_tx
                        .send(Some(data))
                        .map_err(|_| "写入线程已退出".to_string())?;
                    bytes_received += take as u64;
                    total_read += take as u64;
                } else {
                    let slen = slength as usize;
                    let mut data = vec![0u8; slen];
                    da.preloader
                        .device
                        .read_exact(&mut data)
                        .map_err(|e| format!("read data (large): {}", e))?;
                    // Take exactly the remaining requested bytes and drop any DA trailer (stripped on every chunk)
                    let take = (slen.saturating_sub(DA_READ_PER_CHUNK_TRAILER))
                        .min((target_remaining - bytes_received) as usize);
                    data.truncate(take);
                    writer_tx
                        .send(Some(data))
                        .map_err(|_| "写入线程已退出".to_string())?;
                    bytes_received += take as u64;
                    total_read += take as u64;
                }

                if total_read - last_progress_pos >= PROGRESS_INTERVAL
                    || bytes_received >= target_remaining
                {
                    on_packet(total_read);
                    last_progress_pos = total_read;
                }

                if crate::cancel::force_requested() {
                    da.preloader.device.cancel_pending_transfers();
                    let written = finish_dump_writer(writer_tx, writer_handle)?;
                    return Err(format!(
                        "读取已强制停止，已保存 {} 字节；如设备仍在线可续传，否则重新进 BROM 后普通续传",
                        written
                    ));
                }

                if crate::cancel::requested() {
                    let written = finish_dump_writer(writer_tx, writer_handle)?;
                    return Err(format!(
                        "读取已在包边界安全停止，已保存 {} 字节；重新运行同一命令可续传",
                        written
                    ));
                }

                // Optimization: ACK first, then pre-submit the header
                // ACK (OUT) → device starts preparing the next packet once received → pre-submitted header (IN) captures it
                if let Err(e) = da.ack_silent() {
                    da.preloader.device.cancel_pending_transfers();
                    write_resume_file(
                        output_file,
                        addr,
                        size,
                        parttype,
                        total_read,
                        false,
                        packet_len,
                    )?;
                    return Err(format!("send_ack: {}", e));
                }
                if bytes_received >= target_remaining {
                    trace!("[readflash] 最后一包 ACK 已发送，等待最终状态");
                    break;
                }

                // Pre-submit the next packet header after the ACK: the device received the ACK and is preparing the next packet
                queued_header = da.preloader.device.submit_read_request(12).unwrap_or(false);
            }

            da.readflash_final_status()?;
            let written = finish_dump_writer(writer_tx, writer_handle)?;
            remove_resume_file(output_file);
            on_packet(written);

            trace!("[readflash] total read {} bytes", written);
            Ok(written)
        })
    }

    pub(crate) fn readflash_final_status(&mut self) -> Result<(), String> {
        // The serial default timeout is only 1s, and reading the final status header can time out
        // because the device is slow to finish (typical error
        // "readflash final status header: serial read_exact: Operation timed out").
        // Wrap this read in a longer timeout and restore the original afterwards.
        with_read_timeout(self, READFLASH_READ_TIMEOUT_MS, |da| {
            let mut hdr = [0u8; 12];
            da.preloader
                .device
                .read_exact(&mut hdr)
                .map_err(|e| format!("readflash final status header: {}", e))?;
            let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let slength = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
            if magic != CMD_MAGIC {
                return Err(format!("readflash final status bad magic: 0x{:08X}", magic));
            }
            let mut payload = vec![0u8; slength as usize];
            if slength > 0 {
                da.preloader
                    .device
                    .read_exact(&mut payload)
                    .map_err(|e| format!("readflash final status payload: {}", e))?;
            }
            final_read_status_from_payload(&payload)
        })
    }

    /// Silent ACK (send only, no read) so the readflash_data loop does not consume the next packet
    /// Optimization: merge the 12B header and 4B data into one USB OUT transfer, saving one USB call
    pub(crate) fn ack_silent(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        let mut buf = [0u8; 16];
        buf[..12].copy_from_slice(&hdr);
        buf[12..16].copy_from_slice(&0u32.to_le_bytes());
        self.write_with_retry(&buf, "ack_silent")?;
        Ok(())
    }
}

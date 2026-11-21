//! Stream data to raw addresses (with progress bar / resume)

use crate::progress::{ProgressBar, ProgressStyle};
use log::{info, trace, warn};

use crate::da::xflash::{CMD_MAGIC, DAXFlash, pack3};

use super::{format_write_status_error, write_write_resume_file};

/// Parameter chunk size: aligned with Python mtkclient, all payloads are sent in 0x200 chunks.
/// The DA protocol requires strict chunking; sending the whole payload at once causes a device-side deadlock.
const XFLASH_PARAM_CHUNK: usize = 0x200;

impl<'a> DAXFlash<'a> {
    /// Send a parameter list, aligned with Python send_param:
    /// each parameter has its own 12B header, payloads are sent in 0x200 chunks,
    /// and status is read once after the whole list is sent.
    pub(crate) fn send_param_list_chunked(
        &mut self,
        params: &[&[u8]],
        label: &str,
    ) -> Result<(), String> {
        trace!("[send_param_list] label={} params={}", label, params.len());
        for (param_idx, payload) in params.iter().enumerate() {
            if crate::cancel::force_requested() || crate::cancel::requested() {
                self.preloader.device.cancel_pending_transfers();
                return Err(format!("{} 已取消", label));
            }

            let param_pkt = pack3(CMD_MAGIC, 0x01, payload.len() as u32);
            trace!(
                "[send_param_list] 发送 param {} header len={}",
                param_idx,
                payload.len()
            );
            self.write_with_retry(&param_pkt, &format!("{} param {} header", label, param_idx))?;

            // All payloads strictly chunked at 0x200 (aligned with Python mtkclient)
            for (chunk_idx, chunk) in payload.chunks(XFLASH_PARAM_CHUNK).enumerate() {
                self.write_with_retry(
                    chunk,
                    &format!("{} param {} chunk {}", label, param_idx, chunk_idx),
                )?;
            }
        }
        // Status read: aligned with the flash tool, supports 0x00010004 retry
        let mut status = self.status()?;
        if status == 0 {
            return Ok(());
        }
        // Writing occasionally yields status=0x00010004, requiring resync + re-read
        if status == 0x00010004 {
            warn!("{} status=0x00010004，尝试 resync 后重读", label);
            std::thread::sleep(std::time::Duration::from_millis(50));
            status = self.status()?;
            if status == 0 {
                return Ok(());
            }
        }
        Err(format_write_status_error(label, status))
    }

    /// Stream data to raw addresses, reused by partition writes, seccfg/frp, etc.
    /// Accepts any `Read` implementation (file, in-memory buffer, ...) to avoid loading large files entirely into memory.
    /// Shows progress: the custom progress bar (src/progress.rs) updates live and finally reports total elapsed time and speed.
    ///
    /// Resume parameters:
    /// - `start_offset`: bytes already written before this call (for progress bar init and resume file records)
    /// - `resume_input`: input file path (used to save/update the `.wresume` file on cancellation)
    /// - `base_addr`: original partition start address (without the start_offset adjustment)
    /// - `original_total`: original total write size (without subtracting start_offset)
    pub(crate) fn write_flash_data_stream(
        &mut self,
        addr: u64,
        reader: &mut dyn std::io::Read,
        total: u64,
        storage: u32,
        parttype: u32,
        start_offset: u64,
        resume_input: &str,
    ) -> Result<(), String> {
        // Aligned with Python mtkclient writeflash: call get_packet_length before cmd_write_data
        // Do not send xflash_sync (SYNC_SIGNAL would break the DA state machine after HACC)
        let write_packet_size = self.get_packet_length().unwrap_or(0x40000);

        self.cmd_write_data(addr, total, storage, parttype)?;
        let start_time = std::time::Instant::now();

        // Create the progress bar (total is the remaining amount this call; the bar shows total bytes written)
        let total_written = start_offset + total;
        let bar = if total_written > 0 {
            ProgressBar::new(total_written)
        } else {
            ProgressBar::new(total)
        };
        bar.set_style(
            ProgressStyle::with_template(
                "  {spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
                 {binary_bytes}/{binary_total_bytes} ({percent}%) \
                 {binary_bytes_per_sec} ETA {eta}",
            )
            .unwrap()
            .progress_chars("█▓░"),
        );
        bar.set_message(format!("写入: 0x{:08X}", addr));
        if start_offset > 0 {
            bar.set_position(start_offset);
        }

        let mut pos = 0u64;
        let mut chunk_buf = vec![0u8; write_packet_size];
        let mut last_resume_pos: u64 = 0;
        const RESUME_INTERVAL: u64 = 64 * 1024 * 1024; // resume save interval: 64MB

        while pos < total {
            if crate::cancel::force_requested() || crate::cancel::requested() {
                self.preloader.device.cancel_pending_transfers();
                bar.abandon_with_message("写入已取消");
                return self.build_cancel_error(
                    start_offset,
                    pos,
                    total,
                    addr,
                    resume_input,
                    write_packet_size,
                );
            }
            let to_read = std::cmp::min(write_packet_size as u64, total - pos) as usize;
            let n = reader
                .read(&mut chunk_buf[..to_read])
                .map_err(|e| format!("读取文件失败: {}", e))?;
            if n == 0 {
                return Err(format!(
                    "文件提前结束: 期望 {} 字节，实际 {} 字节",
                    total, pos
                ));
            }
            let chunk = &chunk_buf[..n];

            // 16-bit additive checksum (aligned with penumbra download_data, see chunk_checksum)
            let checksum = crate::da::xflash::chunk_checksum(chunk);

            trace!(
                "[writeflash] chunk offset={} size={} checksum=0x{:08X}",
                pos, n, checksum
            );

            let zero = 0u32.to_le_bytes();
            let checksum_bytes = checksum.to_le_bytes();
            if let Err(e) =
                self.send_param_list_chunked(&[&zero, &checksum_bytes, chunk], "writeflash chunk")
            {
                if crate::cancel::force_requested() || crate::cancel::requested() {
                    self.preloader.device.cancel_pending_transfers();
                    bar.abandon_with_message("写入已取消");
                    return self.build_cancel_error(
                        start_offset,
                        pos,
                        total,
                        addr,
                        resume_input,
                        write_packet_size,
                    );
                }
                return Err(e);
            }

            pos += n as u64;
            bar.set_position(start_offset + pos);

            // Periodically save the resume file (every 64MB) so the breakpoint remains recoverable
            if !resume_input.is_empty() && pos - last_resume_pos >= RESUME_INTERVAL {
                let _ = write_write_resume_file(
                    resume_input,
                    addr.saturating_sub(start_offset),
                    start_offset + total,
                    start_offset + pos,
                    Some(write_packet_size),
                );
                last_resume_pos = pos;
            }
        }

        let st = self.read_write_final_status()?;
        if st != 0 {
            bar.abandon_with_message(format!("写入失败: status=0x{:08X}", st));
            return Err(format_write_status_error("writeflash final", st));
        }

        let elapsed = start_time.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            total as f64 / elapsed / 1024.0 / 1024.0
        } else {
            0.0
        };
        bar.finish_with_message(format!("写入完成: {:.2} MB/s", speed));
        info!(
            "  写入完成: {} 字节, 耗时 {:.2}s, 速度 {:.2} MB/s",
            total, elapsed, speed
        );
        Ok(())
    }

    /// Write in-memory data to raw addresses (backward compatible).
    /// Internally delegates to the streaming implementation to unify the code path.
    pub(crate) fn write_flash_data(
        &mut self,
        addr: u64,
        data: &[u8],
        storage: u32,
        parttype: u32,
    ) -> Result<(), String> {
        self.write_flash_data_stream(
            addr,
            &mut data.as_ref(),
            data.len() as u64,
            storage,
            parttype,
            0,
            "",
        )
    }

    /// Read the final write status, supporting 0x00010004 retry
    /// Aligned with the flash tool: after writing, status=0x00010004 occasionally occurs
    fn read_write_final_status(&mut self) -> Result<u32, String> {
        let mut st = self.status()?;
        if st == 0 {
            return Ok(st);
        }
        // status 0x00010004: resync then retry
        if st == 0x00010004 {
            warn!("writeflash final status=0x00010004，尝试 resync");
            std::thread::sleep(std::time::Duration::from_millis(100));
            st = self.status()?;
            if st == 0 {
                return Ok(st);
            }
        }
        Ok(st)
    }

    /// Build the cancellation error and save resume state
    fn build_cancel_error(
        &self,
        start_offset: u64,
        pos: u64,
        total: u64,
        addr: u64,
        resume_input: &str,
        write_packet_size: usize,
    ) -> Result<(), String> {
        if !resume_input.is_empty() {
            let base_addr = addr.saturating_sub(start_offset);
            let original_total = start_offset + total;
            let total_written_now = start_offset + pos;
            let _ = write_write_resume_file(
                resume_input,
                base_addr,
                original_total,
                total_written_now,
                Some(write_packet_size),
            );
            let pct = if original_total > 0 {
                total_written_now as f64 / original_total as f64 * 100.0
            } else {
                0.0
            };
            Err(format!(
                "写入已取消，已写入 {}/{} 字节 ({:.1}%)。重新执行相同命令将从断点继续。",
                total_written_now, original_total, pct
            ))
        } else {
            Err("写入已取消".to_string())
        }
    }
}

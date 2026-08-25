//! 按原始地址流式写入数据（带进度条/续传）

use crate::progress::{ProgressBar, ProgressStyle};
use log::{info, trace, warn};

use crate::da::xflash::{CMD_MAGIC, DAXFlash, pack3};

use super::{format_write_status_error, write_write_resume_file};

/// 参数分块大小：对齐 Python mtkclient，所有 payload 按 0x200 分块发送。
/// DA 协议要求严格按此分块，整包发送会导致 device-side 死锁。
const XFLASH_PARAM_CHUNK: usize = 0x200;

impl<'a> DAXFlash<'a> {
    /// 发送参数列表，对齐 Python send_param：
    /// 每个参数都有独立的 12B header，payload 按 0x200 分块发送，
    /// 整个列表发送完成后读一次 status。
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

            // 所有 payload 严格按 0x200 分块（对齐 Python mtkclient）
            for (chunk_idx, chunk) in payload.chunks(XFLASH_PARAM_CHUNK).enumerate() {
                self.write_with_retry(
                    chunk,
                    &format!("{} param {} chunk {}", label, param_idx, chunk_idx),
                )?;
            }
        }
        // status 读取：对齐刷机匣，支持 0x00010004 重试
        let mut status = self.status()?;
        if status == 0 {
            return Ok(());
        }
        // 写入时偶发 status=0x00010004，需要 resync + 重读
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

    /// 按原始地址流式写入数据，供分区写入、seccfg/frp 等场景复用。
    /// 接受任意 `Read` 实现（文件、内存缓冲区等），避免大文件全量载入内存。
    /// 带进度条显示：使用自研进度条（src/progress.rs）实时更新进度，最后输出总耗时和速度。
    ///
    /// 断点续传参数：
    /// - `start_offset`: 本次调用前已写入的字节数（用于进度条初始化和恢复文件记录）
    /// - `resume_input`: 输入文件路径（用于在取消时保存/更新 `.wresume` 文件）
    /// - `base_addr`: 原始分区起始地址（不含 start_offset 偏移）
    /// - `original_total`: 原始总写入大小（不含 start_offset 减去）
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
        // 对齐 Python mtkclient writeflash：先调用 get_packet_length 再 cmd_write_data
        // 不发送 xflash_sync（SYNC_SIGNAL 会破坏 HACC 后的 DA 状态机）
        let write_packet_size = self.get_packet_length().unwrap_or(0x40000);

        self.cmd_write_data(addr, total, storage, parttype)?;
        let start_time = std::time::Instant::now();

        // 创建进度条（total 是本次剩余量，进度条显示已写入总量）
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
        const RESUME_INTERVAL: u64 = 64 * 1024 * 1024; // 64MB 续传保存间隔

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

            // 优化 checksum：按 8 字节批量处理减少循环次数
            let mut checksum: u32 = 0;
            let mut i = 0;
            while i + 8 <= n {
                checksum = checksum
                    .wrapping_add(chunk[i] as u32)
                    .wrapping_add(chunk[i + 1] as u32)
                    .wrapping_add(chunk[i + 2] as u32)
                    .wrapping_add(chunk[i + 3] as u32)
                    .wrapping_add(chunk[i + 4] as u32)
                    .wrapping_add(chunk[i + 5] as u32)
                    .wrapping_add(chunk[i + 6] as u32)
                    .wrapping_add(chunk[i + 7] as u32);
                i += 8;
            }
            while i < n {
                checksum = checksum.wrapping_add(chunk[i] as u32);
                i += 1;
            }

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

            // 定期保存续传文件（每 64MB），确保断点可恢复
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

    /// 按原始地址写入内存中的数据（向后兼容）。
    /// 内部转接到流式实现，统一代码路径。
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

    /// 读取写入最终状态，支持 0x00010004 重试
    /// 对齐刷机匣：写入完成后偶发 status=0x00010004
    fn read_write_final_status(&mut self) -> Result<u32, String> {
        let mut st = self.status()?;
        if st == 0 {
            return Ok(st);
        }
        // status 0x00010004: resync 后重试
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

    /// 构建取消错误并保存续传状态
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

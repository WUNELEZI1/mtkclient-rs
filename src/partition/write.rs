//! DAXFlash 底层写入原语
//!
//! - `write_flash_data` — 按原始地址写入数据（带进度条显示）
//! - `get_packet_length` — 获取写包长度
//! - `cmd_write_data`   — 发送写命令
//!
//! 写入断点续传辅助函数：
//! - `write_resume_path_for`       — 生成 `.wresume` 文件路径
//! - `write_write_resume_file`     — 写入续传状态文件
//! - `remove_write_resume_file`    — 删除续传状态文件
//! - `check_write_resume`          — 检查续传状态是否匹配

use indicatif::{ProgressBar, ProgressStyle};
use log::{info, trace, warn};

use crate::da::xflash::{CMD_MAGIC, CMD_WRITE_DATA, DAXFlash, pack3};

// ═══════════════════════════════════════════════════════════════════════════════
// 写入断点续传辅助函数
// ═══════════════════════════════════════════════════════════════════════════════

/// 生成写入续传状态文件路径
pub(crate) fn write_resume_path_for(input_file: &str) -> String {
    format!("{}.wresume", input_file)
}

/// 写入续传状态文件
pub(crate) fn write_write_resume_file(
    input_file: &str,
    addr: u64,
    total: u64,
    written: u64,
    packet_len: Option<usize>,
) -> Result<(), String> {
    let packet = packet_len.unwrap_or(0);
    let content = format!(
        "input={}\naddr=0x{:X}\ntotal={}\nwritten={}\npacket_len={}\n",
        input_file, addr, total, written, packet
    );
    std::fs::write(write_resume_path_for(input_file), content)
        .map_err(|e| format!("写入续传状态失败: {}", e))
}

/// 删除写入续传状态文件
pub(crate) fn remove_write_resume_file(input_file: &str) {
    let _ = std::fs::remove_file(write_resume_path_for(input_file));
}

/// 检查写入续传状态是否匹配
/// 返回已写入字节数（若匹配且有效），否则 None
pub(crate) fn check_write_resume(input_file: &str, addr: u64, total: u64) -> Option<u64> {
    let content = match std::fs::read_to_string(write_resume_path_for(input_file)) {
        Ok(c) => c,
        Err(_) => return None,
    };

    let resume_addr = content
        .lines()
        .find_map(|line| line.strip_prefix("addr=0x"))
        .and_then(|v| u64::from_str_radix(v, 16).ok());
    let resume_total = content
        .lines()
        .find_map(|line| line.strip_prefix("total="))
        .and_then(|v| v.parse::<u64>().ok());
    let written = content
        .lines()
        .find_map(|line| line.strip_prefix("written="))
        .and_then(|v| v.parse::<u64>().ok());

    match (resume_addr, resume_total, written) {
        (Some(ra), Some(rt), Some(w)) if ra == addr && rt == total && w > 0 && w < total => Some(w),
        _ => None,
    }
}

fn explain_write_status(status: u32) -> &'static str {
    match status {
        CMD_WRITE_DATA => {
            "读到了 WRITE_DATA 命令 echo，说明 XFlash 状态流错位；请先重试一次，若仍失败请重新进 BROM"
        }
        _ => "DA 返回非零状态",
    }
}

fn format_write_status_error(stage: &str, status: u32) -> String {
    format!(
        "{} status error: 0x{:08X} ({})",
        stage,
        status,
        explain_write_status(status)
    )
}

/// 参数分块大小：对齐 Python mtkclient，所有 payload 按 0x200 分块发送。
/// DA 协议要求严格按此分块，整包发送会导致 device-side 死锁。
const XFLASH_PARAM_CHUNK: usize = 0x200;

impl<'a> DAXFlash<'a> {
    /// 发送参数列表，对齐 Python send_param：
    /// 每个参数都有独立的 12B header，payload 按 0x200 分块发送，
    /// 整个列表发送完成后读一次 status。
    fn send_param_list_chunked(&mut self, params: &[&[u8]], label: &str) -> Result<(), String> {
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
    /// 带进度条显示：使用 indicatif 实时更新进度，最后输出总耗时和速度。
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

    /// 获取写包长度（对齐 Python get_packet_length）
    fn get_packet_length(&mut self) -> Result<usize, String> {
        // 发送 GET_PACKET_LENGTH (0x040007) 通过 devctrl
        // send_devctrl 内部已包含完整的 xread + status 握手
        let data = self.send_devctrl(0x040007, None)?;
        if data.is_empty() {
            return Ok(0x40000); // 默认值
        }
        if data.len() >= 8 {
            let plen = u32::from_le_bytes(data[..4].try_into().unwrap());
            let read_plen = u32::from_le_bytes(data[4..8].try_into().unwrap());
            trace!(
                "DA 写包长度: {} 字节 ({:.2} MiB), 读包长度: {} 字节 ({:.2} MiB)",
                plen,
                plen as f64 / 1024.0 / 1024.0,
                read_plen,
                read_plen as f64 / 1024.0 / 1024.0
            );
            if plen > 0 {
                return Ok(plen as usize);
            }
        } else if data.len() >= 4 {
            let plen = u32::from_le_bytes(data[..4].try_into().unwrap());
            if plen > 0 {
                return Ok(plen as usize);
            }
        }
        // 默认值（对齐 Python 默认行为）
        Ok(0x40000)
    }

    /// 发送写命令（对齐 Python cmd_write_data）
    fn cmd_write_data(
        &mut self,
        addr: u64,
        size: u64,
        storage: u32,
        parttype: u32,
    ) -> Result<bool, String> {
        trace!(
            "[cmd_write_data] addr=0x{:08X} size={} storage={} parttype={}",
            addr, size, storage, parttype
        );

        // 对齐 Python：发送 WRITE_DATA，失败时仅 drain + 重试（不发 xflash_sync）
        for attempt in 0..3 {
            if attempt > 0 {
                warn!(
                    "[cmd_write_data] 第 {} 次尝试失败，执行 drain + 重试...",
                    attempt
                );
                self.preloader.device.drain_pending();
                std::thread::sleep(std::time::Duration::from_millis(500));
            }

            // xsend(WRITE_DATA)
            let pkt = pack3(CMD_MAGIC, 0x01, 4);
            if let Err(e) = self.write_with_retry(&pkt, "cmd_write_data xsend") {
                trace!("[cmd_write_data] write header 失败: {}", e);
                continue;
            }
            if let Err(e) =
                self.write_with_retry(&CMD_WRITE_DATA.to_le_bytes(), "cmd_write_data CMD")
            {
                trace!("[cmd_write_data] write CMD 失败: {}", e);
                continue;
            }

            let mut st = match self.status() {
                Ok(s) => s,
                Err(e) => {
                    trace!("[cmd_write_data] status 读取失败: {}", e);
                    continue;
                }
            };
            if st == CMD_WRITE_DATA {
                warn!("cmd_write_data 读到 WRITE_DATA echo，尝试再读一次 status 进行重同步");
                st = match self.status() {
                    Ok(s) => s,
                    Err(e) => {
                        trace!("[cmd_write_data] 二次 status 读取失败: {}", e);
                        continue;
                    }
                };
            }
            if st == 0 {
                trace!("[cmd_write_data] status ok, 发送 56B 参数");
                let mut param = Vec::with_capacity(56);
                param.extend_from_slice(&storage.to_le_bytes());
                param.extend_from_slice(&parttype.to_le_bytes());
                param.extend_from_slice(&addr.to_le_bytes());
                param.extend_from_slice(&size.to_le_bytes());
                param.extend_from_slice(&[0u8; 32]); // NandExtension 全零
                self.send_param_list_chunked(&[&param], "cmd_write_data param")?;
                return Ok(true);
            }
            warn!(
                "[cmd_write_data] 尝试 {}/2 失败: status=0x{:08X}",
                attempt + 1,
                st
            );
        }

        Err(format!(
            "cmd_write_data 多次尝试后仍然失败。可能原因：DA 状态机未同步，或设备端写入超时。\
             建议：1) 让设备重新进入 BROM 模式后重试；2) 检查 USB 连接稳定性。"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_status_explains_command_echo_desync() {
        assert!(explain_write_status(CMD_WRITE_DATA).contains("WRITE_DATA 命令 echo"));
        assert_eq!(explain_write_status(0xDEAD), "DA 返回非零状态");
    }

    #[test]
    fn write_status_error_includes_stage_code_and_explanation() {
        let message = format_write_status_error("cmd_write_data", CMD_WRITE_DATA);

        assert!(message.contains("cmd_write_data status error"));
        assert!(message.contains("0x00010004"));
        assert!(message.contains("状态流错位"));
    }
}

//! DAXFlash 底层写入原语
//!
//! - `write_flash_data` — 按原始地址写入数据（带进度条显示）
//! - `get_packet_length` — 获取写包长度
//! - `cmd_write_data`   — 发送写命令

use indicatif::{ProgressBar, ProgressStyle};
use log::{info, trace, warn};

use crate::da::xflash::{CMD_MAGIC, CMD_WRITE_DATA, DAXFlash, pack3};

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

    /// 按原始地址写入一段数据，供分区写入、seccfg/frp 等场景复用。
    /// 带进度条显示：使用 indicatif 实时更新进度，最后输出总耗时和速度。
    pub(crate) fn write_flash_data(
        &mut self,
        addr: u64,
        data: &[u8],
        storage: u32,
        parttype: u32,
    ) -> Result<(), String> {
        // 写入前清空 USB IN pending data，防止读取残留干扰写入
        self.preloader.device.drain_pending();

        self.cmd_write_data(addr, data.len() as u64, storage, parttype)?;

        // 写入时跳过 get_packet_length（避免 send_devctrl 干扰 DA 状态）。
        // 使用 128KB 默认值（0x20000），平衡速度和稳定性。
        // 256KB 在部分 DA 上会导致 param 2 header 超时，128KB 更安全。
        let write_packet_size = 0x20000usize;
        let total = data.len();
        let start_time = std::time::Instant::now();

        // 创建进度条（输出到 stderr，与日志统一流，避免视觉交织）
        let bar = ProgressBar::new(total as u64);
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

        let mut pos = 0;
        while pos < total {
            if crate::cancel::force_requested() || crate::cancel::requested() {
                self.preloader.device.cancel_pending_transfers();
                bar.abandon_with_message("写入已取消");
                return Err("写入已取消".to_string());
            }
            let dsize = std::cmp::min(write_packet_size, total - pos);
            let chunk = &data[pos..pos + dsize];
            let checksum: u32 = chunk
                .iter()
                .fold(0u32, |sum, &byte| sum.wrapping_add(byte as u32));

            trace!(
                "[writeflash] chunk offset={} size={} checksum=0x{:08X}",
                pos, dsize, checksum
            );

            let zero = 0u32.to_le_bytes();
            let checksum_bytes = checksum.to_le_bytes();
            self.send_param_list_chunked(&[&zero, &checksum_bytes, chunk], "writeflash chunk")?;

            pos += dsize;

            // 更新进度条
            bar.set_position(pos as u64);
        }

        let st = self.read_write_final_status()?;
        if st != 0 {
            bar.abandon_with_message(format!("写入失败: status=0x{:08X}", st));
            return Err(format_write_status_error("writeflash final", st));
        }

        self.send_devctrl(0x800005, None)?;

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

    /// 获取写包长度（对齐 Python get_packet_length）
    fn get_packet_length(&mut self) -> Result<usize, String> {
        // 发送 GET_PACKET_LENGTH (0x040007) 通过 devctrl
        let data = self.send_devctrl(0x040007, None)?;
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

        // 尝试发送 WRITE_DATA 命令，若状态流错位则同步后重试一次
        for attempt in 0..2 {
            if attempt > 0 {
                warn!("[cmd_write_data] 第一次尝试失败，执行 drain + 重试...");
                self.preloader.device.drain_pending();
                std::thread::sleep(std::time::Duration::from_millis(200));
            }

            // xsend(WRITE_DATA)
            let pkt = pack3(CMD_MAGIC, 0x01, 4);
            if let Err(e) = self.write_with_retry(&pkt, "cmd_write_data xsend") {
                trace!("[cmd_write_data] write header 失败: {}", e);
                continue;
            }
            if let Err(e) = self.write_with_retry(&CMD_WRITE_DATA.to_le_bytes(), "cmd_write_data CMD") {
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

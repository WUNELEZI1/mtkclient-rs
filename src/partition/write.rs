//! DAXFlash 底层写入原语
//!
//! - `write_flash_data` — 按原始地址写入数据（带进度条显示）
//! - `get_packet_length` — 获取写包长度
//! - `cmd_write_data`   — 发送写命令

use indicatif::{ProgressBar, ProgressStyle};
use log::{info, warn};

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

const XFLASH_PARAM_CHUNK: usize = 0x200;

impl<'a> DAXFlash<'a> {
    fn send_param_list_chunked(&mut self, params: &[&[u8]], label: &str) -> Result<(), String> {
        for (param_idx, payload) in params.iter().enumerate() {
            let param_pkt = pack3(CMD_MAGIC, 0x01, payload.len() as u32);
            self.write_with_retry(&param_pkt, &format!("{} param {} header", label, param_idx))?;
            for (chunk_idx, chunk) in payload.chunks(XFLASH_PARAM_CHUNK).enumerate() {
                if crate::cancel::force_requested() || crate::cancel::requested() {
                    self.preloader.device.cancel_pending_transfers();
                    return Err(format!("{} 已取消", label));
                }
                self.write_with_retry(
                    chunk,
                    &format!("{} param {} chunk {}", label, param_idx, chunk_idx),
                )?;
            }
        }
        let status = self.status()?;
        if status == 0 {
            Ok(())
        } else {
            Err(format_write_status_error(label, status))
        }
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
        self.cmd_write_data(addr, data.len() as u64, storage, parttype)?;

        let write_packet_size = self.get_packet_length()?;
        let total = data.len();
        let start_time = std::time::Instant::now();

        // 创建进度条（indicatif）
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
            let checksum: u16 = chunk.iter().map(|&b| b as u16).sum::<u16>();

            let zero = 0u32.to_le_bytes();
            let checksum_bytes = (checksum as u32).to_le_bytes();
            self.send_param_list_chunked(&[&zero, &checksum_bytes, chunk], "writeflash chunk")?;

            pos += dsize;

            // 更新进度条
            bar.set_position(pos as u64);
        }

        let st = self.status()?;
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

    /// 获取写包长度（对齐 Python get_packet_length）
    fn get_packet_length(&mut self) -> Result<usize, String> {
        // 发送 GET_PACKET_LENGTH (0x040007) 通过 devctrl
        let data = self.send_devctrl(0x040007, None)?;
        if data.len() >= 8 {
            let plen = u32::from_le_bytes(data[..4].try_into().unwrap());
            let read_plen = u32::from_le_bytes(data[4..8].try_into().unwrap());
            info!(
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
        // xsend(WRITE_DATA)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "cmd_write_data xsend")?;
        self.write_with_retry(&CMD_WRITE_DATA.to_le_bytes(), "cmd_write_data CMD")?;

        let mut st = self.status()?;
        if st == CMD_WRITE_DATA {
            warn!("cmd_write_data 读到 WRITE_DATA echo，尝试再读一次 status 进行重同步");
            st = self.status()?;
        }
        if st == 0 {
            let mut param = Vec::with_capacity(56);
            param.extend_from_slice(&storage.to_le_bytes());
            param.extend_from_slice(&parttype.to_le_bytes());
            param.extend_from_slice(&addr.to_le_bytes());
            param.extend_from_slice(&size.to_le_bytes());
            param.extend_from_slice(&[0u8; 32]); // NandExtension 全零
            self.send_param_list_chunked(&[&param], "cmd_write_data param")?;
            return Ok(true);
        }
        Err(format_write_status_error("cmd_write_data", st))
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

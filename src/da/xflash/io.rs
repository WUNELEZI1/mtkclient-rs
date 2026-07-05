//! DA 设备 IO 与控制命令
//!
//! 包含 DAXFlash 的设备读写、复位、关闭命令：
//! - `readflash_data`：通过 XFlash 协议读取 flash 原始字节
//! - `ack_silent`：发送 ACK 不读响应（避免偷吃下一个包）
//! - `reset_device`：通过 DA 重启设备（CMD_RESET）
//! - `close_device`：关闭设备（可选 jump_bl 重启）
//! - `patch_vbmeta`：vbmeta 修补占位
//!
//! 拆分动机：这些是"已加载 DA 之后"与设备交互的命令，与 DA 加载主流程
//! 完全解耦，独立成模块便于审查和单元测试。

use log::{info, trace, warn};
use std::sync::atomic::Ordering;

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{CMD_MAGIC, CMD_READ_DATA, pack3};
use crate::usb::log::QUIET_USB_READ;

struct UsbReadQuietGuard(bool);

impl Drop for UsbReadQuietGuard {
    fn drop(&mut self) {
        QUIET_USB_READ.store(self.0, Ordering::Relaxed);
    }
}

fn quiet_usb_reads_temporarily() -> UsbReadQuietGuard {
    UsbReadQuietGuard(QUIET_USB_READ.swap(true, Ordering::Relaxed))
}

fn ensure_output_file_path(path: &str) -> Result<(), String> {
    let path_ref = std::path::Path::new(path);
    if path_ref.is_dir() {
        return Err(format!("输出路径是目录，不是文件: {}", path));
    }
    if let Some(parent) = path_ref.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建输出目录失败 '{}': {}", parent.display(), e))?;
    }
    Ok(())
}

fn parse_packet_length(data: &[u8]) -> Option<usize> {
    if data.len() >= 4 {
        let value = u32::from_le_bytes(data[..4].try_into().ok()?);
        if value > 0 {
            return Some(value as usize);
        }
    }
    None
}

fn resume_path_for(output_file: &str) -> String {
    format!("{}.resume", output_file)
}

fn write_resume_file(
    output_file: &str,
    addr: u64,
    size: u64,
    parttype: u32,
    written: u64,
    active_read: bool,
    packet_len: Option<usize>,
) -> Result<(), String> {
    let packet = packet_len.unwrap_or(0);
    let content = format!(
        "output={}\naddr=0x{:X}\nsize={}\nparttype={}\nwritten={}\nactive_read={}\npacket_len={}\n",
        output_file, addr, size, parttype, written, active_read, packet
    );
    std::fs::write(resume_path_for(output_file), content)
        .map_err(|e| format!("写入续传状态失败: {}", e))
}

fn remove_resume_file(output_file: &str) {
    let _ = std::fs::remove_file(resume_path_for(output_file));
}

fn active_resume_matches(output_file: &str, start_offset: u64) -> bool {
    let Ok(content) = std::fs::read_to_string(resume_path_for(output_file)) else {
        return false;
    };
    content.lines().any(|line| line == "active_read=true")
        && content
            .lines()
            .find_map(|line| line.strip_prefix("written="))
            .and_then(|value| value.parse::<u64>().ok())
            == Some(start_offset)
}

fn final_read_status_from_payload(payload: &[u8]) -> Result<(), String> {
    if payload.len() == 4 {
        let status = u32::from_le_bytes(payload.try_into().unwrap());
        if status != 0 {
            return Err(format!(
                "Read completed with error status: 0x{:08X}",
                status
            ));
        }
    }
    Ok(())
}

// =============================================================================
// Flash 数据读取
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 读取 flash 数据，返回原始字节（默认 USER 分区类型）
    pub(crate) fn readflash_data(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        self.readflash_data_ex(addr, size, 8)
    }

    /// 读取 flash 数据到文件（流水线多线程：USB读取与磁盘写入并行）
    ///
    /// 架构（对齐 Python mtkclient 的 writedata 线程）：
    /// - 主线程：USB 读取循环（读头 → 读数据 → 放入 channel → 发 ACK）
    /// - 写入线程：从 channel 取数据 → 写文件 → 8MB 缓冲
    ///
    /// channel 容量上限 32 包（约 128MB @ 4MB/包），主线程满了会等待写入线程消费
    /// 这样 USB 读取不会被磁盘写入阻塞，实现真正的流水线并行
    /// 激进优化版：读取 flash 数据到文件
    /// 优化点：
    ///   1. BufWriter 64MB 写入缓冲（Python open(wb, buffering=8MB) 的 8 倍）
    ///   2. Batch 累积 8MB 后一次性 channel send（减少同步开销）
    ///   3. Channel 容量 64（512MB 总缓冲 @ 8MB/包）
    ///   4. 进度条更新间隔 16MB（减少锁竞争）
    ///   5. 预分配 buffer 16MB（覆盖更大的 USB 包）
    ///   6. 文件预分配 set_len（避免写入时动态分配磁盘空间）
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
        let _quiet_guard = quiet_usb_reads_temporarily();

        use std::io::{BufWriter, Write};

        const FLUSH_INTERVAL: u64 = 128 * 1024 * 1024; // 128MB 落盘一次，Ctrl+C 时强制落盘
        const RESUME_INTERVAL: u64 = 64 * 1024 * 1024; // 64MB 更新一次续传状态
        const PROGRESS_INTERVAL: u64 = 16 * 1024 * 1024; // 16MB 进度更新
        const MAX_PACKET_SIZE: usize = 0x1000000; // 16MB 预分配 buffer
        const BUF_WRITER_CAP: usize = 64 * 1024 * 1024; // 64MB 写缓冲

        let target_remaining = size - start_offset;
        ensure_output_file_path(output_file)?;

        let active_resume = start_offset > 0 && active_resume_matches(output_file, start_offset);
        let mut packet_len = None;

        if active_resume {
            info!(
                "检测到活跃读取流，发送 ACK 后从 {} 字节继续接收",
                start_offset
            );
            self.ack_silent()
                .map_err(|e| format!("活跃读取流续接 ACK 失败: {}", e))?;
        } else {
            // 对齐 Python readflash：在 cmd_read_data 之前先查询 get_packet_length
            match self.send_devctrl(0x040007, None) {
                Ok(data) => {
                    packet_len = parse_packet_length(&data);
                    if let Some(packet_len) = packet_len {
                        info!(
                            "DA 读包长度: {} 字节 ({:.2} MiB)",
                            packet_len,
                            packet_len as f64 / 1024.0 / 1024.0
                        );
                    } else {
                        trace!("DA 读包长度响应为空或无效");
                    }
                }
                Err(e) => trace!("获取 DA 读包长度失败: {}", e),
            }
            let _ = self.status();

            // cmd_read_data
            let pkt = pack3(CMD_MAGIC, 0x01, 4);
            self.write_with_retry(&pkt, "readflash xsend")?;
            self.write_with_retry(&CMD_READ_DATA.to_le_bytes(), "readflash CMD")?;
            let st = self.status()?;
            if st != 0 {
                return Err(format!("READ_DATA status=0x{:08X}", st));
            }

            // send_param
            let mut param = Vec::with_capacity(56);
            param.extend_from_slice(&1u32.to_le_bytes());
            param.extend_from_slice(&parttype.to_le_bytes());
            param.extend_from_slice(&addr.to_le_bytes());
            param.extend_from_slice(&size.to_le_bytes());
            param.extend_from_slice(&[0u8; 32]);
            let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
            self.write_with_retry(&param_pkt, "readflash param_hdr")?;
            self.write_with_retry(&param, "readflash param")?;
            let st2 = self.status()?;
            if st2 != 0 {
                return Err(format!("send_param status=0x{:08X}", st2));
            }
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

        let raw_file = if start_offset > 0 {
            std::fs::OpenOptions::new()
                .append(true)
                .open(output_file)
                .map_err(|e| format!("打开文件失败 '{}': {}", output_file, e))?
        } else {
            std::fs::File::create(output_file)
                .map_err(|e| format!("创建文件失败 '{}': {}", output_file, e))?
        };
        let mut file = BufWriter::with_capacity(BUF_WRITER_CAP, raw_file);

        let mut total_read: u64 = start_offset;
        let mut bytes_received: u64 = 0;
        let mut data_buf = vec![0u8; MAX_PACKET_SIZE];
        let mut last_progress_pos: u64 = start_offset;
        let mut last_flush_pos: u64 = start_offset;
        let mut last_resume_pos: u64 = start_offset;

        while bytes_received < target_remaining {
            let mut hdr = [0u8; 12];
            match self.preloader.device.read_exact(&mut hdr) {
                Ok(0) => {
                    trace!("[readflash] ZLP on header read, ending");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = file.flush();
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
                let _ = file.flush();
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
                self.preloader
                    .device
                    .read_exact(&mut data_buf[..4])
                    .map_err(|e| format!("read data: {}", e))?;
                if data_buf[0] == 0 && data_buf[1] == 0 && data_buf[2] == 0 && data_buf[3] == 0 {
                    trace!("[readflash] 心跳包，跳过");
                    continue;
                }
                file.write_all(&data_buf[..4])
                    .map_err(|e| format!("写入文件失败: {}", e))?;
                bytes_received += 4;
                total_read += 4;
            } else if slength == 0 {
                continue;
            } else if (slength as usize) <= MAX_PACKET_SIZE {
                let slen = slength as usize;
                self.preloader
                    .device
                    .read_exact(&mut data_buf[..slen])
                    .map_err(|e| format!("read data: {}", e))?;
                file.write_all(&data_buf[..slen])
                    .map_err(|e| format!("写入文件失败: {}", e))?;
                bytes_received += slen as u64;
                total_read += slen as u64;
            } else {
                let mut data = vec![0u8; slength as usize];
                self.preloader
                    .device
                    .read_exact(&mut data)
                    .map_err(|e| format!("read data (large): {}", e))?;
                file.write_all(&data)
                    .map_err(|e| format!("写入文件失败: {}", e))?;
                bytes_received += data.len() as u64;
                total_read += data.len() as u64;
            }

            if total_read - last_flush_pos >= FLUSH_INTERVAL || bytes_received >= target_remaining {
                file.flush().map_err(|e| format!("flush 文件失败: {}", e))?;
                last_flush_pos = total_read;
            }
            if total_read - last_resume_pos >= RESUME_INTERVAL || bytes_received >= target_remaining
            {
                write_resume_file(
                    output_file,
                    addr,
                    size,
                    parttype,
                    total_read,
                    true,
                    packet_len,
                )?;
                last_resume_pos = total_read;
            }

            if total_read - last_progress_pos >= PROGRESS_INTERVAL
                || bytes_received >= target_remaining
            {
                on_packet(total_read);
                last_progress_pos = total_read;
            }

            if crate::cancel::requested() {
                file.flush().map_err(|e| format!("flush 文件失败: {}", e))?;
                write_resume_file(
                    output_file,
                    addr,
                    size,
                    parttype,
                    total_read,
                    true,
                    packet_len,
                )?;
                return Err(format!(
                    "读取已在包边界安全停止，已保存 {} 字节；重新运行同一命令可续传",
                    total_read
                ));
            }

            if let Err(e) = self.ack_silent() {
                let _ = file.flush();
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
        }

        self.readflash_final_status()?;
        file.flush().map_err(|e| format!("flush 文件失败: {}", e))?;
        remove_resume_file(output_file);
        on_packet(total_read);

        trace!("[readflash] total read {} bytes", total_read);
        Ok(total_read)
    }

    /// 读取 flash 数据（全量到内存），用于小分区或需要内存操作的场景
    pub(crate) fn readflash_data_ex(
        &mut self,
        addr: u64,
        size: u64,
        parttype: u32,
    ) -> Result<Vec<u8>, String> {
        // 1. get_packet_length + status
        let _ = self.send_devctrl(0x040007, None);
        let _ = self.status();

        // 2. cmd_read_data
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "readflash xsend")?;
        self.write_with_retry(&CMD_READ_DATA.to_le_bytes(), "readflash CMD")?;
        let st = self.status()?;
        if st != 0 {
            return Err(format!("READ_DATA status=0x{:08X}", st));
        }

        // send_param
        let mut param = Vec::with_capacity(56);
        param.extend_from_slice(&1u32.to_le_bytes());
        param.extend_from_slice(&parttype.to_le_bytes());
        param.extend_from_slice(&addr.to_le_bytes());
        param.extend_from_slice(&size.to_le_bytes());
        param.extend_from_slice(&[0u8; 32]);
        let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.write_with_retry(&param_pkt, "readflash param_hdr")?;
        self.write_with_retry(&param, "readflash param")?;
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("send_param status=0x{:08X}", st2));
        }

        // 3. 数据读取循环（全量到内存）
        let mut buffer = Vec::with_capacity(size as usize);
        let mut remaining = size as usize;

        while remaining > 0 {
            let mut hdr = [0u8; 12];
            match self.preloader.device.read_exact(&mut hdr) {
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

            let mut data = vec![0u8; slength as usize];
            if slength > 0
                && let Err(e) = self.preloader.device.read_exact(&mut data)
            {
                trace!("[readflash_data] read data error: {}", e);
                break;
            }

            if slength == 4 && data.iter().all(|&b| b == 0) {
                trace!("[readflash_data] 心跳包，跳过");
                continue;
            }

            buffer.extend_from_slice(&data);
            remaining = remaining.saturating_sub(data.len());

            if let Err(e) = self.ack_silent() {
                trace!("[readflash_data] send_ack failed: {}", e);
                break;
            }
            if remaining == 0 {
                trace!("[readflash_data] 最后一包 ACK 已发送，等待最终状态");
                break;
            }
        }

        self.readflash_final_status()?;
        trace!("[readflash_data] total read {} bytes", buffer.len());
        Ok(buffer)
    }

    fn readflash_final_status(&mut self) -> Result<(), String> {
        let mut hdr = [0u8; 12];
        self.preloader
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
            self.preloader
                .device
                .read_exact(&mut payload)
                .map_err(|e| format!("readflash final status payload: {}", e))?;
        }
        final_read_status_from_payload(&payload)
    }

    /// 静默 ACK（仅发不读），用于 readflash_data 循环中不偷吃下一个包
    fn ack_silent(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&hdr, "ack_silent hdr")?;
        self.write_with_retry(&0u32.to_le_bytes(), "ack_silent data")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_rejects_directory() {
        let dir = std::env::temp_dir();
        let err = ensure_output_file_path(dir.to_str().unwrap()).unwrap_err();

        assert!(err.contains("输出路径是目录"));
    }

    #[test]
    fn quiet_usb_guard_restores_previous_state() {
        QUIET_USB_READ.store(false, Ordering::Relaxed);
        {
            let _guard = quiet_usb_reads_temporarily();
            assert!(QUIET_USB_READ.load(Ordering::Relaxed));
        }

        assert!(!QUIET_USB_READ.load(Ordering::Relaxed));
    }

    #[test]
    fn parse_packet_length_reads_little_endian_u32() {
        assert_eq!(
            parse_packet_length(&0x40000u32.to_le_bytes()),
            Some(0x40000)
        );
        assert_eq!(parse_packet_length(&0u32.to_le_bytes()), None);
        assert_eq!(parse_packet_length(&[1, 2, 3]), None);
    }

    #[test]
    fn active_resume_matches_only_when_written_offset_matches() {
        let output =
            std::env::temp_dir().join(format!("resume_match_{}_{}.img", std::process::id(), "a"));
        let output = output.to_string_lossy().to_string();
        write_resume_file(&output, 0x1000, 0x4000, 8, 0x2000, true, Some(0x1000)).unwrap();

        assert!(active_resume_matches(&output, 0x2000));
        assert!(!active_resume_matches(&output, 0x1000));

        remove_resume_file(&output);
    }

    #[test]
    fn final_read_status_accepts_zero_and_rejects_error() {
        assert!(final_read_status_from_payload(&0u32.to_le_bytes()).is_ok());
        let err = final_read_status_from_payload(&1u32.to_le_bytes()).unwrap_err();
        assert!(err.contains("0x00000001"));
        assert!(final_read_status_from_payload(&[]).is_ok());
    }
}

// =============================================================================
// 设备重置 / 关闭 / vbmeta 修补
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 通过 DA 重启设备（XFlash CMD_RESET）
    /// 对齐刷机匣：0x010007 + param(storage=1, value=0x64)
    pub fn reset_device(&mut self) -> Result<(), String> {
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010007u32.to_le_bytes())?;
        let st = self.status()?;
        if st != 0 {
            return Err(format!("CMD_RESET status: 0x{:08X}", st));
        }

        // param: storage(4) + value(4) + zeros(20) = 28 字节
        let mut param = vec![0u8; 28];
        param[0..4].copy_from_slice(&1u32.to_le_bytes()); // storage=eMMC
        param[4..8].copy_from_slice(&100u32.to_le_bytes()); // value=0x64
        let param_pkt = pack3(CMD_MAGIC, 0x01, 28);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("CMD_RESET param status: 0x{:08X}", st2));
        }
        info!("设备已通过 DA 重启");
        Ok(())
    }

    pub fn close_device(&mut self, reset: bool) {
        if reset {
            if let Err(e) = self.preloader.jump_bl() {
                warn!("jump_bl 失败: {}", e);
            } else {
                info!("已发送 JUMP_BL 命令，设备将重启");
            }
        }
    }

    /// 修补 vbmeta
    /// 委托给 security::vbmeta::vbmeta_disable 实现（自动处理 vbmeta_a / vbmeta_b / vbmeta）
    pub fn patch_vbmeta(&mut self, mode: u32) -> Result<(), String> {
        crate::security::vbmeta::vbmeta_disable(self, mode)
    }
}

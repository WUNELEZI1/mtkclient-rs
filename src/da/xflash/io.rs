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

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{CMD_MAGIC, CMD_READ_DATA, pack3};

// =============================================================================
// Flash 数据读取
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 读取 flash 数据，返回原始字节（默认 USER 分区类型）
    pub(crate) fn readflash_data(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        self.readflash_data_ex(addr, size, 8)
    }

    /// 读取 flash 数据到文件（流式写入，带进度回调）
    /// 每个 USB 数据包收到后立即写入文件并更新进度，避免全量内存占用。
    /// 支持断点续传：通过 start_offset 指定从何处开始读取。
    ///
    /// 回调 on_packet(total_bytes_read: u64) 在每包写入后被调用。
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

        // 3. 流式读取循环：每包 → 写文件 → 回调
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(output_file)
            .map_err(|e| format!("打开文件失败: {}", e))?;

        let mut total_read: u64 = start_offset;
        let target_remaining = size - start_offset;
        let mut bytes_received: u64 = 0;

        while bytes_received < target_remaining {
            // 读 12 字节头
            let mut hdr = [0u8; 12];
            match self.preloader.device.read_exact(&mut hdr) {
                Ok(0) => {
                    trace!("[readflash_to_file] ZLP on header read, ending loop");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    trace!("[readflash_to_file] read header error: {}", e);
                    break;
                }
            }

            let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let slength = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);

            if magic != CMD_MAGIC {
                trace!(
                    "[readflash_to_file] bad magic: 0x{:08X} at offset {}, ending",
                    magic, total_read
                );
                break;
            }

            // 读数据
            let mut data = vec![0u8; slength as usize];
            if slength > 0
                && let Err(e) = self.preloader.device.read_exact(&mut data)
            {
                trace!("[readflash_to_file] read data error: {}", e);
                break;
            }

            // 心跳包跳过
            if slength == 4 && data.iter().all(|&b| b == 0) {
                trace!("[readflash_to_file] 心跳包，跳过");
                continue;
            }

            // 写入文件
            file.write_all(&data)
                .map_err(|e| format!("写入文件失败: {}", e))?;

            bytes_received += data.len() as u64;
            total_read += data.len() as u64;

            // 每 1MB flush 一次（平衡 IO 性能和数据安全）
            if bytes_received % (1024 * 1024) < data.len() as u64 {
                file.flush().ok();
            }

            // 更新进度
            on_packet(total_read);

            // 最后一包判断
            if bytes_received >= target_remaining {
                trace!("[readflash_to_file] 最后一包，跳过 ACK");
                break;
            }
            if let Err(e) = self.ack_silent() {
                trace!("[readflash_to_file] send_ack failed: {}", e);
                break;
            }
        }

        file.flush().map_err(|e| format!("flush 文件失败: {}", e))?;
        trace!("[readflash_to_file] total read {} bytes", total_read);
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

            if remaining == 0 {
                trace!("[readflash_data] 最后一包，跳过 ACK");
                break;
            }
            if let Err(e) = self.ack_silent() {
                trace!("[readflash_data] send_ack failed: {}", e);
                break;
            }
        }

        trace!("[readflash_data] total read {} bytes", buffer.len());
        Ok(buffer)
    }

    /// 静默 ACK（仅发不读），用于 readflash_data 循环中不偷吃下一个包
    fn ack_silent(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&hdr, "ack_silent hdr")?;
        self.write_with_retry(&0u32.to_le_bytes(), "ack_silent data")?;
        Ok(())
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

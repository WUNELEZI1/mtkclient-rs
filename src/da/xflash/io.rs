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
        use std::io::{BufWriter, Write};
        use std::sync::mpsc::{self, Receiver, SyncSender};

        // 对齐 Python readflash：在 cmd_read_data 之前先查询 get_packet_length
        let _ = self.send_devctrl(0x040007, None);
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

        // === 激进优化参数 ===
        const CHANNEL_CAP: usize = 128;
        const BATCH_SIZE: usize = 16 * 1024 * 1024; // 16MB batch
        const PROGRESS_INTERVAL: u64 = 1 * 1024 * 1024; // 1MB 进度更新
        const MAX_PACKET_SIZE: usize = 0x1000000; // 16MB 预分配 buffer
        const BUF_WRITER_CAP: usize = 64 * 1024 * 1024; // 64MB BufWriter

        let (tx, rx): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) = mpsc::sync_channel(CHANNEL_CAP);
        let output_path = output_file.to_string();
        let target_remaining = size - start_offset;

        // 启动后台写入线程（BufWriter 64MB + 文件预分配）
        let writer_thread = std::thread::Builder::new()
            .name("flash_writer".into())
            .spawn(move || -> Result<(), String> {
                use std::fs::File;

                let raw_file = if start_offset > 0 {
                    // 断点续传：追加模式，不截断已有数据
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&output_path)
                        .map_err(|e| format!("打开文件失败: {}", e))?
                } else {
                    File::create(&output_path)
                        .map_err(|e| format!("创建文件失败: {}", e))?
                };
                let mut file = BufWriter::with_capacity(BUF_WRITER_CAP, raw_file);

                while let Ok(data) = rx.recv() {
                    if data.is_empty() {
                        break;
                    }
                    file.write_all(&data)
                        .map_err(|e| format!("写入文件失败: {}", e))?;
                }
                file.flush().map_err(|e| format!("flush 文件失败: {}", e))?;
                trace!("[writer] 写入线程结束，总计写入 {} 字节", target_remaining);
                Ok(())
            });

        // ---- 主线程：USB 读取循环（batch 累积模式）----
        let mut total_read: u64 = start_offset;
        let mut bytes_received: u64 = 0;
        let mut data_buf = vec![0u8; MAX_PACKET_SIZE];
        let mut last_progress_pos: u64 = 0;
        let mut read_error: Option<String> = None;

        // batch 累积 buffer
        let mut batch: Vec<u8> = Vec::with_capacity(BATCH_SIZE);

        while bytes_received < target_remaining && read_error.is_none() {
            // 读 12 字节头
            let mut hdr = [0u8; 12];
            match self.preloader.device.read_exact(&mut hdr) {
                Ok(0) => {
                    trace!("[readflash] ZLP on header read, ending");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    read_error = Some(format!("read header: {}", e));
                    break;
                }
            }

            let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let slength = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);

            if magic != CMD_MAGIC {
                trace!(
                    "[readflash] bad magic: 0x{:08X} at offset {}, ending",
                    magic, total_read
                );
                break;
            }

            if slength == 4 {
                self.preloader.device.read_exact(&mut data_buf[..4]).ok();
                if data_buf[0] == 0 && data_buf[1] == 0 && data_buf[2] == 0 && data_buf[3] == 0 {
                    trace!("[readflash] 心跳包，跳过");
                    continue;
                }
                batch.extend_from_slice(&data_buf[..4]);
                bytes_received += 4;
                total_read += 4;
            } else if slength == 0 {
                continue;
            } else if (slength as usize) <= MAX_PACKET_SIZE {
                let slen = slength as usize;
                if let Err(e) = self.preloader.device.read_exact(&mut data_buf[..slen]) {
                    read_error = Some(format!("read data: {}", e));
                    break;
                }
                batch.extend_from_slice(&data_buf[..slen]);
                bytes_received += slen as u64;
                total_read += slen as u64;
            } else {
                let mut data = vec![0u8; slength as usize];
                if let Err(e) = self.preloader.device.read_exact(&mut data) {
                    read_error = Some(format!("read data (large): {}", e));
                    break;
                }
                batch.extend_from_slice(&data);
                bytes_received += data.len() as u64;
                total_read += data.len() as u64;
            }

            // batch 满或最后一包时发送
            if batch.len() >= BATCH_SIZE || bytes_received >= target_remaining {
                if tx.send(std::mem::take(&mut batch)).is_err() {
                    read_error = Some("写入线程已终止".into());
                    break;
                }
                batch = Vec::with_capacity(BATCH_SIZE);
            }

            // 进度条更新（每 16MB）
            if total_read - last_progress_pos >= PROGRESS_INTERVAL {
                on_packet(total_read);
                last_progress_pos = total_read;
            }

            // 最后一包判断
            if bytes_received >= target_remaining {
                trace!("[readflash] 最后一包，跳过 ACK");
                break;
            }
            if let Err(e) = self.ack_silent() {
                read_error = Some(format!("send_ack: {}", e));
                break;
            }
        }

        // 发送剩余的 batch
        if !batch.is_empty() && read_error.is_none() {
            let _ = tx.send(batch);
        }

        let writer_handle = match writer_thread {
            Ok(handle) => handle,
            Err(e) => return Err(format!("创建写入线程失败: {}", e)),
        };

        // 结束哨兵
        let _ = tx.send(Vec::new());

        if let Err(e) = writer_handle.join().unwrap_or(Ok(())) {
            read_error = Some(e);
        }

        on_packet(total_read);

        if let Some(e) = read_error {
            return Err(e);
        }

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

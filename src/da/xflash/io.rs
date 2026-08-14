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
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{CMD_MAGIC, CMD_READ_DATA, GET_PKT_LEN, pack3};
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
    crate::resume::read_resume_path(output_file)
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
    // 检查输出文件是否存在
    let file_size = match std::fs::metadata(output_file) {
        Ok(m) => m.len(),
        Err(_) => return false,
    };

    let Ok(content) = std::fs::read_to_string(resume_path_for(output_file)) else {
        return false;
    };

    let has_active_read = crate::resume::is_active_read(&content);
    let resume_written = crate::resume::parse_u64_field(&content, "written=");

    match resume_written {
        Some(written) => {
            // 允许 512 字节容差：实际文件大小和 resume 记录可能因对齐有微小差异
            let size_match = file_size.abs_diff(written) <= 512;
            let offset_match = start_offset.abs_diff(written) <= 512;
            has_active_read && size_match && offset_match
        }
        None => false,
    }
}

fn spawn_dump_writer(
    output_file: &str,
    addr: u64,
    size: u64,
    parttype: u32,
    start_offset: u64,
    packet_len: Option<usize>,
) -> Result<
    (
        SyncSender<Option<Vec<u8>>>,
        Receiver<Vec<u8>>,
        std::thread::JoinHandle<Result<u64, String>>,
    ),
    String,
> {
    const CHANNEL_CAP: usize = 128;
    const FLUSH_INTERVAL: u64 = 128 * 1024 * 1024;
    const RESUME_INTERVAL: u64 = 64 * 1024 * 1024;
    const BUF_WRITER_CAP: usize = 64 * 1024 * 1024;

    let output_path = output_file.to_string();
    let (tx, rx): (SyncSender<Option<Vec<u8>>>, Receiver<Option<Vec<u8>>>) =
        mpsc::sync_channel(CHANNEL_CAP);
    let (recycle_tx, recycle_rx): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) =
        mpsc::sync_channel(CHANNEL_CAP);

    let handle = std::thread::Builder::new()
        .name("flash_dump_writer".to_string())
        .spawn(move || -> Result<u64, String> {
            use std::io::{BufWriter, Write};

            let raw_file = if start_offset > 0 {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&output_path)
                    .map_err(|e| format!("打开文件失败 '{}': {}", output_path, e))?
            } else {
                std::fs::File::create(&output_path)
                    .map_err(|e| format!("创建文件失败 '{}': {}", output_path, e))?
            };
            let mut file = BufWriter::with_capacity(BUF_WRITER_CAP, raw_file);
            let mut written = start_offset;
            let mut last_flush_pos = start_offset;
            let mut last_resume_pos = start_offset;

            while let Ok(message) = rx.recv() {
                let Some(data) = message else {
                    break;
                };
                file.write_all(&data)
                    .map_err(|e| format!("写入文件失败: {}", e))?;
                written += data.len() as u64;
                let mut data = data;
                data.clear();
                let _ = recycle_tx.send(data);

                if written - last_flush_pos >= FLUSH_INTERVAL || written >= size {
                    file.flush().map_err(|e| format!("flush 文件失败: {}", e))?;
                    last_flush_pos = written;
                }
                if written - last_resume_pos >= RESUME_INTERVAL || written >= size {
                    write_resume_file(
                        &output_path,
                        addr,
                        size,
                        parttype,
                        written,
                        true,
                        packet_len,
                    )?;
                    last_resume_pos = written;
                }
            }

            file.flush().map_err(|e| format!("flush 文件失败: {}", e))?;
            write_resume_file(
                &output_path,
                addr,
                size,
                parttype,
                written,
                true,
                packet_len,
            )?;
            Ok(written)
        })
        .map_err(|e| format!("创建写入线程失败: {}", e))?;

    Ok((tx, recycle_rx, handle))
}

fn finish_dump_writer(
    tx: SyncSender<Option<Vec<u8>>>,
    handle: std::thread::JoinHandle<Result<u64, String>>,
) -> Result<u64, String> {
    let _ = tx.send(None);
    handle.join().map_err(|_| "写入线程 panic".to_string())?
}

fn acquire_dump_buffer(recycle_rx: &Receiver<Vec<u8>>, len: usize) -> Vec<u8> {
    let mut buf = recycle_rx
        .try_recv()
        .unwrap_or_else(|_| Vec::with_capacity(len));
    if buf.capacity() < len {
        buf.reserve(len - buf.capacity());
    }
    buf.resize(len, 0);
    buf
}

fn read_header_with_optional_queue(
    device: &mut dyn crate::preloader::transport::BromTransport,
    hdr: &mut [u8; 12],
    queued_header: &mut bool,
) -> Result<(), String> {
    if *queued_header {
        let got = device.complete_read_request(hdr)?;
        *queued_header = false;
        if got < hdr.len() {
            device.read_exact(&mut hdr[got..])?;
        }
    } else {
        device.read_exact(hdr)?;
    }
    Ok(())
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

    /// 发送 READ_DATA 命令及 56B 参数（提取公共逻辑，消除重复）
    fn send_read_data_cmd(&mut self, addr: u64, size: u64, parttype: u32) -> Result<(), String> {
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "readflash xsend")?;
        self.write_with_retry(&CMD_READ_DATA.to_le_bytes(), "readflash CMD")?;
        let st = self.status()?;
        if st != 0 {
            return Err(format!("READ_DATA status=0x{:08X}", st));
        }

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
        Ok(())
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
        trace!(
            "[readflash_to_file] addr=0x{:08X} size={} parttype={} output={} start_offset={}",
            addr, size, parttype, output_file, start_offset
        );
        let _quiet_guard = quiet_usb_reads_temporarily();

        const PROGRESS_INTERVAL: u64 = 4 * 1024 * 1024; // 4MB 进度更新（平衡精度和开销）
        const MAX_PACKET_SIZE: usize = 0x1000000; // 16MB 预分配 buffer

        let target_remaining = size - start_offset;
        ensure_output_file_path(output_file)?;

        let active_resume = start_offset > 0 && active_resume_matches(output_file, start_offset);
        let mut packet_len = None;

        if active_resume {
            info!(
                "检测到活跃读取流，发送 ACK 后从 {} 字节继续接收",
                start_offset
            );
            // 续传 ACK 增加重试：设备可能刚恢复，第一次 ACK 可能超时
            let mut ack_ok = false;
            for attempt in 0..3 {
                match self.ack_silent() {
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
                // ACK 全部失败：设备可能已断开，清理 resume 文件避免无限死循环
                remove_resume_file(output_file);
                return Err("活跃读取流续接 ACK 全部失败".to_string());
            }
        } else {
            // 对齐 Python readflash：在 cmd_read_data 之前先查询 get_packet_length
            // send_devctrl 内部已包含完整的 xread + status 握手
            match self.send_devctrl(GET_PKT_LEN, None) {
                Ok(data) => {
                    packet_len = parse_packet_length(&data);
                    if let Some(packet_len) = packet_len {
                        trace!(
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

            // 发送 READ_DATA 命令及参数
            // 关键修复：非活跃续传分支(start_offset>0 但 resume 不匹配)下，文件以
            // append 模式打开（spawn_dump_writer），必须从 addr+start_offset 续读，
            // 否则会读取分区开头段并追加到已有前缀后，生成内容错乱的损坏镜像。
            self.send_read_data_cmd(addr + start_offset, target_remaining, parttype)?;
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
                self.preloader.device.as_mut(),
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
                let mut data = acquire_dump_buffer(&recycle_rx, 4);
                self.preloader
                    .device
                    .read_exact(&mut data)
                    .map_err(|e| format!("read data: {}", e))?;
                if data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 0 {
                    trace!("[readflash] 心跳包，跳过");
                    continue;
                }
                writer_tx
                    .send(Some(data))
                    .map_err(|_| "写入线程已退出".to_string())?;
                bytes_received += 4;
                total_read += 4;
            } else if slength == 0 {
                continue;
            } else if (slength as usize) <= MAX_PACKET_SIZE {
                let slen = slength as usize;
                let data = if self.da_x_speed >= 3 {
                    self.preloader
                        .device
                        .read_exact_vec(slen)
                        .map_err(|e| format!("read data fast: {}", e))?
                } else {
                    let mut data = acquire_dump_buffer(&recycle_rx, slen);
                    self.preloader
                        .device
                        .read_exact(&mut data)
                        .map_err(|e| format!("read data: {}", e))?;
                    data
                };
                writer_tx
                    .send(Some(data))
                    .map_err(|_| "写入线程已退出".to_string())?;
                bytes_received += slen as u64;
                total_read += slen as u64;
            } else {
                let mut data = vec![0u8; slength as usize];
                self.preloader
                    .device
                    .read_exact(&mut data)
                    .map_err(|e| format!("read data (large): {}", e))?;
                let data_len = data.len() as u64;
                writer_tx
                    .send(Some(data))
                    .map_err(|_| "写入线程已退出".to_string())?;
                bytes_received += data_len;
                total_read += data_len;
            }

            if total_read - last_progress_pos >= PROGRESS_INTERVAL
                || bytes_received >= target_remaining
            {
                on_packet(total_read);
                last_progress_pos = total_read;
            }

            if crate::cancel::force_requested() {
                self.preloader.device.cancel_pending_transfers();
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

            // 优化：先 ACK 再预提交 header
            // ACK (OUT) → 设备收到后开始准备下一包 → 预提交 header (IN) 顺势捕获
            if let Err(e) = self.ack_silent() {
                self.preloader.device.cancel_pending_transfers();
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

            // ACK 后预提交下一包 header：设备已收到 ACK，正在准备下一包数据
            queued_header = self
                .preloader
                .device
                .submit_read_request(12)
                .unwrap_or(false);
        }

        self.readflash_final_status()?;
        let written = finish_dump_writer(writer_tx, writer_handle)?;
        remove_resume_file(output_file);
        on_packet(written);

        trace!("[readflash] total read {} bytes", written);
        Ok(written)
    }

    /// 读取 flash 数据（全量到内存），用于小分区或需要内存操作的场景
    pub(crate) fn readflash_data_ex(
        &mut self,
        addr: u64,
        size: u64,
        parttype: u32,
    ) -> Result<Vec<u8>, String> {
        // 防御：禁止把超大分区整块读入内存（避免 OOM 崩溃）。
        // 大分区请走流式读取（readflash_to_file / r <分区> <文件> / rl <目录>）。
        const MAX_IN_MEMORY_READ: u64 = 256 * 1024 * 1024; // 256 MiB
        if size > MAX_IN_MEMORY_READ {
            return Err(format!(
                "分区过大 ({:.2} GiB)，无法整块读入内存。请使用流式读取：r <分区> <文件> 或 rl <目录>",
                size as f64 / 1024.0 / 1024.0 / 1024.0
            ));
        }

        // 1. get_packet_length — send_devctrl 内部已包含完整的 xread + status 握手
        //    注意：部分设备/DA（尤其 Preloader 模式会话复用）对 GET_PKT_LEN 返回异常
        //    status，但包长度仅用于优化分块，读取循环按设备实际返回的分包处理，
        //    故失败时不中断（对齐 readflash_to_file 的容错行为），避免 printgpt 等命令无谓失败。
        match self.send_devctrl(GET_PKT_LEN, None) {
            Ok(_) => {}
            Err(e) => warn!(
                "readflash_data_ex: get_packet_length 获取失败（已忽略，继续读取）: {}",
                e
            ),
        };

        // 2. 发送 READ_DATA 命令及参数
        self.send_read_data_cmd(addr, size, parttype)?;

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

        // 完整性校验：① 设备可能提前结束（ZLP / 读错误 / 坏 magic / ACK 失败）；
        // ② 心跳包(slength==4 全零)与零长包被 continue 跳过，不计入 buffer。
        // 若实际收到的真实数据字节数 < size，静默返回会让上层（GPT 解析、
        // preloader 提取、分区校验等）拿到不完整的“假成功”结果，故显式报错。
        if buffer.len() != size as usize {
            return Err(format!(
                "读取数据不完整: 期望 {} 字节，实际仅收到 {} 字节（设备提前结束数据传输）",
                size,
                buffer.len()
            ));
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
    /// 优化：将 12B header + 4B data 合并为单次 USB OUT transfer，减少一次 USB 调用
    fn ack_silent(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        let mut buf = [0u8; 16];
        buf[..12].copy_from_slice(&hdr);
        buf[12..16].copy_from_slice(&0u32.to_le_bytes());
        self.write_with_retry(&buf, "ack_silent")?;
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
        let _ = std::fs::remove_file(&output);
        remove_resume_file(&output);

        // 创建与 written=0x2000 匹配的输出文件（active_resume_matches 需要文件存在且大小匹配）
        std::fs::write(&output, vec![0u8; 0x2000]).unwrap();
        write_resume_file(&output, 0x1000, 0x4000, 8, 0x2000, true, Some(0x1000)).unwrap();

        assert!(active_resume_matches(&output, 0x2000));
        assert!(!active_resume_matches(&output, 0x1000));

        let _ = std::fs::remove_file(&output);
        remove_resume_file(&output);
    }

    #[test]
    fn dump_writer_writes_data_and_resume_metadata() {
        let output = std::env::temp_dir().join(format!("dump_writer_{}.img", std::process::id()));
        let output = output.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&output);
        remove_resume_file(&output);

        let (tx, _recycle_rx, handle) =
            spawn_dump_writer(&output, 0x1000, 6, 8, 0, Some(2)).unwrap();
        tx.send(Some(vec![1, 2, 3])).unwrap();
        tx.send(Some(vec![4, 5, 6])).unwrap();
        let written = finish_dump_writer(tx, handle).unwrap();

        assert_eq!(written, 6);
        assert_eq!(std::fs::read(&output).unwrap(), vec![1, 2, 3, 4, 5, 6]);
        let resume = std::fs::read_to_string(resume_path_for(&output)).unwrap();
        assert!(resume.contains("written=6"));
        assert!(resume.contains("packet_len=2"));

        let _ = std::fs::remove_file(&output);
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
    /// 通过 DA 重启设备（reboot 到系统）。
    ///
    /// 内部调用 `da_shutdown(Normal)`；真正的硬件重启由 `da_shutdown` 的
    /// `enablewdt=1` 触发看门狗超时完成（详见 `da_shutdown`）。
    /// 注意：设备须处于可下发 SHUTDOWN 的 idle 状态（不在 mid-read 数据流态），
    /// 否则 DA 会拒绝该命令 —— 调用方（cmd_reboot）已对“未完成的读取”做拦截。
    pub fn reset_device(&mut self) -> Result<(), String> {
        self.da_shutdown(crate::da::xflash::protocol::ShutdownBootMode::Normal)
    }
}

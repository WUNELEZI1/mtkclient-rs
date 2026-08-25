//! 读取路径方法（readflash_* 流水线）
//!
//! 见 [`crate::da::xflash::io`] 模块文档。共享的私有 free fn 由 `super` 提供。

use log::{info, trace, warn};
use std::time::Duration;

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{CMD_MAGIC, CMD_READ_DATA, GET_PKT_LEN, SET_PKT_LEN, pack3};

use super::{
    acquire_dump_buffer, active_resume_matches, ensure_output_file_path, final_read_status_from_payload,
    finish_dump_writer, parse_packet_length, quiet_usb_reads_temporarily, read_header_with_optional_queue,
    remove_resume_file, spawn_dump_writer, with_read_timeout, write_resume_file, READFLASH_READ_TIMEOUT_MS,
};

impl<'a> DAXFlash<'a> {
    /// 读取 flash 数据，返回原始字节（默认 USER 分区类型）
    pub(crate) fn readflash_data(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        self.readflash_data_ex(addr, size, 8)
    }

    /// 发送 READ_DATA 命令及 56B 参数（提取公共逻辑，消除重复）
    pub(crate) fn send_read_data_cmd(&mut self, addr: u64, size: u64, parttype: u32) -> Result<(), String> {
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "readflash xsend")?;
        self.write_with_retry(&CMD_READ_DATA.to_le_bytes(), "readflash CMD")?;
        let st = self.status()?;
        if st != 0 {
            return Err(format!("READ_DATA status=0x{:08X}", st));
        }

        // 56B 参数直接在栈上构造，避免每次读取都堆分配一个 Vec
        let mut param = [0u8; 56];
        param[..4].copy_from_slice(&1u32.to_le_bytes());
        param[4..8].copy_from_slice(&parttype.to_le_bytes());
        param[8..16].copy_from_slice(&addr.to_le_bytes());
        param[16..24].copy_from_slice(&size.to_le_bytes());
        // param[24..56] 保持为零填充（[0u8; 56] 初值）
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
    /// 架构：
    /// - 主线程：USB 读取循环（读头 → 读数据 → 放入 channel → 发 ACK）
    /// - 写入线程：从 channel 取数据 → 写文件（64MB BufWriter 缓冲）
    ///
    /// channel 容量 128（约 512MB @ 4MB/包），主线程满时阻塞等待写入线程消费，
    /// 使 USB 读取不被磁盘写入阻塞，实现真正的流水线并行。
    /// 数据包缓冲通过 recycle channel 复用，避免逐包堆分配（心跳包用栈缓冲短路，不占回收池）。
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

        // 串口默认超时仅 1s，readflash 流式读取持续数秒，中途包间隔超 1s 即超时失败。
        // 用较长读超时包裹整个读取逻辑（RAII，覆盖所有提前 return），结束后自动恢复。
        with_read_timeout(self, READFLASH_READ_TIMEOUT_MS, |da| {
        const PROGRESS_INTERVAL: u64 = 4 * 1024 * 1024; // 4MB 进度更新（平衡精度和开销）
        const MAX_PACKET_SIZE: usize = 0x1000000; // 16MB 预分配 buffer

        if start_offset > size {
            return Err(format!(
                "续传偏移 {} 超过分区大小 {}，resume 文件可能损坏",
                start_offset, size
            ));
        }
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
                // ACK 全部失败：设备可能已断开，清理 resume 文件避免无限死循环
                remove_resume_file(output_file);
                return Err("活跃读取流续接 ACK 全部失败".to_string());
            }
        } else {
            // 对齐 Python readflash：在 cmd_read_data 之前先查询 get_packet_length
            // send_devctrl 内部已包含完整的 xread + status 握手
            // 性能关键：刷机匣实测把工作传输单元设到 2MB（0x200000）批量传输，故这里
            // 主动 SET_PKT_LEN 抬升。DA 不支持时 send_devctrl 回空/报错，忽略即可（无回归）。
            let reported = match da.send_devctrl(GET_PKT_LEN, None) {
                Ok(data) => parse_packet_length(&data),
                Err(e) => {
                    trace!("获取 DA 读包长度失败: {}", e);
                    None
                }
            };
            let mut target = match reported {
                Some(v) if v >= 0x200000 => v.min(0x400000),
                _ => 0x200000,
            };
            // 主动 SET_PKT_LEN 抬升。SET 成功后重新 GET 确认 DA 实际接受的包长度：
            // 部分 DA 只接受更小的值，盲目按请求值传输会触发 DA 端溢出/截断。
            // 仅当 DA 明确回报更小正数时才回退，避免把 GET 失败/返回 0 误判为"更小"。
            if da.send_devctrl(SET_PKT_LEN, Some(&(target as u32).to_le_bytes())).is_ok() {
                trace!(
                    "DA 读包长度已提升至 {} 字节 ({:.2} MiB)",
                    target,
                    target as f64 / 1024.0 / 1024.0
                );
                if let Ok(ack) = da.send_devctrl(GET_PKT_LEN, None) {
                    if let Some(actual) = parse_packet_length(&ack) {
                        if actual > 0 && actual < target {
                            warn!(
                                "DA 实际接受读包长度 {} < 请求 {}，回退以避免 DA 端溢出",
                                actual, target
                            );
                            target = actual;
                        }
                    }
                }
            } else {
                trace!("SET_PKT_LEN 不受支持（已忽略）");
            }
            packet_len = Some(target);

            // 发送 READ_DATA 命令及参数
            // 关键修复：非活跃续传分支(start_offset>0 但 resume 不匹配)下，文件以
            // append 模式打开（spawn_dump_writer），必须从 addr+start_offset 续读，
            // 否则会读取分区开头段并追加到已有前缀后，生成内容错乱的损坏镜像。
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
                // 先以栈缓冲判定心跳：心跳包(全零)直接跳过，不占用/泄漏 recycle 缓冲池，
                // 否则长读取过程中反复心跳会耗尽回收池、退化为逐包新分配。
                let mut tiny = [0u8; 4];
                da.preloader
                    .device
                    .read_exact(&mut tiny)
                    .map_err(|e| format!("read data: {}", e))?;
                if tiny.iter().all(|&b| b == 0) {
                    trace!("[readflash] 心跳包，跳过");
                    continue;
                }
                // 非心跳的 4 字节真实小包极少见，这里直接小分配走回收池
                writer_tx
                    .send(Some(tiny.to_vec()))
                    .map_err(|_| "写入线程已退出".to_string())?;
                bytes_received += 4;
                total_read += 4;
            } else if slength == 0 {
                continue;
            } else if (slength as usize) <= MAX_PACKET_SIZE {
                let slen = slength as usize;
                let data = if da.da_x_speed >= 3 {
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
                writer_tx
                    .send(Some(data))
                    .map_err(|_| "写入线程已退出".to_string())?;
                bytes_received += slen as u64;
                total_read += slen as u64;
            } else {
                let mut data = vec![0u8; slength as usize];
                da.preloader
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

            // 优化：先 ACK 再预提交 header
            // ACK (OUT) → 设备收到后开始准备下一包 → 预提交 header (IN) 顺势捕获
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

            // ACK 后预提交下一包 header：设备已收到 ACK，正在准备下一包数据
            queued_header = da
                .preloader
                .device
                .submit_read_request(12)
                .unwrap_or(false);
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
        // 串口默认超时仅 1s，final status header 读取可能因设备收尾延迟超时而失败
        // （典型报错 "readflash final status header: serial read_exact: Operation timed out"）。
        // 用较长读超时包裹本次读取，结束后自动恢复原超时。
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

    /// 静默 ACK（仅发不读），用于 readflash_data 循环中不偷吃下一个包
    /// 优化：将 12B header + 4B data 合并为单次 USB OUT transfer，减少一次 USB 调用
    pub(crate) fn ack_silent(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        let mut buf = [0u8; 16];
        buf[..12].copy_from_slice(&hdr);
        buf[12..16].copy_from_slice(&0u32.to_le_bytes());
        self.write_with_retry(&buf, "ack_silent")?;
        Ok(())
    }
}

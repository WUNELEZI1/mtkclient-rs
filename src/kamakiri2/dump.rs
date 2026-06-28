//! Kamakiri2 Preloader/BROM dump
//!
//! - [`Preloader::dump_brom`]：通用 BROM 数据 dump（先读 4 字节 BE 长度，再读数据）
//! - [`Preloader::dump_preloader_from_ram`]：从 RAM 中 dump preloader（通过 read32_brom）
//! - [`Preloader::dump_preloader_payload`]：通过 generic_preloader_dump_payload 提取完整 preloader

use log::{debug, info, trace, warn};
use std::time::Duration;

use crate::debug_log;
use crate::paths::获取可执行文件相对路径;
use crate::preloader::Preloader;
use crate::usb::UsbContext;

impl Preloader {
    pub fn dump_brom(&mut self, debug: bool) -> Result<Vec<u8>, String> {
        info!("等待 BROM 发送 dump 长度...");

        let mut len_buf = [0u8; 4];
        self.device
            .read(&mut len_buf)
            .map_err(|e| format!("读取长度失败: {}", e))?;
        let length = u32::from_be_bytes(len_buf) as usize;
        debug_log!(debug, "BROM 数据长度: 0x{:X} ({}) 字节", length, length);

        let mut all_data = Vec::with_capacity(length);
        let mut remaining = length;
        const CHUNK: usize = 0x4000;

        while remaining > 0 {
            let chunk_size = if remaining > CHUNK { CHUNK } else { remaining };
            let mut buf = vec![0u8; chunk_size];
            self.device
                .read(&mut buf)
                .map_err(|e| format!("读取 BROM 数据失败: {}", e))?;
            all_data.extend_from_slice(&buf);
            remaining -= chunk_size;

            let progress = (all_data.len() as f64 / length as f64) * 100.0;
            eprint!("\r  进度: {:.1}% ({}/{})", progress, all_data.len(), length);
        }
        trace!("\n");

        Ok(all_data)
    }

    /// 从 RAM 中 dump preloader（Kamakiri2 漏洞利用后）
    #[allow(dead_code)]
    pub fn dump_preloader_from_ram(&mut self, debug: bool) -> Result<Vec<u8>, String> {
        info!("正在从 RAM 提取 Preloader...");

        let start_addr: u32 = 0x200000;
        let initial_dwords: usize = 0x10000 / 4;
        let data = self.read32_brom(start_addr, initial_dwords)?;
        let initial_bytes: Vec<u8> = data.iter().flat_map(|&w| w.to_le_bytes()).collect();

        debug_log!(debug, "  原始 RAM 数据前 64 字节:");
        for i in (0..64.min(initial_bytes.len())).step_by(16) {
            let end = (i + 16).min(initial_bytes.len());
            let hex_str: String = initial_bytes[i..end]
                .iter()
                .map(|b| format!("{:02X} ", b))
                .collect();
            debug_log!(debug, "    [0x{:04X}] {}", i, hex_str);
        }

        let signature: [u8; 8] = [0x4D, 0x4D, 0x4D, 0x01, 0x38, 0x00, 0x00, 0x00];
        let mut idx = 0;
        for i in 0..initial_bytes.len().saturating_sub(7) {
            if initial_bytes[i..i + 8] == signature {
                idx = i;
                break;
            }
        }
        if idx == 0 {
            return Err("未在 RAM 中找到 Preloader 签名".to_string());
        }

        info!("  在偏移 0x{:X} 找到 Preloader 签名", idx);
        let data = &initial_bytes[idx..];
        if data.len() < 0x24 {
            return Err("Preloader 数据太短".to_string());
        }

        let length = u32::from_le_bytes([data[0x20], data[0x21], data[0x22], data[0x23]]) as usize;
        info!("  Preloader 长度: 0x{:X} ({} 字节)", length, length);

        let mut all_data = Vec::with_capacity(length);
        all_data.extend_from_slice(data);

        let startidx = idx;
        let multiplier: usize = 32;
        let mut current_idx = idx;

        while (current_idx - startidx) < length {
            let dwords = 4 * multiplier;
            match self.read32_brom(start_addr + current_idx as u32, dwords) {
                Ok(chunk_data) => {
                    let chunk_bytes: Vec<u8> =
                        chunk_data.iter().flat_map(|&w| w.to_le_bytes()).collect();
                    all_data.extend_from_slice(&chunk_bytes);
                    current_idx += chunk_bytes.len();
                }
                Err(e) => {
                    warn!("在 0x{:X} 读取失败: {}", start_addr + current_idx as u32, e);
                    break;
                }
            }
        }

        let preloader = if all_data.len() > length {
            all_data[..length].to_vec()
        } else {
            all_data
        };

        if let Some(info_idx) = preloader.windows(16).position(|w| w == b"MTK_BLOADER_INFO") {
            let filename_start = info_idx + 0x1B;
            let filename_end = std::cmp::min(filename_start + 0x30, preloader.len());
            let filename_bytes = &preloader[filename_start..filename_end];
            let filename_len = filename_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(filename_bytes.len());
            let filename = String::from_utf8_lossy(&filename_bytes[..filename_len]).to_string();
            info!("  Preloader 文件名: {}", filename);

            use std::fs::File;
            use std::io::Write;
            if !filename.is_empty() {
                match File::create(&filename) {
                    Ok(mut f) => {
                        if let Err(e) = f.write_all(&preloader) {
                            warn!("保存 preloader 失败: {}", e);
                        } else {
                            info!("  成功保存 preloader 到: {}", filename);
                        }
                    }
                    Err(e) => {
                        warn!("failed to create preloader file: {}", e);
                    }
                }
            }
        }

        info!("  Preloader dump complete: {} bytes", preloader.len());
        Ok(preloader)
    }

    /// 通过 generic_preloader_dump_payload 提取完整 preloader
    /// 流程：
    /// 1. 注入 generic_preloader_dump_payload.bin (ack=0xC1C2C3C4)
    /// 2. 设备在 0x200000-0x210000 搜索 preloader 头部 (4D 4D 4D 01 38)
    /// 3. 设备发回 4 字节 length (LE) + length 字节 preloader 数据
    pub fn dump_preloader_payload(
        &mut self,
        _debug: bool,
        quiet: bool,
        _context: &UsbContext,
    ) -> Result<(Vec<u8>, String), String> {
        // 静默模式：关闭 USB READ 调试日志
        if quiet {
            crate::usb::set_quiet_usb_read(true);
        }

        // 1. 加载并注入 generic_preloader_dump_payload
        let payload_path =
            获取可执行文件相对路径("payloads/generic_preloader_dump_payload.bin");
        let payload = std::fs::read(&payload_path)
            .map_err(|e| format!("读取 generic_preloader_dump_payload.bin 失败: {}", e))?;
        info!(
            "正在通过 generic_preloader_dump_payload 提取 Preloader ({} 字节)...",
            payload.len()
        );

        // 2. 注入 payload（inject_payload_use_chip_send_ptr 内部会验证 ack=0xC1C2C3C4，
        //    且不会 flush_input，保留设备后续发送的 length + preloader 数据）
        //    关键：dump_preloader_payload 必须用 inject_payload_use_chip_send_ptr，
        //    ptr_send = chip.send_ptr.0 = 0x0010286C，
        //    bra_addr = ptr_send - 0x40 = 0x0010282C（与 mtkclient read 路径 raw_value+8 一致）
        self.inject_payload_use_chip_send_ptr(&payload, 0xC1C2C3C4)?;

        // 3. 读 4 字节 length (LE)
        info!("等待设备发送 preloader 长度...");
        let mut len_buf = [0u8; 4];
        // 设置较长超时
        let orig_timeout = self.device.get_timeout();
        self.device.set_timeout(Duration::from_secs(20));
        self.device
            .read_exact(&mut len_buf)
            .map_err(|e| format!("读取 preloader 长度失败: {}", e))?;
        self.device.set_timeout(orig_timeout);

        let length = u32::from_le_bytes(len_buf) as usize;
        info!("Preloader 长度: {} 字节 (0x{:X})", length, length);

        if length == 0 {
            return Err("设备未找到 preloader（0x200000 范围无 MTK header）".into());
        }
        if length > 0x200000 {
            return Err(format!("preloader 长度异常: 0x{:X}", length));
        }

        // 4. 读 length 字节数据
        info!("正在读取 preloader 数据 ({} 字节)...", length);
        let mut all_data = Vec::with_capacity(length);
        let mut remaining = length;
        const CHUNK: usize = 0x4000; // 16KB 块
        while remaining > 0 {
            let chunk_size = std::cmp::min(remaining, CHUNK);
            let mut buf = vec![0u8; chunk_size];
            self.device
                .read_exact(&mut buf)
                .map_err(|e| format!("读取 preloader 数据失败 (剩余={}): {}", remaining, e))?;
            all_data.extend_from_slice(&buf);
            remaining -= chunk_size;
            let progress = (all_data.len() as f64 / length as f64) * 100.0;
            eprint!("\r  进度: {:.1}% ({}/{})", progress, all_data.len(), length);
        }
        trace!("\n");

        info!("已读取 {} 字节 preloader 数据", all_data.len());

        // 5. 提取文件名（从 MTK_BLOADER_INFO 头部）
        // MTK 头部格式（实测 2026-06-28 对照 archive/preloader_k69v1_64_k419.bin）:
        //   0x00: 4D 4D 4D 01
        //   0x04: hdr_size
        //   0x20: file_size (LE u32)
        //   ... 0x4D43C 附近: MTK_BLOADER_INFO_v40\0\0\0\0\0\0\0preloader_xxx.bin
        // 精确偏移：MTK_BLOADER_INFO 字符串本身 16 字节 + "_v40" (3 字节) + "\0" (1 字节)
        //          + "\0\0\0\0\0\0\0" (7 字节填充) = 27 (0x1B) 字节后是文件名
        // 之前用 +0x20 错 5 字节，rposition 找最后非零字节把 \0 后乱数据也读进去 → NUL 错误
        let filename =
            if let Some(info_idx) = all_data.windows(16).position(|w| w == b"MTK_BLOADER_INFO") {
                debug!("[dump] 找到 MTK_BLOADER_INFO 在偏移 0x{:X}", info_idx);
                // 对齐 Python: data[idx + 0x1B:idx + 0x1B + 0x30].rstrip(b"\x00")
                let filename_start = info_idx + 0x1B;
                let filename_end = std::cmp::min(filename_start + 0x30, all_data.len());
                let filename_bytes = &all_data[filename_start..filename_end];
                // 去除末尾的 \0（对齐 Python rstrip）
                let filename_str = String::from_utf8_lossy(filename_bytes)
                    .trim_end_matches('\0')
                    .to_string();
                if filename_str.is_empty() {
                    "preloader_dumped.bin".to_string()
                } else {
                    debug!("[dump] 提取的文件名: {}", filename_str);
                    filename_str
                }
            } else {
                debug!("[dump] 未找到 MTK_BLOADER_INFO，使用默认文件名");
                "preloader_dumped.bin".to_string()
            };

        std::fs::write(&filename, &all_data).map_err(|e| format!("保存失败: {}", e))?;

        info!(
            "Preloader dump 成功！已保存 {} ({} 字节)",
            filename,
            all_data.len()
        );
        Ok((all_data, filename))
    }
}

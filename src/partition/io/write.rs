//! 分区写入 / 擦除 / Bootloader 锁

use log::info;
use std::io::Read;
use crate::da::xflash::{CMD_FORMAT, CMD_MAGIC, DAXFlash, pack3};
use crate::partition::GptInfo;
use crate::partition::write::{check_write_resume, remove_write_resume_file};

/// 在文件末尾填充零字节的 Reader 包装器，用于 512 字节对齐。
struct PaddedReader<R: Read> {
    inner: R,
    remaining: u64, // 剩余需要读取的总字节数（包含填充）
}

impl<R: Read> PaddedReader<R> {
    fn new(inner: R, file_size: u64, pad_to: u64) -> Self {
        let padded = if file_size % pad_to != 0 {
            file_size + (pad_to - file_size % pad_to)
        } else {
            file_size
        };
        Self {
            inner,
            remaining: padded,
        }
    }
}

impl<R: Read> Read for PaddedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let to_read = std::cmp::min(buf.len() as u64, self.remaining) as usize;
        let n = self.inner.read(&mut buf[..to_read])?;
        if n == 0 {
            // 文件已读完，填充零字节
            let fill = std::cmp::min(buf.len(), self.remaining as usize);
            for b in &mut buf[..fill] {
                *b = 0;
            }
            self.remaining -= fill as u64;
            return Ok(fill);
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

impl<'a> DAXFlash<'a> {
    /// 写入文件到分区（带校验）
    /// 分块流式校验：避免大分区全量读入内存导致 OOM。
    /// 支持断点续传：续传时只校验本次写入的部分。
    pub fn write_partition_with_verify(
        &mut self, part_name: &str, in_file: &str
    ) -> Result<(), String> {
        // 先写入（写入分区内部处理断点续传）
        self.write_partition(part_name, in_file)?;

        // 分块流式校验：只校验本次写入的部分
        info!("  开始分块校验写入数据...");
        use std::io::{BufReader, Read, Seek, SeekFrom};
        let file = std::fs::File::open(in_file).map_err(|e| format!("读取原始文件失败: {}", e))?;
        let mut reader = BufReader::new(file);

        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据".to_string())?;
        let gpt_info = GptInfo::parse(gpt_data)?;
        let entry = gpt_info
            .find_partition(part_name)
            .ok_or_else(|| format!("未找到分区: {}", part_name))?;

        // 计算校验起始位置：写入可能续传，只校验本次写入的部分
        let write_size = {
            let file_meta =
                std::fs::metadata(in_file).map_err(|e| format!("无法获取文件元数据: {}", e))?;
            let file_size = file_meta.len();
            if file_size % 512 != 0 {
                file_size + (512 - file_size % 512)
            } else {
                file_size
            }
        };
        // 检查是否是续传写入（resume 文件已删除，但可通过写入大小推算）
        // 实际上 resume 文件已在 写入分区 成功后被删除，
        // 所以这里始终从 0 开始校验是安全的做法（写入已完成）。
        let verify_start = 0u64;

        // Seek 到校验起始位置
        if verify_start > 0 {
            reader
                .seek(SeekFrom::Start(verify_start))
                .map_err(|e| format!("文件 seek 失败: {}", e))?;
        }

        const VERIFY_CHUNK: usize = 0x100000; // 1MB 校验块（减少读取次数）
        let mut offset = verify_start;
        let mut buf = vec![0u8; VERIFY_CHUNK];

        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("读取原始文件失败: {}", e))?;
            if n == 0 {
                break;
            }
            // 不超过文件实际大小（避免校验 padding 的零字节）
            if offset >= write_size {
                break;
            }
            let effective_n = std::cmp::min(n, (write_size.saturating_sub(offset)) as usize);
            let chunk = &buf[..effective_n];
            let device_data = self.readflash_data(entry.start_addr + offset, effective_n as u64)?;
            if device_data != chunk {
                return Err(format!(
                    "校验失败 @ offset 0x{:X}: 写入数据与原始数据不匹配",
                    entry.start_addr + offset
                ));
            }
            offset += effective_n as u64;
        }

        info!("  校验通过 (共 {} 字节)", offset);
        Ok(())
    }

    /// 写入文件到分区（流式，避免大文件 OOM）
    /// 对齐 Python writeflash (xflash_lib.py:writeflash)
    /// 协议: cmd_write_data → 循环分包写入 [0x0(4B)][checksum(4B)][data] → CC_OPTIONAL_DOWNLOAD_ACT → status
    /// 增加校验：文件大小不能超过分区大小，超过时报错。
    /// 支持断点续传：取消后可通过 `.wresume` 文件从断点继续。
    pub fn write_partition(&mut self, part_name: &str, in_file: &str) -> Result<(), String> {
        info!("写入文件 {} 到分区 {}...", in_file, part_name);

        // 打开文件（流式读取，避免全量载入内存）
        let file = std::fs::File::open(in_file).map_err(|e| format!("无法打开文件: {}", e))?;
        let file_meta = file
            .metadata()
            .map_err(|e| format!("无法获取文件元数据: {}", e))?;
        let file_size = file_meta.len();

        // 先尝试解析特殊分区（boot1/boot2/rpmb）
        let (parttype, addr, part_size) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(part_name) {
                info!("写入特殊分区 {} (parttype={})", part_name, pt);
                (pt, addr, size)
            } else {
                // 如果无 GPT 缓存，先尝试从 gpt.bin 加载缓存，再从设备读取
                if self.last_gpt_data.is_none() {
                    if !self.try_load_cached_gpt() {
                        self.read_gpt()?;
                    }
                }
                // find_partition_addr 内部已处理 GPT 物理分区 → super 动态分区回退
                let (addr, size) = self.find_partition_addr(part_name)?;
                (8u32, addr, size) // parttype = USER
            };

        // 校验：文件大小不能超过分区大小
        if file_size > part_size {
            return Err(format!(
                "文件大小 ({}) 超过分区 {} 容量 ({} 字节)，写入被拒绝",
                file_size, part_name, part_size
            ));
        }
        if file_size == part_size {
            info!("  文件大小与分区容量完全匹配");
        } else {
            info!("  文件大小: {} 字节, 分区容量: {} 字节", file_size, part_size);
        }

        // 使用 PaddedReader 自动处理 512 字节对齐填充
        let write_size = if file_size % 512 != 0 {
            file_size + (512 - file_size % 512)
        } else {
            file_size
        };

        // 检查写入断点续传
        let start_offset = match check_write_resume(in_file, addr, write_size) {
            Some(offset) => {
                info!(
                    "  断点续传: 从 0x{:X} ({}) 继续，已写入 {}/{} 字节 ({:.1}%)",
                    offset,
                    offset,
                    offset,
                    write_size,
                    offset as f64 / write_size as f64 * 100.0
                );
                offset
            }
            None => 0,
        };

        // 打开文件并 seek 到断点位置
        use std::io::{Seek, SeekFrom};
        let file_for_seek =
            std::fs::File::open(in_file).map_err(|e| format!("无法打开文件: {}", e))?;
        let mut file_seekable = file_for_seek;
        if start_offset > 0 {
            file_seekable
                .seek(SeekFrom::Start(start_offset))
                .map_err(|e| format!("文件 seek 失败: {}", e))?;
        }

        let remaining = write_size.saturating_sub(start_offset);
        let padded_reader =
            PaddedReader::new(file_seekable, file_size.saturating_sub(start_offset), 512);
        let mut reader = std::io::BufReader::with_capacity(1024 * 1024, padded_reader);

        let write_addr = addr + start_offset;

        // 使用 BufReader 流式读取 + write_flash_data_stream
        self.write_flash_data_stream(
            write_addr,
            &mut reader,
            remaining,
            1,
            parttype,
            start_offset,
            in_file,
        )?;

        // 成功：删除续传状态文件
        remove_write_resume_file(in_file);
        info!("  写入完成: {} 字节写入分区 {}", file_size, part_name);
        Ok(())
    }

    /// 擦除分区
    /// 对齐 Python formatflash (xflash_lib.py:formatflash)
    /// 协议: FORMAT 命令 → send_param → 等待 STATUS_COMPLETE (0x40040005)
    pub fn erase_partition(&mut self, part_name: &str) -> Result<(), String> {
        info!("擦除分区 {}...", part_name);

        // 先尝试解析特殊分区（boot1/boot2/rpmb）
        let (parttype, addr, size) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(part_name) {
                info!("擦除特殊分区 {} (parttype={})", part_name, pt);
                (pt, addr, size)
            } else {
                // 如果无 GPT 缓存，自动读取 GPT 数据
                if self.last_gpt_data.is_none() {
                    self.read_gpt()?;
                }
                // 找到分区地址和大小
                let (addr, size) = self.find_partition_addr(part_name)?;
                (8u32, addr, size) // parttype = USER
            };

        // 发送 FORMAT 命令
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&CMD_FORMAT.to_le_bytes())?;

        let st = self.status()?;
        if st != 0 {
            return Err(format!("FORMAT 命令 status error: 0x{:08X}", st));
        }

        // 发送参数: storage(4) + parttype(4) + addr(8) + size(8) + NandExtension(32)
        let mut param = Vec::with_capacity(56);
        param.extend_from_slice(&1u32.to_le_bytes()); // storage = EMMC
        param.extend_from_slice(&parttype.to_le_bytes()); // parttype
        param.extend_from_slice(&addr.to_le_bytes());
        param.extend_from_slice(&size.to_le_bytes());
        // NandExtension 全零（对齐 Python xflash_flash_param.py）
        param.extend_from_slice(&[0u8; 32]);

        let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        // 等待 STATUS_COMPLETE (0x40040005) 或 STATUS_CONTINUE (0x40040004)
        // 注意：FORMAT 命令完成后，部分 DA 版本返回 0x00000000 (ACK) 而非 0x40040005
        const STATUS_COMPLETE: u32 = 0x40040005;
        const STATUS_CONTINUE: u32 = 0x40040004;

        let erase_start = std::time::Instant::now();
        let mut status = self.status()?;
        while status == STATUS_CONTINUE {
            // STATUS_CONTINUE 包含等待时间（毫秒）
            let wait_ms = self.status()?;
            // 进度提示：每 5 秒输出一次等待信息
            let elapsed = erase_start.elapsed().as_secs();
            if elapsed > 0 && elapsed % 5 == 0 {
                info!(
                    "  擦除中... 已等待 {}s (分区 {}, {:.2} MB)",
                    elapsed,
                    part_name,
                    size as f64 / 1024.0 / 1024.0
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(wait_ms as u64));
            let _ = self.ack();
            status = self.status()?;
        }

        // FORMAT 成功状态：STATUS_COMPLETE 或 0x00000000 (ACK)
        if status != STATUS_COMPLETE && status != 0x00000000 {
            return Err(format!("擦除失败: status=0x{:08X}", status));
        }

        // STATUS_COMPLETE 后必须发送 ACK 完成 FORMAT 事务，
        // 否则 DA 卡在等待 ACK 状态，下一条命令会超时
        if status == STATUS_COMPLETE {
            let _ = self.ack();
        }

        // 擦除完成后排空 USB 管道残留
        // 不用 recover_usb_pipes：clear_halt 会重置 data toggle 导致后续读取错位
        self.preloader.device.drain_pipes();

        info!("  擦除完成: 分区 {} (0x{:X} @ 0x{:X})", part_name, size, addr);
        Ok(())
    }

    /// 解锁 Bootloader
    /// 对齐 Python mtkclient: unlock 命令
    /// 流程: 读取 seccfg 分区 → 解析 V4/V3 结构 → 修改 lock_state → 重新签名 → 写回
    pub fn unlock_bootloader(&mut self) -> Result<(), String> {
        crate::security::seccfg::unlock_bootloader(self)
    }

    /// 锁定 Bootloader
    // 预留：unlock/lock 命令使用
    pub fn lock_bootloader(&mut self) -> Result<(), String> {
        crate::security::seccfg::lock_bootloader(self)
    }
}

//! Partition writing / erasing / bootloader locking

use crate::da::xflash::{CMD_FORMAT, CMD_MAGIC, DAXFlash, pack3};
use crate::partition::write::{check_write_resume, remove_write_resume_file};
use log::info;
use std::io::Read;

/// Reader wrapper that pads the end of the file with zero bytes for 512-byte alignment.
struct PaddedReader<R: Read> {
    inner: R,
    remaining: u64, // total bytes still to read (including padding)
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
            // File exhausted; emit zero padding
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
    /// Write a file to a partition (streaming, to avoid OOM on large files)
    /// Mirrors Python writeflash (xflash_lib.py:writeflash)
    /// Protocol: cmd_write_data → chunked writes `0x0(4B)``checksum(4B)``data` → CC_OPTIONAL_DOWNLOAD_ACT → status
    /// Also validates that the file size does not exceed the partition size and errors otherwise.
    /// Supports resume: after cancellation, continue from the breakpoint via the `.wresume` file.
    pub fn write_partition(&mut self, part_name: &str, in_file: &str) -> Result<(), String> {
        info!(
            "{}",
            crate::i18n::tr("log_write_start", &[in_file, part_name])
        );

        // Open the file (streaming read to avoid loading it all into memory)
        let file = std::fs::File::open(in_file).map_err(|e| format!("无法打开文件: {}", e))?;
        let file_meta = file
            .metadata()
            .map_err(|e| format!("无法获取文件元数据: {}", e))?;
        let file_size = file_meta.len();

        // Try to resolve special partitions first (boot1/boot2/rpmb)
        let (parttype, addr, part_size) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(part_name) {
                info!("写入特殊分区 {} (parttype={})", part_name, pt);
                (pt, addr, size)
            } else {
                // No GPT cache: try the gpt.bin cache first, then read from the device
                if self.last_gpt_data.is_none() {
                    if !self.try_load_cached_gpt() {
                        self.read_gpt()?;
                    }
                }
                // find_partition_addr already handles the GPT physical → super dynamic partition fallback
                let (addr, size) = self.find_partition_addr(part_name)?;
                (8u32, addr, size) // parttype = USER
            };

        // Validate: the file must not exceed the partition size
        if file_size > part_size {
            return Err(format!(
                "文件大小 ({}) 超过分区 {} 容量 ({} 字节)，写入被拒绝",
                file_size, part_name, part_size
            ));
        }
        if file_size == part_size {
            info!("  文件大小与分区容量完全匹配");
        } else {
            info!(
                "  文件大小: {} 字节, 分区容量: {} 字节",
                file_size, part_size
            );
        }

        // PaddedReader handles 512-byte alignment padding automatically
        let write_size = if file_size % 512 != 0 {
            file_size + (512 - file_size % 512)
        } else {
            file_size
        };

        // Check for a write resume point
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

        // Open the file and seek to the resume position
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

        // Stream with BufReader + write_flash_data_stream
        self.write_flash_data_stream(
            write_addr,
            &mut reader,
            remaining,
            1,
            parttype,
            start_offset,
            in_file,
        )?;

        // Success: remove the resume state file
        remove_write_resume_file(in_file);
        info!(
            "{}",
            crate::i18n::tr("log_write_done", &[&file_size.to_string(), part_name])
        );
        Ok(())
    }

    /// Erase a partition
    /// Mirrors Python formatflash (xflash_lib.py:formatflash)
    /// Protocol: FORMAT command → send_param → wait for STATUS_COMPLETE (0x40040005)
    pub fn erase_partition(&mut self, part_name: &str) -> Result<(), String> {
        info!("擦除分区 {}...", part_name);

        // Try to resolve special partitions first (boot1/boot2/rpmb)
        let (parttype, addr, size) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(part_name) {
                info!("擦除特殊分区 {} (parttype={})", part_name, pt);
                (pt, addr, size)
            } else {
                // No GPT cache: read the GPT automatically
                if self.last_gpt_data.is_none() {
                    self.read_gpt()?;
                }
                // Locate the partition address and size
                let (addr, size) = self.find_partition_addr(part_name)?;
                (8u32, addr, size) // parttype = USER
            };

        // Send the FORMAT command
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&CMD_FORMAT.to_le_bytes())?;

        let st = self.status()?;
        if st != 0 {
            return Err(format!("FORMAT 命令 status error: 0x{:08X}", st));
        }

        // Send parameters: storage(4) + parttype(4) + addr(8) + size(8) + NandExtension(32)
        let mut param = Vec::with_capacity(56);
        param.extend_from_slice(&1u32.to_le_bytes()); // storage = EMMC
        param.extend_from_slice(&parttype.to_le_bytes()); // parttype
        param.extend_from_slice(&addr.to_le_bytes());
        param.extend_from_slice(&size.to_le_bytes());
        // NandExtension is all zeros (mirrors Python xflash_flash_param.py)
        param.extend_from_slice(&[0u8; 32]);

        let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        // Wait for STATUS_COMPLETE (0x40040005) or STATUS_CONTINUE (0x40040004)
        // Note: after FORMAT some DA versions return 0x00000000 (ACK) instead of 0x40040005
        const STATUS_COMPLETE: u32 = 0x40040005;
        const STATUS_CONTINUE: u32 = 0x40040004;

        let erase_start = std::time::Instant::now();
        let mut status = self.status()?;
        while status == STATUS_CONTINUE {
            // STATUS_CONTINUE carries the wait time (milliseconds)
            let wait_ms = self.status()?;
            // Progress hint: print the wait status every 5 seconds
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

        // FORMAT success statuses: STATUS_COMPLETE or 0x00000000 (ACK)
        if status != STATUS_COMPLETE && status != 0x00000000 {
            return Err(format!("擦除失败: status=0x{:08X}", status));
        }

        // An ACK must be sent after STATUS_COMPLETE to finish the FORMAT transaction,
        // otherwise the DA keeps waiting for the ACK and the next command times out
        if status == STATUS_COMPLETE {
            let _ = self.ack();
        }

        // Drain residual data from the USB pipes after erasing
        // Not recover_usb_pipes: clear_halt resets the data toggle and desyncs later reads
        self.preloader.device.drain_pipes();

        info!(
            "  擦除完成: 分区 {} (0x{:X} @ 0x{:X})",
            part_name, size, addr
        );
        Ok(())
    }
}

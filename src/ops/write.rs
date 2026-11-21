//! `write`: write an image into a partition (with resume + read-back verify + danger confirmation).
//!
//! Flow:
//! 1. Locate the partition and check the image size (**never truncate**: a file larger than the partition is rejected);
//! 2. Critical partitions require confirmation (`--yes` passes directly; otherwise print the risk and ask y/N);
//! 3. Read `.resume`: resume only if all four fingerprints match (partition/address/total/size+mtime);
//! 4. Stream the write, persisting a checkpoint every 8 MiB (and on failure);
//! 5. Read-back verify (on by default) covering the whole written range;
//! 6. Delete `.resume` on success.

use std::cell::RefCell;
use std::rc::Rc;

use log::{info, warn};

use super::checkpoint::{
    CRC_CHUNK, ChunkCrc, Crc32, CrcTracker, PERSIST_INTERVAL, WriteCheckpoint,
};
use super::{Op, OpCtx, OpOutcome, danger_for_partition, is_critical_partition};
use crate::core::capability::{Capability, CapabilitySet};

/// Read-back verify chunk size.
const VERIFY_CHUNK: u64 = 0x10_0000;

/// Write a file into a partition.
pub struct WriteOp {
    pub part: String,
    pub file: String,
    pub verify: bool,
}

impl Op for WriteOp {
    fn name(&self) -> &'static str {
        "write"
    }

    fn required_caps(&self) -> CapabilitySet {
        CapabilitySet::EMPTY
            .with(Capability::StorageWrite)
            .with(Capability::GptParsed)
    }

    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String> {
        let meta = std::fs::metadata(&self.file)
            .map_err(|e| format!("无法读取镜像 {}: {}", self.file, e))?;
        let file_size = meta.len();
        if file_size == 0 {
            return Err(format!("镜像 {} 为空", self.file));
        }

        let span = ctx.session.find_partition(&self.part)?;
        if file_size > span.size {
            return Err(format!(
                "镜像 {} 字节 > 分区 {} 容量 {} 字节，拒绝写入（不做截断）",
                file_size, self.part, span.size
            ));
        }

        // 512-byte alignment (DA writes whole blocks)
        let total = if file_size % 512 == 0 {
            file_size
        } else {
            file_size + (512 - file_size % 512)
        };

        // Danger confirmation: critical partitions must be confirmed
        if is_critical_partition(&self.part) {
            ctx.confirm_action(
                "write",
                danger_for_partition(&self.part),
                &format!("写入重点分区 {} ({} 字节)", self.part, file_size),
            )?;
        }

        // Resume: dual check of GPT hash + chunk CRCs (Phase 1.2)
        // A changed device GPT means the layout may differ, so addresses are untrusted → abort instead of resuming.
        let gpt_crc = ctx.session.gpt().map(super::checkpoint::crc32).unwrap_or(0);
        let cp =
            WriteCheckpoint::load_checked(&self.file, &self.part, span.addr, total, Some(gpt_crc))?;
        let (start, tracker) = match cp {
            Some(c) => {
                warn!(
                    "断点续传: {} 已写 {}/{} 字节（{:.1}%），从 0x{:X} 继续",
                    self.part,
                    c.written,
                    total,
                    c.written as f64 / total as f64 * 100.0,
                    c.written
                );
                // Before writing, prove the written prefix is still intact. Any CRC mismatch aborts:
                // continuing would leave bad data in the partition while the user believes it resumed.
                self.verify_prefix(ctx, span.addr, &c.chunks)?;
                info!(
                    "续传前置校验通过: {} 个 CRC 分块（共 {} 字节）",
                    c.chunks.len(),
                    c.written
                );
                (
                    c.written,
                    CrcTracker::resume(c.chunk_size, c.chunks.clone()),
                )
            }
            None => (0, CrcTracker::new(CRC_CHUNK)),
        };
        let chunk_size = tracker.chunk_size();
        let (sig_size, sig_mtime) =
            crate::ops::checkpoint::file_signature(&self.file).unwrap_or((file_size, 0));

        let mut src = std::fs::File::open(&self.file)
            .map_err(|e| format!("打开镜像 {} 失败: {}", self.file, e))?;
        if start > 0 {
            use std::io::Seek;
            src.seek(std::io::SeekFrom::Start(start))
                .map_err(|e| format!("seek 失败: {}", e))?;
        }
        let tracker = Rc::new(RefCell::new(tracker));
        let mut padded = PaddedReader {
            inner: src,
            remaining: total - start,
            short: 0,
        };

        let file = self.file.clone();
        let part = self.part.clone();
        let mut last_persisted = start;
        let mut res =
            ctx.session
                .write_stream(span.addr, &mut padded, total, start, &mut |written| {
                    if written - last_persisted >= PERSIST_INTERVAL {
                        // Count only device-confirmed bytes, so prefetched but unwritten data is not included.
                        if let Err(e) = tracker.borrow_mut().sync_from_file(&file, written) {
                            warn!("计算分块 CRC 失败: {}", e);
                        }
                        let chunks = tracker.borrow().snapshot();
                        let c = WriteCheckpoint {
                            part: part.clone(),
                            addr: span.addr,
                            total,
                            written,
                            file_size: sig_size,
                            file_mtime: sig_mtime,
                            gpt_crc,
                            chunk_size,
                            chunks,
                        };
                        if let Err(e) = c.save(&file) {
                            warn!("保存续传状态失败: {}", e);
                        }
                        last_persisted = written;
                    }
                });

        if let Err(e) = res {
            // Persist even on failure (the process may exit immediately)
            let written = last_persisted;
            if written > 0 && written < total {
                let _ = tracker.borrow_mut().sync_from_file(&self.file, written);
                let c = WriteCheckpoint {
                    part: self.part.clone(),
                    addr: span.addr,
                    total,
                    written,
                    file_size: sig_size,
                    file_mtime: sig_mtime,
                    gpt_crc,
                    chunk_size,
                    chunks: tracker.borrow().snapshot(),
                };
                let _ = c.save(&self.file);
            }
            return Err(format!(
                "{}\n已保存断点：重跑同一条命令即可从断点继续（{}）",
                e,
                WriteCheckpoint::path_for(&self.file).display()
            ));
        }
        res = Ok(());
        res?;

        // Read-back verify covering the whole image range, including the part written before resuming.
        // Always verify on resume: `.resume` only says where the last write stopped, not that the data is still there
        // (another machine or an intervening flash can hit the checkpoint with wrong data).
        let mut verified = 0u64;
        let do_verify = self.verify || start > 0;
        if do_verify {
            // A verify failure means the existing prefix is untrusted (another machine / flashed meanwhile),
            // so the checkpoint must be invalidated or it will keep resuming from a wrong offset.
            info!("回读校验 {} 字节...", total);
            if let Err(e) = self.verify_range(ctx, span.addr, total) {
                WriteCheckpoint::clear(&self.file);
                return Err(format!("{}\n已作废本次断点，请重跑同一条命令从头写入", e));
            }
            verified = total;
            info!("校验通过: {} 字节一致", verified);
        }

        WriteCheckpoint::clear(&self.file);

        // Phase 1.3: after writing an AVB-protected partition run a forced check; warn in red if verify is on (non-blocking).
        super::avb_guard::warn_after_write(ctx.session, &self.part);

        info!(
            "写入完成: {} -> {} ({} 字节{})",
            self.file,
            self.part,
            file_size,
            if start > 0 { "，含续传" } else { "" }
        );

        Ok(OpOutcome {
            summary: format!(
                "{} -> {}（{} 字节{}）",
                self.file,
                self.part,
                file_size,
                if start > 0 { "，续传" } else { "" }
            ),
            artifacts: Vec::new(),
            facts: vec![
                ("bytes".into(), file_size.to_string()),
                ("resumed_from".into(), start.to_string()),
                ("verified".into(), verified.to_string()),
            ],
        })
    }
}

impl WriteOp {
    /// Verify the CRC of the written prefix on the device before resuming (64 MiB per chunk).
    ///
    /// Any mismatch aborts: absolute defense — better to make the user delete the checkpoint and
    /// rewrite than to keep writing past bad data as if it were good.
    fn verify_prefix(
        &self,
        ctx: &mut OpCtx<'_>,
        addr: u64,
        chunks: &[ChunkCrc],
    ) -> Result<(), String> {
        for c in chunks {
            let mut hasher = Crc32::new();
            let mut off = 0u64;
            while off < c.len {
                let take = (c.len - off).min(VERIFY_CHUNK);
                let dev = ctx.session.read_flash(addr + c.offset + off, take)?;
                if dev.len() as u64 != take {
                    return Err(format!(
                        "断点校验读回长度不符 @0x{:X}（期望 {} 实际 {}）",
                        c.offset + off,
                        take,
                        dev.len()
                    ));
                }
                hasher.update(&dev);
                off += take;
            }
            let got = hasher.finalize();
            if got != c.crc {
                return Err(format!(
                    "断点数据已损坏 @0x{:X}（块 CRC 期望 0x{:08X} 实际 0x{:08X}）。\
                     设备里的已写前缀与断点记录不符，继续写会留下坏数据；\
                     请删除 {} 后从头写入",
                    c.offset,
                    c.crc,
                    got,
                    WriteCheckpoint::path_for(&self.file).display()
                ));
            }
        }
        Ok(())
    }

    /// Read back and compare chunk by chunk (covering `[addr, addr+total)`).
    fn verify_range(&self, ctx: &mut OpCtx<'_>, addr: u64, total: u64) -> Result<(), String> {
        let mut src =
            std::fs::File::open(&self.file).map_err(|e| format!("重新打开镜像失败: {}", e))?;
        let mut offset = 0u64;
        let mut buf = vec![0u8; VERIFY_CHUNK as usize];
        while offset < total {
            let take = (total - offset).min(VERIFY_CHUNK);
            let n = read_full(&mut src, &mut buf[..take as usize])?;
            let device = ctx.session.read_flash(addr + offset, take)?;
            if device.len() as u64 != take {
                return Err(format!("校验读回长度不符 @0x{:X}", addr + offset));
            }
            let mut expect = buf[..take as usize].to_vec();
            // Zero-pad a short tail block (the write pads the same way)
            if n < take as usize {
                expect[n..].fill(0);
            }
            if device != expect {
                let pos = device
                    .iter()
                    .zip(&expect)
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                return Err(format!(
                    "校验失败 @0x{:X}（首个不一致字节在 0x{:X}）",
                    addr + offset,
                    addr + offset + pos as u64
                ));
            }
            offset += take;
        }
        Ok(())
    }
}

/// Fill the buffer (returns bytes actually read; a short tail is allowed).
fn read_full(src: &mut std::fs::File, buf: &mut [u8]) -> Result<usize, String> {
    use std::io::Read;
    let mut n = 0;
    while n < buf.len() {
        let r = src
            .read(&mut buf[n..])
            .map_err(|e| format!("读取镜像失败: {}", e))?;
        if r == 0 {
            break;
        }
        n += r;
    }
    Ok(n)
}

/// Zero-padding reader (DA requires whole 512-byte blocks).
struct PaddedReader {
    inner: std::fs::File,
    remaining: u64,
    short: usize,
}

impl std::io::Read for PaddedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        // Always fill `want` (`want` is a multiple of 512: total is aligned, buf is 1 MiB),
        // padding with zeros if the file ends early. This keeps the bytes written to the device
        // in exact one-to-one correspondence with the bytes fed to the CRC tracker, so resume
        // cannot mismatch CRC coverage due to padding differences.
        let want = (buf.len() as u64).min(self.remaining) as usize;
        let mut n = 0usize;
        while n < want {
            let r = self.inner.read(&mut buf[n..want])?;
            if r == 0 {
                break;
            }
            n += r;
        }
        if n < want {
            buf[n..want].fill(0);
            self.short += want - n;
        }
        self.remaining -= want as u64;
        Ok(want)
    }
}

//! `write`：把镜像写进分区（带断点续传 + 回读校验 + 危险确认）。
//!
//! 流程：
//! 1. 定位分区、校验镜像大小（**绝不截断**：文件大于分区直接拒绝）；
//! 2. 重点分区要求确认（`--yes` / `--token` / 交互 y/N）；
//! 3. 读 `.resume`：四项指纹（分区/地址/总长/源文件大小+mtime）全对才续传；
//! 4. 流式写入，每 8 MiB（及失败时）落一次 checkpoint；
//! 5. 回读校验（默认开），覆盖整个已写范围；
//! 6. 成功后删除 `.resume`。

use log::{info, warn};

use super::checkpoint::{PERSIST_INTERVAL, WriteCheckpoint};
use super::{Op, OpCtx, OpOutcome, danger_for_partition, is_critical_partition};
use crate::core::capability::{Capability, CapabilitySet};

/// 回读校验分块。
const VERIFY_CHUNK: u64 = 0x10_0000;

/// 写入文件到分区。
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

        // 512 字节对齐（DA 要求整块写）
        let total = if file_size % 512 == 0 {
            file_size
        } else {
            file_size + (512 - file_size % 512)
        };

        // 危险确认：重点分区必须确认
        if is_critical_partition(&self.part) {
            ctx.confirm_action(
                "write",
                danger_for_partition(&self.part),
                &format!("写入重点分区 {} ({} 字节)", self.part, file_size),
            )?;
        }

        // 续传
        let cp = WriteCheckpoint::load(&self.file, &self.part, span.addr, total);
        let start = match &cp {
            Some(c) => {
                warn!(
                    "断点续传: {} 已写 {}/{} 字节（{:.1}%），从 0x{:X} 继续",
                    self.part,
                    c.written,
                    total,
                    c.written as f64 / total as f64 * 100.0,
                    c.written
                );
                c.written
            }
            None => 0,
        };
        let (sig_size, sig_mtime) =
            crate::ops::checkpoint::file_signature(&self.file).unwrap_or((file_size, 0));

        let mut src = std::fs::File::open(&self.file)
            .map_err(|e| format!("打开镜像 {} 失败: {}", self.file, e))?;
        if start > 0 {
            use std::io::Seek;
            src.seek(std::io::SeekFrom::Start(start))
                .map_err(|e| format!("seek 失败: {}", e))?;
        }
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
                        let c = WriteCheckpoint {
                            part: part.clone(),
                            addr: span.addr,
                            total,
                            written,
                            file_size: sig_size,
                            file_mtime: sig_mtime,
                        };
                        if let Err(e) = c.save(&file) {
                            warn!("保存续传状态失败: {}", e);
                        }
                        last_persisted = written;
                    }
                });

        if let Err(e) = res {
            // 失败也要落盘（进程可能马上退出）
            let written = last_persisted;
            if written > 0 && written < total {
                let c = WriteCheckpoint {
                    part: self.part.clone(),
                    addr: span.addr,
                    total,
                    written,
                    file_size: sig_size,
                    file_mtime: sig_mtime,
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

        // 回读校验（覆盖整个镜像范围，包括续传前已写的部分）。
        // 续传一律校验：`.resume` 只证明"上次写到哪"，不证明"那部分还在设备里"
        // （换了台机器 / 期间被别人刷过，都会命中断点但数据不对）。
        let mut verified = 0u64;
        let do_verify = self.verify || start > 0;
        if do_verify {
            // 校验失败说明设备里既有的前缀不可信（换了机器 / 中途被刷过），
            // 此时必须作废断点，否则会一直从错误的偏移续传。
            info!("回读校验 {} 字节...", total);
            if let Err(e) = self.verify_range(ctx, span.addr, total) {
                WriteCheckpoint::clear(&self.file);
                return Err(format!("{}\n已作废本次断点，请重跑同一条命令从头写入", e));
            }
            verified = total;
            info!("校验通过: {} 字节一致", verified);
        }

        WriteCheckpoint::clear(&self.file);
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
    /// 逐块回读比对（覆盖 `[addr, addr+total)`）。
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
            // 文件尾部不足一块时按零填充（写入时同样补零）
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

/// 读满缓冲区（返回实际读到的字节数，允许末尾不足）。
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

/// 尾部补零的 Reader（DA 要求整 512 字节块）。
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
        let want = (buf.len() as u64).min(self.remaining) as usize;
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            // 文件已读完，用零补到对齐长度
            let fill = want;
            buf[..fill].fill(0);
            self.remaining -= fill as u64;
            self.short += fill;
            return Ok(fill);
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

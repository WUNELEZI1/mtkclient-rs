//! `read`：把分区内容读成镜像文件。
//!
//! 只声明 `StorageRead`；不关心"DA 怎么来的""要不要绕过"。
//!
//! 落盘策略：先写 `<out>.part`，全部成功后再 rename 成 `<out>`。
//! 这样中途断连不会留下半个镜像骗到后续的刷机/校验步骤。

use std::io::Write;

use log::info;

use super::{Op, OpCtx, OpOutcome};
use crate::core::capability::{Capability, CapabilitySet};

/// 分块大小（1 MiB）：与真机 DA 单次 READ_DATA 上限一致。
const CHUNK: u64 = 0x10_0000;

/// 读取分区到文件。
pub struct ReadOp {
    pub part: String,
    pub out: String,
}

impl Op for ReadOp {
    fn name(&self) -> &'static str {
        "read"
    }

    fn required_caps(&self) -> CapabilitySet {
        CapabilitySet::EMPTY
            .with(Capability::StorageRead)
            .with(Capability::GptParsed)
    }

    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String> {
        let span = ctx.session.find_partition(&self.part)?;
        if span.size == 0 {
            return Err(format!("分区 {} 大小为 0，拒绝读取", self.part));
        }
        info!(
            "读取 {} @ 0x{:X}（{} 字节）→ {}",
            self.part, span.addr, span.size, self.out
        );

        let tmp = format!("{}.part", self.out);
        let mut file =
            std::fs::File::create(&tmp).map_err(|e| format!("创建临时文件 {} 失败: {}", tmp, e))?;

        let mut done = 0u64;
        let mut next_report = 64 * 1024 * 1024;
        while done < span.size {
            let take = (span.size - done).min(CHUNK);
            let chunk = ctx.session.read_flash(span.addr + done, take)?;
            if chunk.len() as u64 != take {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!(
                    "读取长度不符: 期望 {} 实际 {}（@0x{:X}）",
                    take,
                    chunk.len(),
                    span.addr + done
                ));
            }
            file.write_all(&chunk)
                .map_err(|e| format!("写入 {} 失败: {}", tmp, e))?;
            done += take;
            if done >= next_report && done < span.size {
                info!(
                    "  已读取 {:.1} MiB / {:.1} MiB",
                    done as f64 / 1048576.0,
                    span.size as f64 / 1048576.0
                );
                next_report += 64 * 1024 * 1024;
            }
        }
        file.flush().ok();
        drop(file);
        std::fs::rename(&tmp, &self.out)
            .map_err(|e| format!("提交目标文件 {} 失败: {}", self.out, e))?;
        info!("完成: {} ({} 字节)", self.out, done);

        Ok(OpOutcome {
            summary: format!("{} -> {}（{} 字节）", self.part, self.out, done),
            artifacts: vec![self.out.clone()],
            facts: vec![
                ("bytes".into(), done.to_string()),
                ("addr".into(), format!("0x{:X}", span.addr)),
            ],
        })
    }
}

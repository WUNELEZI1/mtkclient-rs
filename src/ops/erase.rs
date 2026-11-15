//! `erase`：擦除分区（幂等）。
//!
//! 幂等性来自两处保证：
//! 1. 协议层：FORMAT 只依赖 (地址, 长度)，重复下发结果相同；
//! 2. 后端层：擦除后再擦一次不会因为"已是空分区"而报错（真机 DA 与仿真设备均如此）。
//!
//! 因此"擦到一半断线 → 重跑同一条命令"是安全的；这里额外把擦除前后的
//! 读回结果做一次抽样比对（可选），用于验证后端确实把数据清成 0xFF。

use log::{info, warn};

use super::{Op, OpCtx, OpOutcome, is_critical_partition};
use crate::confirm::Danger;
use crate::core::capability::{Capability, CapabilitySet};

/// 擦除分区。
pub struct EraseOp {
    pub part: String,
    /// 擦除后抽样确认前 4 KiB 已变为空（0xFF / 0x00）
    pub check: bool,
}

impl Op for EraseOp {
    fn name(&self) -> &'static str {
        "erase"
    }

    fn required_caps(&self) -> CapabilitySet {
        CapabilitySet::EMPTY
            .with(Capability::StorageWrite)
            .with(Capability::GptParsed)
    }

    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String> {
        let span = ctx.session.find_partition(&self.part)?;
        if span.size == 0 {
            return Err(format!("分区 {} 大小为 0，无法擦除", self.part));
        }

        // 擦除一律确认：这是不可"读回来"的操作
        let danger = if is_critical_partition(&self.part) {
            Danger::High
        } else {
            Danger::High
        };
        ctx.confirm_action(
            "erase",
            danger,
            &format!("擦除 {}（{} 字节）", self.part, span.size),
        )?;

        info!(
            "擦除 {} @ 0x{:X}（{} 字节）",
            self.part, span.addr, span.size
        );
        let bytes = ctx.session.erase_partition(&self.part)?;
        info!("擦除完成: {} ({} 字节)", self.part, bytes);

        let mut check_note = String::new();
        if self.check && span.size >= 512 {
            let head = ctx.session.read_flash(span.addr, 4096.min(span.size))?;
            let blank = head.iter().all(|b| *b == 0xFF || *b == 0x00);
            if !blank {
                warn!("擦除后抽样读回非空（前 4 KiB 仍有数据），请检查 DA/存储状态");
                check_note = "（抽样非空，建议复擦）".into();
            } else {
                check_note = "（抽样为空）".into();
            }
        }

        Ok(OpOutcome {
            summary: format!("已擦除 {}（{} 字节）{}", self.part, bytes, check_note),
            artifacts: Vec::new(),
            facts: vec![
                ("bytes".into(), bytes.to_string()),
                ("addr".into(), format!("0x{:X}", span.addr)),
            ],
        })
    }
}

//! `erase`: erase a partition (idempotent).
//!
//! Idempotency rests on two guarantees:
//! 1. Protocol layer: FORMAT depends only on (address, length), so repeating it gives the same result;
//! 2. Backend layer: erasing an already-blank partition does not error (true for both real-device DA and the emulator).
//!
//! So "connection drops mid-erase then rerun the same command" is safe; an optional
//! sampled read-back comparison around the erase verifies the backend really cleared data to 0xFF.

use log::{info, warn};

use super::{Op, OpCtx, OpOutcome, danger_for_partition};
use crate::core::capability::{Capability, CapabilitySet};

/// Erase a partition.
pub struct EraseOp {
    pub part: String,
    /// Sample the first 4 KiB after erase to confirm it is blank (0xFF / 0x00)
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

        // Erase always requires confirmation (data cannot be recovered); unrecoverable partitions need a token every time
        let danger = danger_for_partition(&self.part);
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

//! `vbmeta`: slot-aware disabling of vbmeta verification.
//!
//! Reuses the existing pure function [`decide_action`] and the three-tier `Session::active_slot` fallback:
//! - shared `vbmeta` (no `_a/_b` suffix): always processed;
//! - `vbmeta_*_a/_b`: only the active slot is processed; the other slot is `[SKIP]` with a logged reason;
//! - slot undeterminable (A/B but bootctrl read failed): **fail immediately, write no partition**.

use log::{info, warn};

use super::{Op, OpCtx, OpOutcome};
use crate::confirm::Danger;
use crate::core::capability::{Capability, CapabilitySet};
use crate::security::vbmeta::{SlotState, VBMETA_CANDIDATES, VbAction, decide_action};

pub struct VbmetaOp {
    /// flags: 3 = disable both hashtree and verification (recommended for flashing)
    pub mode: u32,
}

impl Op for VbmetaOp {
    fn name(&self) -> &'static str {
        "vbmeta"
    }

    fn required_caps(&self) -> CapabilitySet {
        CapabilitySet::EMPTY
            .with(Capability::StorageWrite)
            .with(Capability::GptParsed)
    }

    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String> {
        // Determine the slot first: abort entirely on failure (never write with an unknown slot)
        let slot = match ctx.session.active_slot() {
            Ok(Some(c)) => {
                info!("活动槽位: {}（只改该槽 + 共享 vbmeta）", c);
                SlotState::Known(c)
            }
            Ok(None) => {
                info!("未检测到 A/B 槽位（按非 A/B 设备处理）");
                SlotState::NotAb
            }
            Err(e) => {
                return Err(format!("{}；已中止，未写入任何分区", e));
            }
        };

        ctx.confirm_action(
            "vbmeta",
            Danger::High,
            &format!("修补 vbmeta（flags={}）", self.mode),
        )?;

        let mut patched = Vec::new();
        let mut skipped = Vec::new();
        let mut missing = Vec::new();

        for name in VBMETA_CANDIDATES {
            match decide_action(name, slot) {
                VbAction::Skip(reason) => {
                    // Report only when the partition actually exists, to avoid a wall of "missing" lines
                    if ctx.session.find_partition(name).is_ok() {
                        info!("  [SKIP] {}（{}）", name, reason);
                        skipped.push(name);
                    }
                    continue;
                }
                VbAction::Patch => {}
            }

            let span = match ctx.session.find_partition(name) {
                Ok(s) => s,
                Err(_) => {
                    missing.push(name);
                    continue;
                }
            };
            info!("  {} @ 0x{:X}（{} 字节）", name, span.addr, span.size);
            let data = ctx.session.read_flash(span.addr, span.size)?;
            let vb = match crate::security::avb::VbmetaImage::parse(&data) {
                Ok(v) => v,
                Err(e) => {
                    warn!("  {} 跳过（非标准 vbmeta: {}）", name, e);
                    continue;
                }
            };
            for d in &vb.descriptors {
                if let Some(pn) = &d.partition_name {
                    info!("    descriptor: {} ({})", d.tag_debug_name(), pn);
                }
            }
            info!(
                "  当前 flags=0x{:02X} -> 目标 0x{:02X}",
                vb.header.flags, self.mode
            );
            let modified = vb.disable_verification(self.mode);
            ctx.session.write_flash(span.addr, &modified)?;
            info!("  {} 已修补", name);
            patched.push(name);
        }

        if patched.is_empty() {
            return Err(format!(
                "未找到可修补的 vbmeta 分区（跳过: {}，缺失: {}）",
                skipped.join(","),
                missing.join(",")
            ));
        }

        Ok(OpOutcome {
            summary: format!(
                "vbmeta 已修补 {} 个（跳过 {} 个非活动槽）",
                patched.len(),
                skipped.len()
            ),
            artifacts: Vec::new(),
            facts: vec![
                ("patched".into(), patched.join(",")),
                ("skipped".into(), skipped.join(",")),
                ("flags".into(), self.mode.to_string()),
            ],
        })
    }
}

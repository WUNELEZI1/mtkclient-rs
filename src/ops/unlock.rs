//! `unlock`：一键解锁链（seccfg 解锁 + vbmeta 校验禁用）。
//!
//! 为什么合成一个 op 而不是串两个 op：
//! - 能力声明只有一份（`StorageRead | StorageWrite`），规划器只决策一次；
//! - "解锁后必须处理 vbmeta" 是**业务不变式**，放在同一个 `run` 里由类型保证；
//! - 日志上仍能清楚看到两段，且第二段失败不会掩盖第一段的成功。
//!
//! 顺序很重要：先写 seccfg（解除锁），再改 vbmeta（关校验）。
//! 反过来的话，一旦第二步失败，设备会处于"已关校验但仍上锁"的中间态。

use log::{info, warn};

use super::seccfg::{SeccfgAction, SeccfgOp};
use super::vbmeta::VbmetaOp;
use super::{Op, OpCtx, OpOutcome};
use crate::core::capability::{Capability, CapabilitySet};

/// 解锁时写入的 vbmeta flags（hashtree + verification 全关）。
pub const UNLOCK_VBMETA_FLAGS: u32 = 3;

pub struct UnlockOp;

impl Op for UnlockOp {
    fn name(&self) -> &'static str {
        "unlock"
    }

    fn required_caps(&self) -> CapabilitySet {
        CapabilitySet::EMPTY
            .with(Capability::StorageRead)
            .with(Capability::StorageWrite)
            .with(Capability::GptParsed)
    }

    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String> {
        info!("=== 步骤 1/2: seccfg 解锁 ===");
        let seccfg_out = SeccfgOp {
            action: SeccfgAction::Unlock,
        }
        .run(ctx)?;

        info!("=== 步骤 2/2: 禁用 vbmeta 校验（防 dm-verity 起不来）===");
        let vbmeta = match (VbmetaOp {
            mode: UNLOCK_VBMETA_FLAGS,
        })
        .run(ctx)
        {
            Ok(o) => o,
            Err(e) => {
                // 解锁已生效，vbmeta 失败属于"需要用户跟进"的情况，不当成整体失败
                warn!(
                    "vbmeta 自动处理失败: {}（可用 `zyb vbmeta {}` 重试）",
                    e, UNLOCK_VBMETA_FLAGS
                );
                return Ok(OpOutcome {
                    summary: format!("Bootloader 已解锁；vbmeta 未处理（{}）", e),
                    artifacts: Vec::new(),
                    facts: vec![
                        ("lock_state".into(), "3".into()),
                        ("vbmeta".into(), format!("failed: {}", e)),
                    ],
                });
            }
        };

        Ok(OpOutcome {
            summary: format!("{}；{}", seccfg_out.summary, vbmeta.summary),
            artifacts: Vec::new(),
            facts: vec![
                ("lock_state".into(), "3".into()),
                ("dm_verity_state".into(), "0".into()),
                ("vbmeta".into(), vbmeta.summary.clone()),
            ],
        })
    }
}

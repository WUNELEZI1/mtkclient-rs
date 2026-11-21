//! `unlock`: one-shot unlock chain (seccfg unlock + vbmeta verification disable).
//!
//! Why a single op instead of chaining two ops:
//! - Only one capability declaration (`StorageRead | StorageWrite`), so the planner decides once;
//! - "vbmeta must be handled after unlock" is a **business invariant** enforced by types inside one `run`;
//! - Logs still show both stages, and a stage-2 failure does not mask stage-1 success.
//!
//! Order matters: write seccfg (clear lock) first, then modify vbmeta (disable verification).
//! Reversed, a failed second step would leave the device "verification off but still locked".

use log::{info, warn};

use super::seccfg::{SeccfgAction, SeccfgOp};
use super::vbmeta::VbmetaOp;
use super::{Op, OpCtx, OpOutcome};
use crate::core::capability::{Capability, CapabilitySet};

/// vbmeta flags written on unlock (hashtree + verification fully disabled).
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
                // Unlock already took effect; a vbmeta failure needs user follow-up, not overall failure
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

        // Read back the real lock state read-only (SeccfgOp judges both V3/V4), replacing hardcoded "3".
        // A read failure does not mask a successful unlock; it only falls back to the default.
        let lock_state = SeccfgOp::current_lock_state(ctx)
            .map(|v| v.to_string())
            .unwrap_or_else(|e| {
                warn!("回读锁状态失败（不影响解锁结果）: {}", e);
                "3".into()
            });
        Ok(OpOutcome {
            summary: format!("{}；{}", seccfg_out.summary, vbmeta.summary),
            artifacts: Vec::new(),
            facts: vec![
                ("lock_state".into(), lock_state),
                ("dm_verity_state".into(), "0".into()),
                ("vbmeta".into(), vbmeta.summary.clone()),
            ],
        })
    }
}

//! `seccfg`: bootloader unlock / lock (capability-driven path).
//!
//! Shares the same structure parsing and signing code as `zyb seccfg unlock`; only the data
//! channel differs: this path goes through [`crate::session::Session`], so real hardware and simulation run the same code.
//!
//! Key safety points (pinned by tests):
//! - Offset 0x0C = `lock_state` (1=locked, 3=unlocked);
//! - Offset 0x10 = `dm_verity_state`, **written as 0 for both unlock and lock**.
//!   Upstream mtkclient keeps the original value by default and writes 1 on unlock with `critical=True`
//!   (= DM_VERITY_GENERAL_ERROR), which is exactly why a forced BL unlock makes boot report a dm-verity error.
//!   We always write 0: behavior differs from upstream, but one command can never leave the device unbootable.

use log::{info, warn};

use super::{Op, OpCtx, OpOutcome};
use crate::confirm::Danger;
use crate::core::capability::{Capability, CapabilitySet};
use crate::security::seccfg::v3::SecCfgV3;
use crate::security::seccfg::v4::SecCfgV4;

/// Unlock / lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeccfgAction {
    Unlock,
    Lock,
}

impl SeccfgAction {
    fn flag(self) -> &'static str {
        match self {
            SeccfgAction::Unlock => "unlock",
            SeccfgAction::Lock => "lock",
        }
    }
}

pub struct SeccfgOp {
    pub action: SeccfgAction,
}

impl SeccfgOp {
    /// Read the current lock state without modifying it (used by the unlock chain to decide whether vbmeta must change).
    /// Supports V3 (attr mapped to 1/3) and V4 (native lock_state value).
    pub fn current_lock_state(ctx: &mut OpCtx<'_>) -> Result<u32, String> {
        let span = ctx.session.find_partition("seccfg")?;
        let data = ctx.session.read_flash(span.addr, span.size.min(0x4000))?;
        if Self::is_v3(&data) {
            let v3 = SecCfgV3::parse(&data)?;
            Ok(if v3.seccfg_attr == SecCfgV3::ATTR_UNLOCK {
                3
            } else {
                1
            })
        } else {
            let v4 = SecCfgV4::parse(&data)?;
            Ok(v4.lock_state)
        }
    }

    /// A V3 header starts with `AND_SECCFG_v\0\0\0`; the first 4 bytes of a V4 header are magic 0x4D4D4D4D.
    fn is_v3(data: &[u8]) -> bool {
        data.len() >= 16 && &data[..12] == b"AND_SECCFG_v"
    }

    /// V3 unlock/lock: parse → idempotence check → software SEJ re-sign → write back → read-back attr verification.
    /// V3 SEJ encryption is pure software AES (no device HACC), so real hardware and simulation share this implementation.
    fn run_v3(&self, ctx: &mut OpCtx<'_>, data: &[u8]) -> Result<OpOutcome, String> {
        let span = ctx.session.find_partition("seccfg")?;
        let v3 = SecCfgV3::parse(data)?;
        let target_attr = match self.action {
            SeccfgAction::Unlock => SecCfgV3::ATTR_UNLOCK,
            SeccfgAction::Lock => SecCfgV3::ATTR_DEFAULT,
        };
        if v3.seccfg_attr == target_attr {
            return Ok(OpOutcome {
                summary: format!("seccfg V3 已是 {}，无需修改", self.action.flag()),
                artifacts: Vec::new(),
                facts: vec![("changed".into(), "0".into())],
            });
        }
        ctx.confirm_action(
            if self.action == SeccfgAction::Unlock {
                "seccfg unlock"
            } else {
                "seccfg lock"
            },
            Danger::High,
            &format!(
                "V3 seccfg {}（attr 0x{:08X} -> 0x{:08X}）",
                self.action.flag(),
                v3.seccfg_attr,
                target_attr
            ),
        )?;

        let image = v3.create(self.action.flag(), span.size as usize)?;
        ctx.session.write_flash(span.addr, &image)?;

        let back = ctx.session.read_flash(span.addr, span.size.min(0x4000))?;
        let v3b = SecCfgV3::parse(&back)?;
        if v3b.seccfg_attr != target_attr {
            return Err(format!(
                "回读校验失败: V3 attr 期望 0x{:08X} 实际 0x{:08X}",
                target_attr, v3b.seccfg_attr
            ));
        }
        info!(
            "seccfg V3 {} 完成: attr=0x{:08X}, hwtype={}",
            self.action.flag(),
            v3b.seccfg_attr,
            v3b.hwtype
        );
        Ok(OpOutcome {
            summary: format!(
                "seccfg V3 {} 成功（attr=0x{:08X}, hwtype={}）",
                self.action.flag(),
                v3b.seccfg_attr,
                v3b.hwtype
            ),
            artifacts: Vec::new(),
            facts: vec![
                ("seccfg_version".into(), "V3".into()),
                ("attr".into(), format!("0x{:08X}", v3b.seccfg_attr)),
                ("addr".into(), format!("0x{:X}", span.addr)),
            ],
        })
    }
}

impl Op for SeccfgOp {
    fn name(&self) -> &'static str {
        "seccfg"
    }

    fn required_caps(&self) -> CapabilitySet {
        CapabilitySet::EMPTY
            .with(Capability::StorageRead)
            .with(Capability::StorageWrite)
            .with(Capability::GptParsed)
    }

    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String> {
        let span = ctx.session.find_partition("seccfg")?;
        let data = ctx.session.read_flash(span.addr, span.size.min(0x4000))?;
        info!(
            "读取 seccfg: addr=0x{:X} size={}（读入 {} 字节）",
            span.addr,
            span.size,
            data.len()
        );

        // All versions supported: V3 (AND_SECCFG_v header, software SEJ) and V4 (0x4D4D4D4D header).
        if Self::is_v3(&data) {
            return self.run_v3(ctx, &data);
        }
        if data.len() < 28 || u32::from_le_bytes(data[0..4].try_into().unwrap()) != SecCfgV4::MAGIC
        {
            return Err("seccfg 结构无法识别（既非 V3 也非 V4）".into());
        }
        let v4 = SecCfgV4::parse(&data)?;
        info!(
            "当前状态: lock_state={} dm_verity_state={} hwtype={}",
            v4.lock_state, v4.dm_verity_state, v4.hwtype
        );

        let target = match self.action {
            SeccfgAction::Unlock => 3u32,
            SeccfgAction::Lock => 1u32,
        };
        if v4.lock_state == target {
            return Ok(OpOutcome {
                summary: format!("seccfg 已是 {}，无需修改", self.action.flag()),
                artifacts: Vec::new(),
                facts: vec![("changed".into(), "0".into())],
            });
        }

        ctx.confirm_action(
            if self.action == SeccfgAction::Unlock {
                "seccfg unlock"
            } else {
                "seccfg lock"
            },
            Danger::High,
            &format!(
                "{}（lock_state {} -> {}）",
                self.action.flag(),
                v4.lock_state,
                target
            ),
        )?;

        // Build the new header + signature (real devices use HACC/SEJ; the simulation returns the digest as-is)
        let (header, new_lock, new_dm, _) =
            crate::security::seccfg::build::build_v4_header(&v4, self.action.flag())?;
        let digest = crate::sha::Sha256::digest(header);
        let enc = ctx.session.seccfg_sign(&digest, &v4.hwtype)?;
        let image =
            crate::security::seccfg::build::pad_seccfg_image(&header, &enc, span.size as usize);

        ctx.session.write_flash(span.addr, &image)?;

        // Read-back verification: offsets 0x0C / 0x10 must equal what we wrote
        let back = ctx.session.read_flash(span.addr, 0x40.min(span.size))?;
        if back.len() < 20 {
            return Err("回读 seccfg 失败（长度不足）".into());
        }
        let got_lock = u32::from_le_bytes(back[0x0C..0x10].try_into().unwrap());
        let got_dm = u32::from_le_bytes(back[0x10..0x14].try_into().unwrap());
        if got_lock != new_lock {
            return Err(format!(
                "回读校验失败: lock_state 期望 {} 实际 {}",
                new_lock, got_lock
            ));
        }
        if got_dm != 0 {
            return Err(format!(
                "回读校验失败: dm_verity_state 期望 0 实际 {}（会触发 dm-verity error）",
                got_dm
            ));
        }
        if new_dm != 0 {
            warn!("内部错误: dm_verity_state 目标值不是 0");
        }

        info!(
            "seccfg {} 完成: lock_state={} dm_verity_state=0",
            self.action.flag(),
            new_lock
        );
        Ok(OpOutcome {
            summary: format!(
                "seccfg {} 成功（lock_state={}, dm_verity_state=0）",
                self.action.flag(),
                new_lock
            ),
            artifacts: Vec::new(),
            facts: vec![
                ("lock_state".into(), new_lock.to_string()),
                ("dm_verity_state".into(), "0".into()),
                ("addr".into(), format!("0x{:X}", span.addr)),
            ],
        })
    }
}

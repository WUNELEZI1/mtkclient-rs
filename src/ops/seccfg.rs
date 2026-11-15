//! `seccfg`：Bootloader 解锁 / 上锁（能力驱动路径）。
//!
//! 与既有 `zyb seccfg unlock` 共用同一套结构解析与签名实现，区别只在
//! 数据通道：这里走 [`Session`]，因此真机与仿真跑的是同一段代码。
//!
//! 关键安全点（有测试钉死）：
//! - 偏移 0x0C = `lock_state`（1=locked，3=unlocked）；
//! - 偏移 0x10 = `dm_verity_state`，**unlock/lock 都写 0**。
//!   上游 mtkclient 默认保留原值、`critical=True` 时 unlock 写 1
//!   （= DM_VERITY_GENERAL_ERROR），这正是"强解 BL 后开机会报 dm-verity error"的成因。
//!   我们选择强制写 0：代价是行为与上游不同，收益是不会因一条命令让设备起不来。

use log::{info, warn};

use super::{Op, OpCtx, OpOutcome};
use crate::confirm::Danger;
use crate::core::capability::{Capability, CapabilitySet};
use crate::security::seccfg::v4::SecCfgV4;

/// 解锁 / 上锁。
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
    /// 只读地检查当前锁状态（供 unlock 链判断是否需要动 vbmeta）。
    pub fn current_lock_state(ctx: &mut OpCtx<'_>) -> Result<u32, String> {
        let span = ctx.session.find_partition("seccfg")?;
        let data = ctx.session.read_flash(span.addr, span.size.min(0x4000))?;
        let v4 = SecCfgV4::parse(&data)?;
        Ok(v4.lock_state)
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

        if data.len() < 28 || u32::from_le_bytes(data[0..4].try_into().unwrap()) != SecCfgV4::MAGIC
        {
            return Err(
                "seccfg 不是 V4 结构（V3/SW 需要 SEJ 加密，请使用 legacy 命令 zyb seccfg）".into(),
            );
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

        // 构造新头 + 签名（真机走 HACC/SEJ；仿真返回明文摘要）
        let (header, new_lock, new_dm, _) =
            crate::security::seccfg::build::build_v4_header(&v4, self.action.flag())?;
        let digest = crate::sha::Sha256::digest(&header);
        let enc = ctx.session.seccfg_sign(&digest, &v4.hwtype)?;
        let image =
            crate::security::seccfg::build::pad_seccfg_image(&header, &enc, span.size as usize);

        ctx.session.write_flash(span.addr, &image)?;

        // 回读校验：偏移 0x0C / 0x10 必须是我们写的值
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

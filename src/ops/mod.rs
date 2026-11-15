//! 业务操作（ops）：以"目标能力"声明，而不是以"执行步骤"声明。
//!
//! 每个 op 只回答两件事：
//! 1. 我需要哪些能力（[`Op::required_caps`]）；
//! 2. 能力齐了之后做什么（[`Op::run`]）。
//!
//! 中间的"怎么获得这些能力"由 [`crate::exec::Executor`] 交给
//! Planner / Orchestrator 决定，因此 CLI 不再需要硬编码流程。
//!
//! 注意：新增 op 不需要改 Executor —— 只要它声明能力并实现 `run`。

pub mod checkpoint;
pub mod erase;
pub mod fsshell;
pub mod printgpt;
pub mod read;
pub mod seccfg;
pub mod unlock;
pub mod vbmeta;
pub mod write;

use crate::confirm::{ConfirmContext, ConfirmError, Danger, DangerConfirm};
use crate::core::capability::{Capability, CapabilitySet};
use crate::session::Session;

/// 执行上下文。
pub struct OpCtx<'a> {
    pub session: &'a mut dyn Session,
    /// 是否有权写入产物文件（scatter.txt / gpt_debug_*.bin）。
    /// 单测里置 false，避免污染仓库。
    pub write_artifacts: bool,
    pub log_level: u8,
    /// 危险操作确认器（`--yes` / `--token` / 交互式终端）
    pub confirm: &'a mut DangerConfirm,
    /// 命令行提供的确认 token（设备 SN 后 4 位或版本串后 4 位）
    pub token: Option<String>,
}

impl OpCtx<'_> {
    /// 危险操作确认。
    ///
    /// 通过时把警告打到 stderr（`--yes` 也不例外，保证留痕），
    /// 未通过时返回可操作的错误信息（提示 `--yes` / `--token`）。
    pub fn confirm_action(
        &mut self,
        action: &'static str,
        danger: Danger,
        detail: &str,
    ) -> Result<(), String> {
        let serial = self
            .session
            .serial()
            .unwrap_or_else(|| "UNKNOWN".to_string());
        let ctx = ConfirmContext {
            serial,
            fingerprint: detail.to_string(),
            timestamp: crate::util::now_iso8601(),
        };
        match self
            .confirm
            .require(action, danger, &ctx, self.token.as_deref())
        {
            Ok(Some(w)) => {
                eprintln!("{}", w);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(ConfirmError::NeedToken) => Err(format!(
                "{} 需要确认：请加 --yes 跳过交互，或 --token {} （设备 SN 后 4 位）",
                action,
                ctx.token()
            )),
            Err(e) => Err(format!("{} 未确认: {}", action, e)),
        }
    }
}

/// 重点分区（擦/写需要确认）。
///
/// 判定原则：写坏之后**设备无法自行恢复**或会丢失回滚/校准数据的分区。
/// 普通数据分区（如 super 内的子分区、vendor 等）不在列表内。
pub fn is_critical_partition(name: &str) -> bool {
    let bare = name.trim_end_matches(|c| c == 'a' || c == 'b');
    let bare = bare.trim_end_matches('_');
    const EXACT: [&str; 24] = [
        "preloader",
        "preloader_backup",
        "seccfg",
        "nvram",
        "nvdata",
        "nvcfg",
        "persist",
        "protect1",
        "protect2",
        "proinfo",
        "frp",
        "misc",
        "para",
        "expdb",
        "metadata",
        "userdata",
        "super",
        "boot",
        "init_boot",
        "vendor_boot",
        "recovery",
        "lk",
        "tee",
        "dtbo",
    ];
    if EXACT.contains(&bare) {
        return true;
    }
    name.starts_with("vbmeta")
        || name.starts_with("tee")
        || name.starts_with("lk")
        || name.starts_with("md1img")
        || name.starts_with("scp")
        || name.starts_with("sspm")
        || name.starts_with("spmfw")
        || name.starts_with("cam_vpu")
        || name.starts_with("gz")
}

/// 执行结果。
#[derive(Debug, Clone, Default)]
pub struct OpOutcome {
    /// 一句话结果（进日志）
    pub summary: String,
    /// 产出的文件路径
    pub artifacts: Vec<String>,
    /// 关键数量（如分区数），便于测试断言
    pub facts: Vec<(String, String)>,
}

/// 一个业务操作。
pub trait Op {
    /// 目标名（进决策日志的 `goal` 字段）
    fn name(&self) -> &'static str;
    /// 目标能力
    fn required_caps(&self) -> CapabilitySet;
    /// 执行
    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String>;
}

/// op 名 → 目标能力（CLI 的 `plan` 与 `exec` 共用同一份定义，避免漂移）。
pub fn required_caps(op: &str) -> Option<CapabilitySet> {
    let caps: &[Capability] = match op {
        // printgpt 只读：不需要写能力
        "printgpt" => &[Capability::StorageRead, Capability::GptParsed],
        // 按分区名读写必须先有 GPT（否则分区名无法解析成地址）
        "read" => &[Capability::StorageRead, Capability::GptParsed],
        "write" | "erase" | "vbmeta" => &[Capability::StorageWrite, Capability::GptParsed],
        "seccfg" => &[
            Capability::StorageRead,
            Capability::StorageWrite,
            Capability::GptParsed,
        ],
        "unlock" => &[Capability::StorageRead, Capability::StorageWrite],
        "fs_shell" => &[Capability::StorageRead, Capability::GptParsed],
        "rpmb" => &[Capability::RpmbAccess],
        _ => return None,
    };
    let mut set = CapabilitySet::EMPTY;
    for c in caps {
        set.insert(*c);
    }
    Some(set)
}

/// op 名 → `'static` 名称（决策日志需要）。
pub fn op_static_name(op: &str) -> &'static str {
    match op {
        "printgpt" => "printgpt",
        "read" => "read",
        "write" => "write",
        "erase" => "erase",
        "seccfg" => "seccfg",
        "unlock" => "unlock",
        "vbmeta" => "vbmeta",
        "fs_shell" => "fs_shell",
        "rpmb" => "rpmb",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printgpt_is_read_only() {
        let r = required_caps("printgpt").unwrap();
        assert!(r.contains(Capability::StorageRead));
        assert!(r.contains(Capability::GptParsed));
        assert!(
            !r.contains(Capability::StorageWrite),
            "printgpt 不应要求写能力"
        );
    }

    #[test]
    fn unknown_op_has_no_caps() {
        assert!(required_caps("nope").is_none());
        assert_eq!(op_static_name("nope"), "unknown");
    }
}

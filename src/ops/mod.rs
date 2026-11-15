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

pub mod printgpt;

use crate::core::capability::{Capability, CapabilitySet};
use crate::session::Session;

/// 执行上下文。
pub struct OpCtx<'a> {
    pub session: &'a mut dyn Session,
    /// 是否有权写入产物文件（scatter.txt / gpt_debug_*.bin）。
    /// 单测里置 false，避免污染仓库。
    pub write_artifacts: bool,
    pub log_level: u8,
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
        "read" => &[Capability::StorageRead],
        "write" | "erase" => &[Capability::StorageWrite],
        "seccfg" => &[Capability::StorageRead, Capability::StorageWrite],
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

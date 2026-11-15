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
    /// 危险操作确认器（`--yes` / 交互式终端）
    pub confirm: &'a mut DangerConfirm,
}

impl OpCtx<'_> {
    /// 危险操作确认。
    ///
    /// - 不加 `--yes`：确认器先打印风险提示，再交互式问 `y/N`；
    ///   非交互环境直接报错并提示加 `--yes`。
    /// - 加 `--yes`：确认器静默放行（`--yes` 是给脚本/程序调用的，不打印警告）。
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
        match self.confirm.require(action, danger, &ctx) {
            Ok(()) => Ok(()),
            Err(ConfirmError::NeedConfirmation) => Err(format!(
                "{} 需要确认（{}操作）：请在交互式终端回答 y，或加 --yes 跳过交互",
                action,
                danger.label()
            )),
            Err(ConfirmError::Declined) => Err(format!("{} 未确认: 操作已被取消", action)),
        }
    }
}

/// 去掉 A/B 槽后缀（`boot_a` → `boot`）。
///
/// 只剥 `_a`/`_b` 或紧邻的单字母槽后缀；**不能无脑 `trim_end_matches('a')`**——
/// `userdata` / `nvdata` / `para` 的词尾 a 是名字的一部分，剥掉就再也匹配不到重点分区。
fn strip_slot(name: &str) -> &str {
    if let Some(s) = name.strip_suffix("_a").or_else(|| name.strip_suffix("_b")) {
        return s;
    }
    if let Some(s) = name.strip_suffix('a').or_else(|| name.strip_suffix('b')) {
        return s;
    }
    name
}

/// 重点分区（擦/写需要确认）。
///
/// 判定原则：写坏之后**设备无法自行恢复**或会丢失回滚/校准数据的分区。
/// 普通数据分区（如 super 内的子分区、vendor 等）不在列表内。
pub fn is_critical_partition(name: &str) -> bool {
    const EXACT: [&str; 25] = [
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
        "data",
        "super",
        "boot",
        "init_boot",
        "vendor_boot",
        "recovery",
        "lk",
        "tee",
        "dtbo",
    ];
    // 原名与"去掉槽后缀"两种写法都算，避免 `boot_a` 漏判、`userdata` 误剥
    [name, strip_slot(name)].iter().any(|n| {
        EXACT.contains(n)
            || n.starts_with("vbmeta")
            || n.starts_with("tee")
            || n.starts_with("lk")
            || n.starts_with("md1img")
            || n.starts_with("scp")
            || n.starts_with("sspm")
            || n.starts_with("spmfw")
            || n.starts_with("cam_vpu")
            || n.starts_with("gz")
    })
}

/// 不可恢复分区：写坏/擦掉之后设备**无法自行恢复**（变砖，或丢校准/回滚数据）。
///
/// 这批分区要求每次操作都重新确认（`Danger::Irreversible`），不接受"本次会话已确认过"。
pub fn is_irreversible_partition(name: &str) -> bool {
    const SET: [&str; 11] = [
        "preloader",
        "preloader_backup",
        "seccfg",
        "nvram",
        "nvdata",
        "nvcfg",
        "persist",
        "proinfo",
        "protect1",
        "protect2",
        "frp",
    ];
    [name, strip_slot(name)]
        .iter()
        .any(|n| SET.contains(n) || n.starts_with("preloader"))
}

/// 该分区写/擦的危险等级。
pub fn danger_for_partition(name: &str) -> Danger {
    if is_irreversible_partition(name) {
        Danger::Irreversible
    } else {
        Danger::High
    }
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
        "unlock" => &[
            Capability::StorageRead,
            Capability::StorageWrite,
            Capability::GptParsed,
        ],
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

    /// 词尾恰好是 a/b 的分区名不能被当成槽后缀剥掉（曾经 `userdata` → `userdat` 漏判）。
    #[test]
    fn critical_partition_names_with_trailing_a_or_b() {
        for name in ["userdata", "nvdata", "para", "data", "nvdata_bak"] {
            let expect = name != "nvdata_bak"; // bak 不在列表内，仅确保不 panic
            assert_eq!(
                is_critical_partition(name),
                expect,
                "{} 的重点分区判定不对",
                name
            );
        }
        // 带槽后缀的照样命中
        assert!(is_critical_partition("boot_a"));
        assert!(is_critical_partition("boot_b"));
        assert!(is_critical_partition("vbmeta_system_b"));
        assert!(is_critical_partition("lk"));
        // 普通分区不打断
        assert!(!is_critical_partition("vendor"));
        assert!(!is_critical_partition("system"));
    }

    #[test]
    fn irreversible_set_is_a_subset_of_critical() {
        for name in [
            "preloader",
            "preloader_backup",
            "seccfg",
            "nvram",
            "nvdata",
            "nvcfg",
            "persist",
            "proinfo",
            "protect1",
            "protect2",
            "frp",
        ] {
            assert!(is_critical_partition(name), "{} 应是重点分区", name);
            assert!(is_irreversible_partition(name), "{} 应不可恢复", name);
            assert_eq!(danger_for_partition(name), Danger::Irreversible);
        }
        // boot 是重点但可恢复（刷回去就行）
        assert!(is_critical_partition("boot_a"));
        assert!(!is_irreversible_partition("boot_a"));
        assert_eq!(danger_for_partition("boot_a"), Danger::High);
    }

    #[test]
    fn unknown_op_has_no_caps() {
        assert!(required_caps("nope").is_none());
        assert_eq!(op_static_name("nope"), "unknown");
    }

    /// `required_caps(op)` 表与各 `Op::required_caps()` 必须逐位一致。
    ///
    /// 两处定义一旦漂移，`plan`（读表）和 `exec`（读 trait）就会对同一个操作给出
    /// 不同的能力缺口，规划出的步骤也会跟着错。
    #[test]
    fn capability_table_matches_each_op() {
        let ops: Vec<(&str, CapabilitySet)> = vec![
            ("printgpt", printgpt::PrintGptOp.required_caps()),
            (
                "read",
                read::ReadOp {
                    part: "boot_a".into(),
                    out: "x".into(),
                }
                .required_caps(),
            ),
            (
                "write",
                write::WriteOp {
                    part: "boot_a".into(),
                    file: "x".into(),
                    verify: false,
                }
                .required_caps(),
            ),
            (
                "erase",
                erase::EraseOp {
                    part: "boot_a".into(),
                    check: true,
                }
                .required_caps(),
            ),
            (
                "seccfg",
                seccfg::SeccfgOp {
                    action: seccfg::SeccfgAction::Unlock,
                }
                .required_caps(),
            ),
            ("vbmeta", vbmeta::VbmetaOp { mode: 3 }.required_caps()),
            ("unlock", unlock::UnlockOp.required_caps()),
            (
                "fs_shell",
                fsshell::FsShellOp {
                    target: fsshell::FsTarget::Boot,
                    file: None,
                    action: fsshell::FsAction::Info,
                }
                .required_caps(),
            ),
        ];
        for (name, want) in ops {
            let got = required_caps(name).unwrap_or_else(|| panic!("{} 未登记能力", name));
            assert_eq!(got, want, "{} 的能力表与 Op 实现不一致", name);
            assert_eq!(op_static_name(name), name);
        }
    }
}

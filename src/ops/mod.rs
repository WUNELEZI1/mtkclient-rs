//! Business operations (ops): declared by target capabilities, not by execution steps.
//!
//! Each op answers only two things:
//! 1. Which capabilities it needs ([`Op::required_caps`]);
//! 2. What to do once they are available ([`Op::run`]).
//!
//! How to obtain those capabilities is delegated by [`crate::exec::Executor`] to the
//! Planner / Orchestrator, so the CLI no longer hardcodes any flow.
//!
//! Note: adding an op needs no Executor change — it only declares capabilities and implements `run`.

pub mod avb_guard;
pub mod checkpoint;
pub mod erase;
pub mod format;
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

/// Execution context.
pub struct OpCtx<'a> {
    pub session: &'a mut dyn Session,
    /// Whether artifact files may be written (scatter.txt / gpt_debug_*.bin).
    /// Set to false in unit tests to avoid polluting the repo.
    pub write_artifacts: bool,
    pub log_level: u8,
    /// Dangerous-operation confirmer (`--yes` / interactive terminal)
    pub confirm: &'a mut DangerConfirm,
}

impl OpCtx<'_> {
    /// Confirm a dangerous operation.
    ///
    /// - Without `--yes`: prints the risk warning first, then asks `y/N` interactively;
    ///   in a non-interactive environment it errors out and suggests adding `--yes`.
    /// - With `--yes`: passes silently (`--yes` targets scripts/programs, so no warning is printed).
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

/// Strip the A/B slot suffix (`boot_a` → `boot`).
///
/// Only strips `_a`/`_b` or a directly adjacent single-letter suffix; **never blindly `trim_end_matches('a')`** —
/// the trailing a in `userdata` / `nvdata` / `para` is part of the name; stripping it breaks critical matches.
fn strip_slot(name: &str) -> &str {
    if let Some(s) = name.strip_suffix("_a").or_else(|| name.strip_suffix("_b")) {
        return s;
    }
    if let Some(s) = name.strip_suffix('a').or_else(|| name.strip_suffix('b')) {
        return s;
    }
    name
}

/// Critical partitions (erase/write requires confirmation).
///
/// Rule: partitions that **cannot be recovered by the device** after a bad write, or that hold
/// rollback/calibration data. Normal data partitions (super sub-partitions, vendor, etc.) are excluded.
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
    // Match both the raw name and the slot-stripped name, so `boot_a` is not missed and `userdata` is not mangled
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

/// Irreversible partitions: after a bad write/erase the device **cannot recover** (brick, or lost calibration/rollback data).
///
/// These require a fresh confirmation on every operation (`Danger::Irreversible`); a session-level confirmation does not count.
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

/// Danger level for writing/erasing this partition.
pub fn danger_for_partition(name: &str) -> Danger {
    if is_irreversible_partition(name) {
        Danger::Irreversible
    } else {
        Danger::High
    }
}

/// Execution result.
#[derive(Debug, Clone, Default)]
pub struct OpOutcome {
    /// One-line result (goes to the log)
    pub summary: String,
    /// Paths of produced files
    pub artifacts: Vec<String>,
    /// Key facts (e.g. partition count) for test assertions
    pub facts: Vec<(String, String)>,
}

/// A business operation.
pub trait Op {
    /// Goal name (goes to the `goal` field of the decision log)
    fn name(&self) -> &'static str;
    /// Required capabilities
    fn required_caps(&self) -> CapabilitySet;
    /// Execute
    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String>;
}

/// op name → required capabilities (shared by CLI `plan` and `exec` to avoid drift).
pub fn required_caps(op: &str) -> Option<CapabilitySet> {
    let caps: &[Capability] = match op {
        // printgpt is read-only: no write capability needed
        "printgpt" => &[Capability::StorageRead, Capability::GptParsed],
        // Name-based read/write requires the GPT first (otherwise names cannot resolve to addresses)
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

/// op name → `'static` name (needed by the decision log).
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

    /// Partition names that merely end in a/b must not be treated as slot suffixes (e.g. `userdata` → `userdat`).
    #[test]
    fn critical_partition_names_with_trailing_a_or_b() {
        for name in ["userdata", "nvdata", "para", "data", "nvdata_bak"] {
            let expect = name != "nvdata_bak"; // bak is not listed; this only ensures no panic
            assert_eq!(
                is_critical_partition(name),
                expect,
                "{} 的重点分区判定不对",
                name
            );
        }
        // Slot-suffixed names still match
        assert!(is_critical_partition("boot_a"));
        assert!(is_critical_partition("boot_b"));
        assert!(is_critical_partition("vbmeta_system_b"));
        assert!(is_critical_partition("lk"));
        // Normal partitions must not trip the check
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
        // boot is critical but recoverable (just flash it back)
        assert!(is_critical_partition("boot_a"));
        assert!(!is_irreversible_partition("boot_a"));
        assert_eq!(danger_for_partition("boot_a"), Danger::High);
    }

    #[test]
    fn unknown_op_has_no_caps() {
        assert!(required_caps("nope").is_none());
        assert_eq!(op_static_name("nope"), "unknown");
    }

    /// The `required_caps(op)` table must match each `Op::required_caps()` bit for bit.
    ///
    /// If the two definitions drift, `plan` (table-driven) and `exec` (trait-driven) report
    /// different capability gaps for the same operation, and the planned steps go wrong.
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

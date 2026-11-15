//! 执行器：**唯一执行入口**。
//!
//! 职责边界：
//! - 决定做什么 → 交给 [`Orchestrator`]（Planner 的搜索 + 决策日志）；
//! - 把 `PlanStep` 翻译成 [`Session`] 调用；
//! - 每执行一步，**校验能力确实增加了**（防止"步骤跑了但没生效"）；
//! - 收尾核对 `required ⊆ acquired`，不满足即报错，而不是继续往下跑。
//!
//! 这样"能力驱动"是**可验证**的：日志里既能看到规划结论，也能看到每一步带来的能力增量。

use log::{info, trace};

use crate::confirm::DangerConfirm;
use crate::core::capability::{Capability, CapabilitySet};
use crate::core::device::ProtocolVersion;
use crate::core::plan::{Mode, PlanStep};
use crate::exploit::orchestrator::{Decision, DecisionLog, Orchestrator};
use crate::exploit::planner::{PlanError, Planner};
use crate::exploit::registry::ExploitRegistry;
use crate::ops::{Op, OpCtx, OpOutcome};
use crate::session::Session;

/// 执行错误。
#[derive(Debug)]
pub enum ExecError {
    /// 规划失败（无可行路径）
    Plan(PlanError),
    /// 某一步执行失败
    Step { step: String, msg: String },
    /// 步骤执行完但能力没有增加（后端未真正生效）
    NoProgress {
        step: String,
        missing: Vec<&'static str>,
    },
    /// 全部步骤跑完仍未满足目标能力
    Incomplete { missing: Vec<&'static str> },
    /// op 自身失败
    Op(String),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Plan(e) => write!(f, "规划失败: {}", e),
            ExecError::Step { step, msg } => write!(f, "步骤 {} 失败: {}", step, msg),
            ExecError::NoProgress { step, missing } => {
                write!(f, "步骤 {} 未产生预期能力: {}", step, missing.join(","))
            }
            ExecError::Incomplete { missing } => {
                write!(f, "执行结束仍缺能力: {}", missing.join(","))
            }
            ExecError::Op(m) => write!(f, "操作失败: {}", m),
        }
    }
}

impl std::error::Error for ExecError {}

/// 执行报告。
#[derive(Debug)]
pub struct ExecReport {
    pub decision: DecisionLog,
    /// 实际执行的步骤（label）
    pub executed: Vec<String>,
    /// 每步带来的新增能力
    pub gains: Vec<(String, Vec<&'static str>)>,
    /// op 的执行结果（summary / 产物 / 关键数量），供调用方展示与测试断言
    #[allow(dead_code)]
    pub outcome: OpOutcome,
}

/// 执行器。
pub struct Executor {
    orch: Orchestrator,
    /// 危险操作确认器（`--yes` / 交互式终端）
    confirm: DangerConfirm,
}

impl Executor {
    pub fn new(mode: Mode) -> Self {
        Self {
            orch: Orchestrator::new(Planner::new(ExploitRegistry::builtin(), mode)),
            confirm: DangerConfirm::new(false),
        }
    }

    /// `--yes`：跳过交互与警告，直接放行（给脚本/程序调用）。
    pub fn with_assume_yes(mut self, yes: bool) -> Self {
        self.confirm = DangerConfirm::new(yes);
        self
    }

    /// 允许交互式确认（stdin 为终端时由 CLI 打开）。
    pub fn with_interactive(mut self, on: bool) -> Self {
        self.confirm.set_interactive(on);
        self
    }

    /// 打印决策（人类可读 + 单行 JSON），返回日志对象。
    fn announce(
        &self,
        goal: &'static str,
        session: &mut dyn Session,
        required: CapabilitySet,
        log_level: u8,
    ) -> Result<Decision, ExecError> {
        let state = session.state();
        let current = session.caps();
        let decision = self
            .orch
            .decide(
                goal,
                required,
                current,
                &state,
                session.da(),
                session.da_source(),
            )
            .map_err(|e| {
                println!(
                    "{}",
                    Orchestrator::log_failure(
                        goal,
                        required,
                        &e,
                        &state,
                        self.orch.planner().mode().label(),
                        session.da_source()
                    )
                );
                ExecError::Plan(e)
            })?;

        log::debug!("[exec] 执行后端: {}", session.label());
        // 人类可读（含 gap / selected / alternatives / reason）
        print!("{}", decision.log.human());
        // 结构化决策日志：--log 2 起输出（默认只保留人类可读的 selected/reason，
        // 避免正常刷机输出里夹一整行 JSON）。`plan` 命令仍始终输出 JSON。
        if log_level >= 2 {
            println!("{}", decision.log.json());
        }
        if let Some(alt) = decision.alternatives.first() {
            trace!(
                "[exec] 次优路径 (cost={}): {}",
                alt.cost.total(),
                alt.labels().join(" -> ")
            );
        }
        Ok(decision)
    }

    /// 执行一个 op。
    pub fn execute(
        &mut self,
        op: &dyn Op,
        session: &mut dyn Session,
        write_artifacts: bool,
        log_level: u8,
    ) -> Result<ExecReport, ExecError> {
        let required = op.required_caps();
        let decision = self.announce(op.name(), session, required, log_level)?;
        let log = decision.log.clone();
        // 决策即执行计划：不重新规划，避免"日志说的"与"实际跑的"不一致
        let steps = decision.selected.steps.clone();

        let mut acquired = session.caps();
        let mut executed = Vec::new();
        let mut gains = Vec::new();

        for (i, step) in steps.iter().enumerate() {
            let label = step.label();
            info!("[exec] step {}/{}: {}", i + 1, steps.len(), label);
            self.run_step(step, session)?;
            executed.push(label.clone());

            let effect = self.effect_of(step);
            let actual = session.caps();
            let missing: Vec<&'static str> = Capability::ALL
                .iter()
                .filter(|c| effect.contains(**c) && !actual.contains(**c))
                .map(|c| c.label())
                .collect();
            if !missing.is_empty() {
                return Err(ExecError::NoProgress {
                    step: label,
                    missing,
                });
            }
            let before = acquired;
            acquired = acquired.union(effect);
            let gained: Vec<&'static str> = Capability::ALL
                .iter()
                .filter(|c| acquired.contains(**c) && !before.contains(**c))
                .map(|c| c.label())
                .collect();
            trace!("[exec] 能力增量: [{}]", gained.join(","));
            gains.push((label, gained));
        }

        let missing: Vec<&'static str> = Capability::ALL
            .iter()
            .filter(|c| required.contains(**c) && !acquired.contains(**c))
            .map(|c| c.label())
            .collect();
        if !missing.is_empty() {
            return Err(ExecError::Incomplete { missing });
        }
        trace!(
            "[exec] 能力齐备: [{}]",
            Capability::ALL
                .iter()
                .filter(|c| acquired.contains(**c))
                .map(|c| c.label())
                .collect::<Vec<_>>()
                .join(",")
        );

        let mut ctx = OpCtx {
            session,
            write_artifacts,
            log_level,
            confirm: &mut self.confirm,
        };
        let outcome = op.run(&mut ctx).map_err(ExecError::Op)?;
        info!("[exec] 完成: {}", outcome.summary);
        if !outcome.facts.is_empty() {
            log::debug!(
                "[exec] facts: {}",
                outcome
                    .facts
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }

        Ok(ExecReport {
            decision: log,
            executed,
            gains,
            outcome,
        })
    }

    /// `PlanStep` → 具体后端调用。
    fn run_step(&self, step: &PlanStep, session: &mut dyn Session) -> Result<(), ExecError> {
        let label = step.label();
        let res = match step {
            PlanStep::Exploit(name) => session.apply_exploit(name),
            PlanStep::DaLoad(_) => session.load_da(),
            PlanStep::ParamAdjust("open_storage") => session.open_storage(),
            PlanStep::ParamAdjust("parse_gpt") => session.parse_gpt(),
            other => Err(format!("执行器尚未支持该步骤: {}", other.label())),
        };
        res.map_err(|msg| ExecError::Step { step: label, msg })
    }

    /// 每一步"应得"的能力。
    fn effect_of(&self, step: &PlanStep) -> CapabilitySet {
        match step {
            PlanStep::Exploit(name) => self
                .orch
                .planner()
                .registry()
                .get(name)
                .map(|m| m.produces)
                .unwrap_or(CapabilitySet::EMPTY),
            PlanStep::DaLoad(proto) => {
                let mut s = CapabilitySet::EMPTY.with(Capability::DaLoaded);
                s.insert(match proto {
                    ProtocolVersion::V6 => Capability::DaXml,
                    _ => Capability::DaXFlash,
                });
                s
            }
            PlanStep::ParamAdjust("open_storage") => CapabilitySet::EMPTY
                .with(Capability::StorageRead)
                .with(Capability::StorageWrite),
            PlanStep::ParamAdjust("parse_gpt") => CapabilitySet::EMPTY.with(Capability::GptParsed),
            _ => CapabilitySet::EMPTY,
        }
    }
}

#[cfg(test)]
#[path = "ops_flow.rs"]
mod ops_flow;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::printgpt::PrintGptOp;
    use crate::session::SimSession;
    use crate::transport::vdev::VirtualDevice;

    fn sim(preset: &str) -> SimSession {
        let dev = VirtualDevice::from_preset(preset).expect("预设存在");
        SimSession::new(dev).with_da(vec![0xABu8; 4096])
    }

    #[test]
    fn printgpt_closes_the_loop_on_clean_device() {
        let mut s = sim("v5-clean");
        let r = Executor::new(Mode::Auto)
            .execute(&PrintGptOp, &mut s, false, 0)
            .expect("闭环应成功");
        assert_eq!(r.decision.goal, "printgpt");
        assert_eq!(
            r.decision.selected_steps,
            vec![
                "da_load(V5(XFlash))",
                "param_adjust(open_storage)",
                "param_adjust(parse_gpt)"
            ]
        );
        // 决策成本与手算一致：3 步 * 100 + 1 次 DA * 200 + 1900ms/10
        assert_eq!(r.decision.selected_cost, 690);
        assert!(!r.decision.alternatives.is_empty(), "至少给出两条路");
        // 每步都真实提升了能力
        let gained: Vec<&str> = r.gains.iter().flat_map(|(_, g)| g.clone()).collect();
        assert!(gained.contains(&"da_loaded"));
        assert!(gained.contains(&"storage_read"));
        assert!(gained.contains(&"gpt_parsed"));
        assert_eq!(
            r.outcome
                .facts
                .iter()
                .find(|(k, _)| k == "partitions")
                .map(|(_, v)| v.as_str()),
            // 默认布局：seccfg/misc/para + boot_a/boot_b/recovery/vbmeta_a/vbmeta_b/
            // super/userdata/metadata = 11
            Some("11")
        );
    }

    #[test]
    fn sla_device_forces_exploit_first() {
        let mut s = sim("v5-sla");
        let r = Executor::new(Mode::Auto)
            .execute(&PrintGptOp, &mut s, false, 0)
            .unwrap();
        assert_eq!(r.executed[0], "exploit(kamakiri2)");
        assert!(r.gains[0].1.contains(&"safety_bypassed"));
        assert!(r.decision.selected_cost > 690, "绕过必然更贵");
    }

    #[test]
    fn v6_never_uses_brom_exploit() {
        let mut s = sim("v6");
        let r = Executor::new(Mode::Auto)
            .execute(&PrintGptOp, &mut s, false, 0)
            .unwrap();
        assert!(
            !r.executed.iter().any(|x| x.contains("exploit")),
            "V6 不应出现 BROM exploit: {:?}",
            r.executed
        );
        assert!(r.executed[0].contains("da_load(V6"));
    }

    #[test]
    fn ufs_profile_reports_ufs() {
        let mut s = sim("ufs-ab");
        s.load_da().unwrap();
        let info = s.device_info().unwrap();
        assert_eq!(info.emmc_type, "UFS");
        assert_eq!(info.block, 4096);
    }

    #[test]
    fn transport_fault_surfaces_as_step_error() {
        let mut s = sim("flaky");
        let err = Executor::new(Mode::Auto)
            .execute(&PrintGptOp, &mut s, false, 0)
            .unwrap_err();
        match err {
            ExecError::Step { step, msg } => {
                assert!(step.contains("da_load"), "实际: {}", step);
                // flaky 画像用 `fault=silent` 注入；静默 = 掉线，按"断开"上报
                // （不可重试、立刻失败），不能再当作可重试的瞬时超时。
                assert!(
                    msg.contains("断开") || msg.contains("超时"),
                    "实际: {}",
                    msg
                );
            }
            other => panic!("应为步骤错误: {:?}", other),
        }
    }

    #[test]
    fn missing_emmc_info_does_not_block_printgpt() {
        // 老 DA / 定制 DA 不支持 0x00040001 时，容量字段降级为 0，但读表仍须成功
        let mut s = sim("no-emmc-info");
        let r = Executor::new(Mode::Auto)
            .execute(&PrintGptOp, &mut s, false, 0)
            .unwrap();
        assert_eq!(r.executed.len(), 3);
    }

    #[test]
    fn plan_failure_is_reported_without_panic() {
        // 没有 DA 字节 → 无法规划出 StorageRead
        let dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let mut s = SimSession::new(dev);
        let err = Executor::new(Mode::Auto)
            .execute(&PrintGptOp, &mut s, false, 0)
            .unwrap_err();
        assert!(matches!(err, ExecError::Plan(_)), "实际: {:?}", err);
    }
}

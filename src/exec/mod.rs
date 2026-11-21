//! Executor: the **single execution entry point**.
//!
//! Responsibility boundaries:
//! - decide what to do → delegated to [`Orchestrator`] (Planner search + decision logging);
//! - translate `PlanStep` into [`crate::session::Session`] calls;
//! - after each step, **verify the capability actually increased** (guarding against "step ran but had no effect");
//! - at the end check `required ⊆ acquired`, error out if unsatisfied instead of continuing.
//!
//! This makes "capability-driven" **verifiable**: logs show both the planning conclusion and each step's capability gain.

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

/// Execution error.
#[derive(Debug)]
pub enum ExecError {
    /// Planning failed (no feasible path)
    Plan(PlanError),
    /// A step failed to execute
    Step { step: String, msg: String },
    /// Step finished but the capability did not increase (backend did not take effect)
    NoProgress {
        step: String,
        missing: Vec<&'static str>,
    },
    /// All steps finished but the target capability is still unsatisfied
    Incomplete { missing: Vec<&'static str> },
    /// The op itself failed
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

/// Execution report.
#[derive(Debug)]
pub struct ExecReport {
    pub decision: DecisionLog,
    /// Steps actually executed (label)
    pub executed: Vec<String>,
    /// Capabilities newly gained by each step
    pub gains: Vec<(String, Vec<&'static str>)>,
    /// Op execution result (summary / artifacts / key counts) for caller display and test assertions
    #[allow(dead_code)]
    pub outcome: OpOutcome,
}

/// Executor.
pub struct Executor {
    orch: Orchestrator,
    /// Danger-operation confirmer (`--yes` / interactive terminal)
    confirm: DangerConfirm,
}

impl Executor {
    pub fn new(mode: Mode) -> Self {
        Self {
            orch: Orchestrator::new(Planner::new(ExploitRegistry::builtin(), mode)),
            confirm: DangerConfirm::new(false),
        }
    }

    /// `--yes`: skip interaction and warnings, pass through directly (for script/programmatic calls).
    pub fn with_assume_yes(mut self, yes: bool) -> Self {
        self.confirm = DangerConfirm::new(yes);
        self
    }

    /// Allow interactive confirmation (enabled by the CLI when stdin is a terminal).
    pub fn with_interactive(mut self, on: bool) -> Self {
        self.confirm.set_interactive(on);
        self
    }

    /// Print the decision (human-readable + single-line JSON) and return the log object.
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
        // Human-readable (includes gap / selected / alternatives / reason)
        print!("{}", decision.log.human());
        // Structured decision log: emitted from --log 2 (by default only the human-readable selected/reason is kept,
        // to avoid a whole JSON line mixed into normal flashing output). The `plan` command still always emits JSON.
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

    /// Execute an op.
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
        // The decision is the execution plan: do not re-plan, avoiding a mismatch between "what the log says" and "what actually runs"
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

    /// `PlanStep` → concrete backend call.
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

    /// Capabilities "expected" from each step.
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
        // Decision cost matches manual calculation: 3 steps * 100 + 1 DA * 200 + 1900ms/10
        assert_eq!(r.decision.selected_cost, 690);
        assert!(!r.decision.alternatives.is_empty(), "至少给出两条路");
        // Every step genuinely increased capabilities
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
            // Default layout: seccfg/misc/para + boot_a/boot_b/recovery/vbmeta_a/vbmeta_b/
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
                // The flaky profile injects via `fault=silent`; silent = disconnected, reported as "disconnected"
                // (non-retryable, fails immediately), and must not be treated as a retryable transient timeout.
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
        // When an old/custom DA does not support 0x00040001, capacity fields degrade to 0, but reading the table must still succeed
        let mut s = sim("no-emmc-info");
        let r = Executor::new(Mode::Auto)
            .execute(&PrintGptOp, &mut s, false, 0)
            .unwrap();
        assert_eq!(r.executed.len(), 3);
    }

    #[test]
    fn plan_failure_is_reported_without_panic() {
        // No DA bytes → cannot plan StorageRead
        let dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let mut s = SimSession::new(dev);
        let err = Executor::new(Mode::Auto)
            .execute(&PrintGptOp, &mut s, false, 0)
            .unwrap_err();
        assert!(matches!(err, ExecError::Plan(_)), "实际: {:?}", err);
    }
}

//! 规划模型：步骤、成本、计划。
//!
//! ## Cost 定义（验收项 27）
//!
//! 加权是**字典序式**的手工权重，越小越好：
//!
//! | 项 | 权重 | 理由 |
//! |---|---|---|
//! | 每个步骤 | 100 | 步骤越多，失败面越大 |
//! | 每次模式切换（重插/重启） | 300 | 需要用户物理操作，体验成本最高 |
//! | 每次 DA 加载 | 200 | 耗时数百 ms 到数秒，且可能失败 |
//! | 预计耗时 | `est_ms / 10` | 同一层级内做微调 |
//!
//! 因此“步骤数”优先于“是否重启”，再优先于“是否加载 DA”，最后才是耗时。
//! 平局时用 `preferred_caps` 打破（见 Planner）。

use super::capability::CapabilitySet;

/// exploit 所处阶段。定义在 core，避免 `core` 反向依赖 `exploit`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// BootROM 层（linecode / BROM 内存读写）
    Brom,
    /// Preloader 层（2nd DA bypass、镜像签名伪造）
    Preloader,
    /// DA 层（Carbonara / Heapbait 等 DA patch）
    Da,
}

impl Stage {
    pub const fn label(self) -> &'static str {
        match self {
            Stage::Brom => "brom",
            Stage::Preloader => "preloader",
            Stage::Da => "da",
        }
    }
}

/// 工作模式：约束 Planner 搜索空间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 只走 BROM（不搜 Preloader exploit）
    Brom,
    /// 只走 Preloader / DA 加载（不搜 BROM exploit）
    Preloader,
    /// 全搜，BROM 优先（步骤少）
    Auto,
}

impl Mode {
    pub const fn label(self) -> &'static str {
        match self {
            Mode::Brom => "brom",
            Mode::Preloader => "preloader",
            Mode::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "brom" => Some(Mode::Brom),
            "preloader" | "pl" => Some(Mode::Preloader),
            "auto" => Some(Mode::Auto),
            _ => None,
        }
    }

    /// 该模式是否允许某类 exploit 阶段。
    pub const fn allows(self, stage: Stage) -> bool {
        match self {
            Mode::Brom => matches!(stage, Stage::Brom),
            Mode::Preloader => matches!(stage, Stage::Preloader),
            Mode::Auto => true,
        }
    }
}

/// 计划中的一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanStep {
    /// 切换工作模式（需要重插 / 重启）
    ModeSwitch(Mode),
    /// 施加一个 exploit（名字取自注册表）
    Exploit(&'static str),
    /// 加载 DA
    DaLoad(super::device::ProtocolVersion),
    /// patch DA（Carbonara / Heapbait）
    DaPatch(&'static str),
    /// 重试（例如握手失败后第 N 次）
    Retry(u8),
    /// 参数调整（例如降低 DA 波特率）
    ParamAdjust(&'static str),
}

impl PlanStep {
    pub fn label(&self) -> String {
        match self {
            PlanStep::ModeSwitch(m) => format!("mode_switch({})", m.label()),
            PlanStep::Exploit(n) => format!("exploit({})", n),
            PlanStep::DaLoad(p) => format!("da_load({})", p.label()),
            PlanStep::DaPatch(n) => format!("da_patch({})", n),
            PlanStep::Retry(n) => format!("retry({})", n),
            PlanStep::ParamAdjust(n) => format!("param_adjust({})", n),
        }
    }
}

/// 成本（见模块注释的权重表）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cost {
    pub steps: u32,
    pub mode_switches: u32,
    pub da_loads: u32,
    pub est_ms: u32,
}

impl Cost {
    pub const STEP_W: u32 = 100;
    pub const MODE_SWITCH_W: u32 = 300;
    pub const DA_LOAD_W: u32 = 200;

    pub fn total(&self) -> u32 {
        self.steps * Self::STEP_W
            + self.mode_switches * Self::MODE_SWITCH_W
            + self.da_loads * Self::DA_LOAD_W
            + self.est_ms / 10
    }

    /// 累加一步。
    pub fn add_step(&mut self, step: &PlanStep) {
        self.steps += 1;
        match step {
            PlanStep::ModeSwitch(_) => {
                self.mode_switches += 1;
                self.est_ms += 3000;
            }
            PlanStep::DaLoad(_) => {
                self.da_loads += 1;
                self.est_ms += 1500;
            }
            PlanStep::DaPatch(_) => self.est_ms += 800,
            PlanStep::Exploit(_) => self.est_ms += 500,
            PlanStep::Retry(_) => self.est_ms += 1000,
            PlanStep::ParamAdjust(_) => self.est_ms += 200,
        }
    }
}

/// 一条完整计划。
#[derive(Debug, Clone)]
pub struct Plan {
    pub steps: Vec<PlanStep>,
    pub cost: Cost,
    /// 执行完可获得的全部能力（含偏序展开前的直接位）
    pub acquired: CapabilitySet,
    /// 为什么选它（人类可读，进决策日志）
    pub reason: String,
}

impl Plan {
    pub fn labels(&self) -> Vec<String> {
        self.steps.iter().map(|s| s.label()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::device::ProtocolVersion;

    #[test]
    fn cost_weights_are_ordered() {
        let mut a = Cost::default();
        a.add_step(&PlanStep::Exploit("x"));
        let mut b = Cost::default();
        b.add_step(&PlanStep::ModeSwitch(Mode::Brom));
        assert!(a.total() < b.total(), "步骤优先于模式切换");

        let mut c = Cost::default();
        c.add_step(&PlanStep::DaLoad(ProtocolVersion::V5));
        assert!(c.total() > a.total());
    }

    #[test]
    fn more_steps_costs_more() {
        let mut a = Cost::default();
        a.add_step(&PlanStep::Exploit("x"));
        let mut b = a;
        b.add_step(&PlanStep::Exploit("y"));
        assert!(b.total() > a.total());
    }

    #[test]
    fn mode_parsing() {
        assert_eq!(Mode::parse("BROM"), Some(Mode::Brom));
        assert_eq!(Mode::parse("preloader"), Some(Mode::Preloader));
        assert_eq!(Mode::parse("nope"), None);
    }
}

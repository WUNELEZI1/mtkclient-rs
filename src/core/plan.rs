//! Planning model: steps, cost, plan.
//!
//! ## Cost definition (acceptance item 27)
//!
//! Weights are manual and **lexicographic**; smaller is better:
//!
//! | Item | Weight | Rationale |
//! |---|---|---|
//! | Per step | 100 | More steps mean more failure surface |
//! | Per mode switch (replug/reboot) | 300 | Requires physical user action, highest UX cost |
//! | Per DA load | 200 | Takes hundreds of ms to seconds and may fail |
//! | Estimated time | `est_ms / 10` | Fine-tuning within the same tier |
//!
//! So "step count" outranks "reboot", which outranks "DA load", with time last.
//! Ties are broken by `preferred_caps` (see Planner).

use super::capability::CapabilitySet;

/// Exploit stage. Defined in core to avoid `core` depending on `exploit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// BootROM layer (linecode / BROM memory access)
    Brom,
    /// Preloader layer (2nd DA bypass, image signature forgery)
    Preloader,
    /// DA layer (Carbonara / Heapbait and other DA patches)
    Da,
}

/// Work mode: constrains the Planner search space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// BROM only (no Preloader exploit search)
    Brom,
    /// Preloader / DA load only (no BROM exploit search)
    Preloader,
    /// Search everything, BROM first (fewer steps)
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

    /// Whether this mode allows a given exploit stage.
    pub const fn allows(self, stage: Stage) -> bool {
        match self {
            Mode::Brom => matches!(stage, Stage::Brom),
            Mode::Preloader => matches!(stage, Stage::Preloader),
            Mode::Auto => true,
        }
    }
}

/// One step in a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanStep {
    /// Switch work mode (requires replug / reboot); currently only used by cost weight tests
    #[cfg(test)]
    ModeSwitch(Mode),
    /// Apply one exploit (name from the registry)
    Exploit(&'static str),
    /// Load DA
    DaLoad(super::device::ProtocolVersion),
    /// Parameter tweak (e.g. lower DA baud rate)
    ParamAdjust(&'static str),
}

impl PlanStep {
    pub fn label(&self) -> String {
        match self {
            #[cfg(test)]
            PlanStep::ModeSwitch(m) => format!("mode_switch({})", m.label()),
            PlanStep::Exploit(n) => format!("exploit({})", n),
            PlanStep::DaLoad(p) => format!("da_load({})", p.label()),
            PlanStep::ParamAdjust(n) => format!("param_adjust({})", n),
        }
    }
}

/// Cost (see the weight table in the module docs).
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

    /// Accumulate one step.
    pub fn add_step(&mut self, step: &PlanStep) {
        self.steps += 1;
        match step {
            #[cfg(test)]
            PlanStep::ModeSwitch(_) => {
                self.mode_switches += 1;
                self.est_ms += 3000;
            }
            PlanStep::DaLoad(_) => {
                self.da_loads += 1;
                self.est_ms += 1500;
            }
            PlanStep::Exploit(_) => self.est_ms += 500,
            PlanStep::ParamAdjust(_) => self.est_ms += 200,
        }
    }
}

/// A complete plan.
#[derive(Debug, Clone)]
pub struct Plan {
    pub steps: Vec<PlanStep>,
    pub cost: Cost,
    /// All capabilities gained after execution (including direct bits before partial-order expansion)
    pub acquired: CapabilitySet,
    /// Why this plan was chosen (human-readable, goes into the decision log)
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

//! 核心抽象层：能力格、安全标志、设备状态、规划模型。
//!
//! 本层**零 IO**：不碰 USB / 文件 / 日志，只做纯数据与纯函数，
//! 这样 Planner 和注册表可以在 mock / 单测里完全脱机验证。
//!
//! 依赖方向：`core` 不依赖任何其它内部模块（可被 transport / da / exploit / ops 引用）。

pub mod capability;
pub mod device;
pub mod plan;
pub mod safety;

#[allow(unused_imports)]
pub use capability::{Capability, CapabilitySet};
#[allow(unused_imports)]
pub use device::{DeviceState, KNOWN_V6_DA_CODES, ProtocolVersion};
#[allow(unused_imports)]
pub use plan::{Cost, Mode, Plan, PlanStep, Stage};
#[allow(unused_imports)]
pub use safety::SafetyFlags;

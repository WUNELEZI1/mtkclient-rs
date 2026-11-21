//! Core abstraction layer: capability lattice, safety flags, device state, plan model.
//!
//! This layer is **zero-IO**: it touches no USB / files / logging and holds only pure
//! data and pure functions, so the Planner and registry can be fully validated offline
//! in mocks / unit tests.
//!
//! Dependency direction: `core` depends on no other internal module (it may be used by transport / da / exploit / ops).

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

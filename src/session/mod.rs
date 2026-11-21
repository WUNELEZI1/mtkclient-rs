//! Execution session: binds "planned steps" to a concrete backend.
//!
//! Two implementations:
//! - [`RealSession`] — real device, reusing the existing `DAXFlash` pipeline (USB / serial);
//! - [`SimSession`]  — offline simulation, `da::engine` state machine + `transport::vdev` virtual device.
//!
//! Both implement the same [`crate::session::Session`] trait, so the `ops` and `exec` layers are written **once**.
//! The capability-driven path is therefore identical on real hardware and in simulation: a step
//! sequence proven in simulation is the one the real device follows (protocol gaps are absorbed per backend).

pub mod real;

mod sim;

pub use real::RealSession;
pub use sim::SimSession;

use crate::core::capability::CapabilitySet;
use crate::core::device::DeviceState;

/// Device summary (used for table headers / logs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub hw_code: u16,
    pub da_code: u16,
    pub protocol: &'static str,
    pub boot1: u64,
    pub boot2: u64,
    pub user: u64,
    pub block: u32,
    pub emmc_type: String,
    /// CID register (non-empty when the eMMC query succeeded); used for auditing and part-swap checks.
    pub cid: Vec<u8>,
    /// Current active slot (A/B), filled by GPT slot probing.
    pub slot: Option<char>,
}

/// Partition location result (the capability-driven path uses byte addresses only, hiding GPT details).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionSpan {
    pub addr: u64,
    pub size: u64,
    /// 8 = user area (GPT partition), 0/1 = boot1/boot2
    pub parttype: u32,
}

/// Minimal session capabilities required for one execution.
///
/// Methods deliberately mirror [`crate::core::plan::PlanStep`] one-to-one; the Executor only translates.
pub trait Session {
    /// Backend name (for logs)
    fn label(&self) -> &'static str;
    /// Planning input: device state
    fn state(&self) -> DeviceState;
    /// Capabilities currently available (must reflect real backend state; the Executor validates them)
    fn caps(&self) -> CapabilitySet;
    /// DA bytes usable for planning / upload
    fn da(&self) -> Option<&[u8]>;
    /// DA source description (recorded in the decision log)
    fn da_source(&self) -> &'static str;

    /// Run one exploit
    fn apply_exploit(&mut self, name: &'static str) -> Result<(), String>;
    /// Load the DA (handshake / upload / jump / SYNC)
    fn load_da(&mut self) -> Result<(), String>;
    /// Open storage (query capacity + SET_STORAGE)
    fn open_storage(&mut self) -> Result<(), String>;
    /// Read and parse the GPT (results are cached)
    fn parse_gpt(&mut self) -> Result<(), String>;

    /// Raw GPT bytes already read
    fn gpt(&self) -> Option<&[u8]>;
    /// Device information (for printing)
    fn device_info(&mut self) -> Result<DeviceInfo, String>;

    /// super dynamic partition metadata (Some only when readable; None on simulated devices)
    fn super_metadata(&mut self) -> Option<crate::partition::lp::SuperMetadata> {
        None
    }

    // ── Partition IO (read / write / erase) ─────────────
    //
    // These are the only data channel of the ops layer: real devices go through DAXFlash, simulation
    // through da::engine, so the same `ops::write` code behaves identically on both sides.

    /// Locate a partition (including the special boot1/boot2 partitions).
    fn find_partition(&mut self, name: &str) -> Result<PartitionSpan, String>;

    /// Read by address (a single call may be large; the backend chunks it).
    fn read_flash(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String>;

    /// Write an in-memory buffer to an address (seccfg / vbmeta / small images).
    fn write_flash(&mut self, addr: u64, data: &[u8]) -> Result<(), String>;

    /// Streaming write: copy `total` bytes from `src` to `addr + start`,
    /// invoking `on_progress(total_written)` after each chunk (the persistence point for resume).
    ///
    /// `total` is the **total length to write** (counted from offset 0, including already-written parts).
    fn write_stream(
        &mut self,
        addr: u64,
        src: &mut dyn std::io::Read,
        total: u64,
        start: u64,
        on_progress: &mut dyn FnMut(u64),
    ) -> Result<(), String>;

    /// Erase by partition name (the backend resolves the address). Returns the number of bytes erased.
    fn erase_partition(&mut self, name: &str) -> Result<u64, String>;

    /// Active A/B slot.
    ///
    /// - `Ok(Some(c))`: the confirmed active slot;
    /// - `Ok(None)`: not an A/B device;
    /// - `Err(_)`: **undeterminable** (A/B but bootctrl unreadable) — callers must fail, never guess.
    fn active_slot(&mut self) -> Result<Option<char>, String>;

    /// Sign the seccfg header digest with the device-side key (the 32-byte hash tail of V4).
    fn seccfg_sign(&mut self, digest: &[u8; 32], hwtype: &str) -> Result<Vec<u8>, String>;

    /// Device serial number (used as the confirmation token for dangerous operations).
    fn serial(&mut self) -> Option<String> {
        None
    }
}

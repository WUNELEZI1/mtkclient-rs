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

pub use real::RealSession;

use crate::core::capability::{Capability, CapabilitySet};
use crate::core::device::{DeviceState, ProtocolVersion};
use crate::da::engine::{DaEngine, EngineError};
use crate::transport::RetryPolicy;
use crate::transport::vdev::VirtualDevice;

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

// =============================================================================
// Simulation session
// =============================================================================

/// Offline simulation session.
pub struct SimSession {
    dev: VirtualDevice,
    engine: DaEngine,
    da: Option<Vec<u8>>,
    gpt: Option<Vec<u8>>,
    info: Option<DeviceInfo>,
    state: DeviceState,
    /// Capabilities granted by exploits accepted on the device side (the simulation confirms they took effect)
    granted: CapabilitySet,
    /// Opened storage information (the storage parameter for writes/erases)
    storage: Option<crate::da::engine::StorageInfo>,
    /// Bounded transient-timeout retry policy (kept across reconnects and reloads)
    retry: RetryPolicy,
}

impl SimSession {
    pub fn new(dev: VirtualDevice) -> Self {
        let state = dev.profile.to_state();
        Self {
            dev,
            engine: DaEngine::new(),
            da: None,
            gpt: None,
            info: None,
            state,
            granted: CapabilitySet::EMPTY,
            storage: None,
            retry: RetryPolicy::default(),
        }
    }

    /// Set the transient-timeout retry policy (from `--max-retries`).
    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self.engine = std::mem::take(&mut self.engine).with_retry(policy);
        self
    }

    /// Inject DA bytes (the simulation skips signature checks but runs the full upload flow).
    pub fn with_da(mut self, da: Vec<u8>) -> Self {
        self.da = Some(da);
        self
    }

    /// Device-side event log (for test assertions / diagnostics).
    pub fn events(&self) -> &[String] {
        &self.dev.events
    }

    /// Save the virtual device flash state (for cross-process resume demos).
    pub fn save_state(&self, path: &str) -> Result<(), String> {
        self.dev.save_state(path)
    }

    /// Load the virtual device flash state.
    pub fn load_state(&mut self, path: &str) -> Result<(), String> {
        self.dev.load_state(path)?;
        self.engine = DaEngine::new().with_retry(self.retry);
        self.gpt = None;
        self.info = None;
        self.storage = None;
        Ok(())
    }

    #[cfg(test)]
    /// Simulate a replug: protocol state is reset, flash contents are kept (for resume tests).
    pub fn replug(&mut self) {
        self.dev.replug();
        self.engine = DaEngine::new().with_retry(self.retry);
        self.gpt = None;
        self.info = None;
        self.storage = None;
    }

    fn map_err(e: EngineError) -> String {
        e.to_string()
    }
}

impl Session for SimSession {
    fn label(&self) -> &'static str {
        "sim"
    }

    fn state(&self) -> DeviceState {
        self.state.clone()
    }

    fn caps(&self) -> CapabilitySet {
        use crate::da::engine::Phase;
        let mut set = CapabilitySet::EMPTY;
        if matches!(self.engine.phase(), Phase::Da1 | Phase::Ready) {
            set.insert(Capability::DaLoaded);
            set.insert(if self.state.protocol == ProtocolVersion::V6 {
                Capability::DaXml
            } else {
                Capability::DaXFlash
            });
        }
        // A cleared security flag on the device is equivalent to "BROM bypass complete"
        if self.engine.phase() != Phase::Idle && self.dev_bypassed() {
            set.insert(Capability::SafetyBypassed);
            set.insert(Capability::DaAuthBypass);
        }
        if self.engine.phase() == Phase::Ready {
            set.insert(Capability::StorageRead);
            set.insert(Capability::StorageWrite);
        }
        if self.gpt.is_some() {
            set.insert(Capability::GptParsed);
        }
        set.union(self.granted)
    }

    fn da(&self) -> Option<&[u8]> {
        self.da.as_deref()
    }

    fn da_source(&self) -> &'static str {
        if self.da.is_some() {
            "sim-injected"
        } else {
            "none"
        }
    }

    fn apply_exploit(&mut self, name: &'static str) -> Result<(), String> {
        // The handshake must be done first: on real devices the exploit is also a BROM-stage artifact
        if self.engine.phase() == crate::da::engine::Phase::Idle {
            self.engine
                .connect(&mut self.dev)
                .map_err(SimSession::map_err)?;
        }
        self.engine
            .sim_exploit(&mut self.dev, name)
            .map_err(SimSession::map_err)?;
        // The device confirms the exploit → the host gains every capability declared by the registry
        if let Some(meta) = crate::exploit::registry::ExploitRegistry::builtin().get(name) {
            self.granted = self.granted.union(meta.produces);
        }
        Ok(())
    }

    fn load_da(&mut self) -> Result<(), String> {
        let da = self
            .da
            .clone()
            .ok_or_else(|| "未提供 DA 文件（--da / --loader）".to_string())?;
        if self.engine.phase() == crate::da::engine::Phase::Idle {
            self.engine
                .connect(&mut self.dev)
                .map_err(SimSession::map_err)?;
        }
        self.engine
            .load_da(&mut self.dev, &da)
            .map_err(SimSession::map_err)?;
        // Run one extensions handshake after DA1 starts (a required post-patch step on real devices)
        let _ = self.engine.load_da_extensions(&mut self.dev);
        Ok(())
    }

    fn open_storage(&mut self) -> Result<(), String> {
        if self.engine.phase() == crate::da::engine::Phase::Idle {
            self.engine
                .connect(&mut self.dev)
                .map_err(SimSession::map_err)?;
        }
        // Query failure is non-fatal (older DAs lack support); capacity fields stay 0 and a warning is logged
        let storage = self
            .engine
            .query_storage(&mut self.dev)
            .unwrap_or_else(|_| crate::da::engine::StorageInfo {
                block: 512,
                ..Default::default()
            });
        self.engine
            .open_storage(&mut self.dev)
            .map_err(SimSession::map_err)?;
        self.storage = Some(storage);
        self.info = Some(DeviceInfo {
            name: self.state.name.clone(),
            hw_code: self.state.hw_code,
            da_code: self.state.da_code,
            protocol: self.state.protocol.label(),
            boot1: storage.boot1,
            boot2: storage.boot2,
            user: storage.user,
            block: storage.block,
            emmc_type: storage.type_name().to_string(),
            cid: Vec::new(),
            slot: self.state.current_slot,
        });
        Ok(())
    }

    fn parse_gpt(&mut self) -> Result<(), String> {
        // Read 32KB first (primary GPT header + entries); expand on demand if that is too small
        let mut data = self
            .engine
            .read_flash(&mut self.dev, 0, 32768)
            .map_err(SimSession::map_err)?;
        let info = crate::partition::GptInfo::parse(&data)?;
        let need = 512 + info.num_part_entries as u64 * info.part_entry_size as u64;
        if need > data.len() as u64 {
            data = self
                .engine
                .read_flash(&mut self.dev, 0, need)
                .map_err(SimSession::map_err)?;
        }
        if &data[512..520] != b"EFI PART" {
            return Err("GPT 签名缺失".into());
        }
        self.gpt = Some(data);
        Ok(())
    }

    fn gpt(&self) -> Option<&[u8]> {
        self.gpt.as_deref()
    }

    fn device_info(&mut self) -> Result<DeviceInfo, String> {
        if let Some(i) = &self.info {
            return Ok(i.clone());
        }
        self.open_storage()?;
        self.info
            .clone()
            .ok_or_else(|| "无法获取设备信息".to_string())
    }

    fn find_partition(&mut self, name: &str) -> Result<PartitionSpan, String> {
        // boot1/boot2 are special partitions (they are not in the GPT)
        let (parttype, special_size) = match name {
            "boot1" | "boot2" => {
                let st = self.ensure_storage()?;
                let size = if name == "boot1" { st.boot1 } else { st.boot2 };
                (if name == "boot1" { 0u32 } else { 1u32 }, size)
            }
            _ => (8u32, 0u64),
        };
        if self.gpt.is_none() {
            self.parse_gpt()?;
        }
        let gpt = self.gpt.clone().ok_or_else(|| "GPT 不可用".to_string())?;
        let info = crate::partition::GptInfo::parse(&gpt)?;
        let entry = info
            .find_partition(name)
            .ok_or_else(|| format!("未找到分区: {}", name))?;
        let size = if special_size > 0 {
            special_size
        } else {
            entry.size
        };
        Ok(PartitionSpan {
            addr: entry.start_addr,
            size,
            parttype,
        })
    }

    fn read_flash(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        // A single READ_DATA is capped at 1 MiB (the real DA chunks by packet length too)
        let mut out = Vec::with_capacity(size as usize);
        let mut pos = 0u64;
        while pos < size {
            let take = (size - pos).min(0x10_0000);
            let chunk = self
                .engine
                .read_flash(&mut self.dev, addr + pos, take)
                .map_err(SimSession::map_err)?;
            out.extend_from_slice(&chunk);
            pos += take;
        }
        Ok(out)
    }

    fn write_flash(&mut self, addr: u64, data: &[u8]) -> Result<(), String> {
        let storage = self.storage_type();
        self.engine
            .begin_write(&mut self.dev, addr, data.len() as u64, storage, 8)
            .map_err(SimSession::map_err)?;
        let mut pos = 0usize;
        while pos < data.len() {
            let take = (data.len() - pos).min(0x10_0000);
            let mut chunk = data[pos..pos + take].to_vec();
            while !chunk.len().is_multiple_of(512) {
                chunk.push(0);
            }
            self.engine
                .write_chunk(&mut self.dev, &chunk)
                .map_err(SimSession::map_err)?;
            pos += take;
        }
        self.engine
            .end_write(&mut self.dev)
            .map_err(SimSession::map_err)
    }

    fn write_stream(
        &mut self,
        addr: u64,
        src: &mut dyn std::io::Read,
        total: u64,
        start: u64,
        on_progress: &mut dyn FnMut(u64),
    ) -> Result<(), String> {
        let storage = self.storage_type();
        self.engine
            .begin_write(&mut self.dev, addr + start, total - start, storage, 8)
            .map_err(SimSession::map_err)?;
        let mut pos = start;
        let mut buf = vec![0u8; 0x10_0000];
        while pos < total {
            let want = ((total - pos) as usize).min(buf.len());
            let mut n = 0usize;
            while n < want {
                let r = src
                    .read(&mut buf[n..want])
                    .map_err(|e| format!("读取源数据失败: {}", e))?;
                if r == 0 {
                    break;
                }
                n += r;
            }
            if n == 0 {
                return Err(format!("源数据提前结束: 期望 {} 字节", total - pos));
            }
            let mut chunk = buf[..n].to_vec();
            while !chunk.len().is_multiple_of(512) {
                chunk.push(0);
            }
            self.engine
                .write_chunk(&mut self.dev, &chunk)
                .map_err(SimSession::map_err)?;
            pos += n as u64;
            on_progress(pos);
        }
        self.engine
            .end_write(&mut self.dev)
            .map_err(SimSession::map_err)
    }

    fn erase_partition(&mut self, name: &str) -> Result<u64, String> {
        let span = self.find_partition(name)?;
        if span.size == 0 {
            return Err(format!("分区 {} 大小为 0", name));
        }
        self.engine
            .format(&mut self.dev, span.parttype, span.addr, span.size)
            .map_err(SimSession::map_err)?;
        Ok(span.size)
    }

    fn active_slot(&mut self) -> Result<Option<char>, String> {
        if self.gpt.is_none() {
            self.parse_gpt()?;
        }
        let has_a = self.partition_exists("boot_a") || self.partition_exists("vbmeta_a");
        let has_b = self.partition_exists("boot_b") || self.partition_exists("vbmeta_b");
        if !has_a && !has_b {
            return Ok(None);
        }
        // bootctrl readability is profile-driven (on real devices it comes from the misc boot_control struct)
        if self.dev.profile.bootctrl
            && let Some(s) = self.dev.profile.slot
        {
            return Ok(Some(s));
        }
        match (has_a, has_b) {
            (true, true) => Err(
                "无法确定活动 A/B 槽位（bootctrl 读取失败，且 GPT 中 _a/_b 同时存在）".to_string(),
            ),
            (true, false) => Ok(Some('a')),
            _ => Ok(Some('b')),
        }
    }

    fn seccfg_sign(&mut self, digest: &[u8; 32], _hwtype: &str) -> Result<Vec<u8>, String> {
        // The simulated device does not verify the seccfg hash and returns the digest as-is (sim-only; real devices use HACC/SEJ)
        Ok(digest.to_vec())
    }
}

impl SimSession {
    fn ensure_storage(&mut self) -> Result<crate::da::engine::StorageInfo, String> {
        if let Some(s) = self.storage {
            return Ok(s);
        }
        let s = self.engine.query_storage(&mut self.dev).unwrap_or_default();
        self.storage = Some(s);
        Ok(s)
    }

    fn storage_type(&self) -> u32 {
        self.storage.map(|s| s.storage_type()).unwrap_or(0)
    }

    fn partition_exists(&self, name: &str) -> bool {
        self.gpt
            .as_ref()
            .and_then(|g| crate::partition::GptInfo::parse(g).ok())
            .map(|i| i.find_partition(name).is_some())
            .unwrap_or(false)
    }
}

impl SimSession {
    fn dev_bypassed(&self) -> bool {
        // VirtualDevice exposes no `bypassed` field, so infer from the event log:
        // this reflects real device-side state, not session-side bookkeeping.
        self.dev
            .events
            .iter()
            .any(|e| e.contains("SIM exploit 接受"))
    }
}

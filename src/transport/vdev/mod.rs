//! Virtual MTK device (100% offline simulation).
//!
//! This is the **device side**: it receives host bytes, parses them per the BROM / XFlash
//! protocol, and produces device responses. Paired with `da::engine` (host-side state machine)
//! it can run the whole `connect → DA upload → open storage → read GPT` flow without a real device.
//!
//! Design trade-offs:
//! - Parsing is fed **one byte at a time** with explicit state-machine state, so fragmented
//!   arrival (USB short packets) works naturally;
//! - A device profile ([`DeviceProfile`]) describes "what machine this is": chip, security
//!   flags, storage sizes, partition layout, fault injection points; several presets built in (see `preset`);
//! - The GPT is **generated on the fly** from the partition list (including both CRCs), so
//!   `GptInfo::parse`'s validation path is genuinely exercised rather than fed fixed bytes;
//! - Exploits cannot be reproduced in a byte stream, so a sim-only command [`wire::SIM_EXPLOIT_CMD`]
//!   means "the device accepts this exploit", used to validate the orchestration chain (marked sim-only in docs).

use std::collections::VecDeque;

use super::wire;
use super::{PortError, StreamPort};

mod chaos;
mod image;
mod port;
mod profile;
mod protocol;
mod storage;
#[cfg(test)]
mod tests;

pub use self::chaos::ChaosConfig;
#[allow(unused_imports)]
pub use self::profile::{DeviceFault, DeviceProfile, StorageProfile};

use self::chaos::{ChaosRng, ChaosState};
use self::image::{build_image, build_pages};

// =============================================================================
// Virtual device
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Handshake: i bytes received so far
    Handshake(usize),
    /// BROM single-byte command state
    Brom,
    /// 4-byte word command (entered when the first byte is 0x00)
    BromWord { buf: [u8; 4], n: usize },
    /// SEND_DA args: 0=addr 1=size 2=siglen
    SendDaArgs { idx: u8, buf: [u8; 4], n: usize },
    /// SEND_DA data body
    SendDaData,
    /// JUMP_DA first address echo
    JumpEcho { buf: [u8; 4], n: usize },
    /// JUMP_DA second address (triggers the jump)
    JumpTrigger { buf: [u8; 4], n: usize },
    /// DA packet header
    DaHeader { buf: [u8; 12], n: usize },
    /// DA payload
    DaPayload { len: usize, buf: Vec<u8> },
    /// id byte of the sim-only exploit handshake
    ExploitId,
}

/// Exploit names known to the simulated device (id = index).
pub const SIM_EXPLOITS: [&str; 5] = [
    "kamakiri2",
    "carbonara",
    "heapbait",
    "hashimoto",
    // CVE-2022-20069 (Preloader OOB); the CVE-2022-20435 alias is a historical mistake.
    "cve-20069",
];

/// Exploit name → id (None for unregistered names).
pub fn exploit_id(name: &str) -> Option<u8> {
    SIM_EXPLOITS
        .iter()
        .position(|n| *n == name)
        .map(|i| i as u8)
}

/// Virtual device.
pub struct VirtualDevice {
    pub profile: DeviceProfile,
    out: VecDeque<u8>,
    state: State,
    /// Number of DA segments uploaded (0 → next is DA1, 1 → DA2)
    da_stage: u8,
    send_addr: u32,
    send_size: usize,
    send_siglen: u32,
    da_buf: Vec<u8>,
    /// Whether the security flags have been cleared by an exploit
    bypassed: bool,
    /// DA command currently waiting for parameters
    pending_cmd: Option<u32>,
    /// Most recent READ_DATA request
    pending_read: Option<(u64, u64)>,
    /// Storage image (GPT area)
    image: Vec<u8>,
    /// Partition content (stored per 4 KiB page, sparse; misses fall back to image / 0x00)
    pages: std::collections::HashMap<u64, Vec<u8>>,
    /// Erased ranges (read back as 0xFF). Stored as ranges rather than page-filling,
    /// otherwise erasing a 1 GiB partition would consume 1 GiB of memory.
    erased: Vec<(u64, u64)>,
    /// Current write transaction
    write: Option<WriteSession>,
    /// FORMAT transaction stage (0=idle 1=waiting for first ack 2=waiting for final ack)
    format_stage: u8,
    /// Cumulative bytes written to this device (for fault injection)
    total_written: u64,
    faults: Vec<(usize, DeviceFault)>,
    host_bytes: usize,
    dead: bool,
    silent: bool,
    /// Device-side event log (for test assertions)
    pub events: Vec<String>,
    /// Host-side write buffer (diagnostics)
    pub written: Vec<u8>,
    /// Chaos injection (`--sim-chaos`); `None` = off, behavior identical to plain simulation.
    chaos: Option<ChaosState>,
}

/// Storage page size (sparse storage granularity).
const PAGE: u64 = 4096;
/// Magic of the simulation state file (8 bytes, right after the u32 version).
const STATE_MAGIC: &[u8; 8] = b"VDEV1\0\0\0";
/// Simulation state file layout version; bump on layout changes to avoid silently misreading old formats.
const STATE_VERSION: u32 = 1;

/// A single write transaction (device side).
#[derive(Debug)]
struct WriteSession {
    addr: u64,
    size: u64,
    written: u64,
    /// 0=expects 0u32 1=expects checksum 2=expects data
    stage: u8,
    checksum: u32,
}

impl VirtualDevice {
    pub fn new(profile: DeviceProfile) -> Self {
        let image = build_image(&profile);
        let pages = build_pages(&profile);
        let faults = profile.faults.clone();
        Self {
            profile,
            out: VecDeque::new(),
            state: State::Handshake(0),
            da_stage: 0,
            send_addr: 0,
            send_size: 0,
            send_siglen: 0,
            da_buf: Vec::new(),
            bypassed: false,
            pending_cmd: None,
            pending_read: None,
            pages,
            erased: Vec::new(),
            image,
            write: None,
            format_stage: 0,
            total_written: 0,
            faults,
            host_bytes: 0,
            dead: false,
            silent: false,
            events: Vec::new(),
            written: Vec::new(),
            chaos: None,
        }
    }

    /// Build a device from a preset name (entry point for tests and external tools).
    #[allow(dead_code)]
    pub fn from_preset(name: &str) -> Option<Self> {
        DeviceProfile::preset(name).map(Self::new)
    }

    /// Enable chaos injection (`--sim-chaos`). A noop config is equivalent to disabled.
    pub fn with_chaos(mut self, cfg: ChaosConfig) -> Self {
        self.chaos = if cfg.is_noop() {
            None
        } else {
            Some(ChaosState {
                cfg,
                rng: ChaosRng::new(cfg.seed),
                bitflips: 0,
                drops: 0,
                silences: 0,
                glitches: 0,
            })
        };
        self
    }

    /// Chaos counters (bitflips, drops, silences, glitches) — for diagnostics/tests.
    #[allow(dead_code)]
    pub fn chaos_counts(&self) -> (u64, u64, u64, u64) {
        match &self.chaos {
            Some(c) => (c.bitflips, c.drops, c.silences, c.glitches),
            None => (0, 0, 0, 0),
        }
    }

    /// Build a device from profile text (entry point for external tools).
    #[allow(dead_code)]
    pub fn profile_text(text: &str) -> Result<Self, String> {
        DeviceProfile::from_text(text).map(Self::new)
    }

    /// Save flash state (pages + erased ranges) to a file, to reproduce "replug/reconnect" across processes.
    pub fn save_state(&self, path: &str) -> Result<(), String> {
        // Header is a fixed 12 bytes: magic(8) + version(4), strictly symmetric with `load_state`'s p=12
        let mut buf = Vec::with_capacity(12 + self.pages.len() * (8 + PAGE as usize));
        buf.extend_from_slice(STATE_MAGIC);
        buf.extend_from_slice(&STATE_VERSION.to_le_bytes());
        buf.extend_from_slice(&(self.pages.len() as u32).to_le_bytes());
        for (k, v) in &self.pages {
            buf.extend_from_slice(&k.to_le_bytes());
            buf.extend_from_slice(v);
        }
        buf.extend_from_slice(&(self.erased.len() as u32).to_le_bytes());
        for (s, e) in &self.erased {
            buf.extend_from_slice(&s.to_le_bytes());
            buf.extend_from_slice(&e.to_le_bytes());
        }
        buf.extend_from_slice(&self.total_written.to_le_bytes());
        std::fs::write(path, buf).map_err(|e| format!("保存仿真状态失败: {}", e))
    }

    /// Load flash state (inverse of `save_state`).
    pub fn load_state(&mut self, path: &str) -> Result<(), String> {
        let buf = std::fs::read(path).map_err(|e| format!("读取仿真状态失败: {}", e))?;
        if buf.len() < 12 || &buf[0..8] != STATE_MAGIC {
            return Err("仿真状态文件格式不匹配".to_string());
        }
        let ver = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        if ver != STATE_VERSION {
            return Err(format!(
                "仿真状态文件版本不支持: {}（当前 {}）",
                ver, STATE_VERSION
            ));
        }
        let mut p = 12usize;
        let rd_u32 = |b: &[u8], o: usize| -> u32 {
            u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
        };
        let pages = rd_u32(&buf, p) as usize;
        p += 4;
        // Page count cap: prevent a corrupt file from forcing a huge preallocation (4 GiB / 4 KiB = 1M pages, 8x margin)
        if pages > 8 * 1024 * 1024 || 12 + 4 + pages * (8 + PAGE as usize) > buf.len() {
            return Err("仿真状态文件被截断（页区）".to_string());
        }
        self.pages.clear();
        for _ in 0..pages {
            if p + 8 + PAGE as usize > buf.len() {
                return Err("仿真状态文件被截断（页区）".to_string());
            }
            let key = u64::from_le_bytes(buf[p..p + 8].try_into().unwrap());
            p += 8;
            self.pages.insert(key, buf[p..p + PAGE as usize].to_vec());
            p += PAGE as usize;
        }
        let ranges = if p + 4 <= buf.len() {
            rd_u32(&buf, p) as usize
        } else {
            return Err("仿真状态文件被截断（擦除区）".to_string());
        };
        p += 4;
        self.erased.clear();
        if ranges > 1024 * 1024 || p + ranges * 16 > buf.len() {
            return Err("仿真状态文件被截断（擦除区）".to_string());
        }
        for _ in 0..ranges {
            if p + 16 > buf.len() {
                return Err("仿真状态文件被截断（擦除区）".to_string());
            }
            let s = u64::from_le_bytes(buf[p..p + 8].try_into().unwrap());
            let e = u64::from_le_bytes(buf[p + 8..p + 16].try_into().unwrap());
            p += 16;
            self.erased.push((s, e));
        }
        if p + 8 <= buf.len() {
            self.total_written = u64::from_le_bytes(buf[p..p + 8].try_into().unwrap());
        }
        self.ev(format!("状态已载入: {} 页", self.pages.len()));
        Ok(())
    }

    #[cfg(test)]
    /// Simulate one "replug": clear faults and protocol state, **keep flash content** (writes/erases are not lost).
    ///
    /// Resume semantics rest on exactly this assumption: after reconnect the storage still holds
    /// the data, so writing can continue from the offset recorded in `.resume`.
    pub fn replug(&mut self) {
        self.out.clear();
        self.state = State::Handshake(0);
        self.dead = false;
        self.silent = false;
        self.host_bytes = 0;
        self.da_stage = 0;
        self.da_buf.clear();
        self.pending_cmd = None;
        self.pending_read = None;
        self.write = None;
        self.format_stage = 0;
        self.faults.clear();
        self.bypassed = false;
        self.ev("replug: 已重连（flash 内容保留）");
    }

    fn ev(&mut self, msg: impl Into<String>) {
        self.events.push(msg.into());
    }

    fn push(&mut self, bytes: &[u8]) {
        self.out.extend(bytes.iter().copied());
    }

    fn push_packet(&mut self, dtype: u32, payload: &[u8]) {
        let mut hdr = [0u8; wire::XFLASH_HEADER_LEN];
        hdr[0..4].copy_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
        hdr[4..8].copy_from_slice(&dtype.to_le_bytes());
        hdr[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        self.push(&hdr);
        self.push(payload);
    }

    fn push_status(&mut self, value: u32) {
        self.push_packet(wire::XFLASH_TYPE_FLOW, &value.to_le_bytes());
    }
}

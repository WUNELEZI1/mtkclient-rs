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

// =============================================================================
// Device profiles
// =============================================================================

/// Storage profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageProfile {
    pub boot1: u64,
    pub boot2: u64,
    pub user: u64,
    pub rpmb: u64,
    pub block: u32,
    /// 0=eMMC 1=UFS 2=SD 3=MMC 6=UFS_CARD
    pub emmc_type: u32,
}

impl Default for StorageProfile {
    fn default() -> Self {
        Self {
            boot1: 0x40_0000,
            boot2: 0x40_0000,
            user: 0x1D1A_00000,
            rpmb: 0x400_0000,
            block: 512,
            emmc_type: 0,
        }
    }
}

/// Fault injection (device side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFault {
    /// From host byte N onward the device stops responding (host sees a timeout)
    Silent,
    /// From host byte N onward the device disconnects (host sees Disconnected)
    Dead,
    /// Disconnect once the cumulative written bytes reach N (simulates an unplug mid-write, for resume tests)
    WriteDead(u64),
    /// Go silent once the cumulative written bytes reach N (simulates a stuck write)
    WriteSilent(u64),
}

/// Chaos fault injection config (`--sim-chaos`).
///
/// Probabilities are always expressed in **per-mille**; `0` disables that item. All default to 0
/// (most stable; behavior identical to plain simulation). A **fixed seed** drives a deterministic
/// PRNG, so the same seed reproduces the same fault sequence exactly — the precondition for chaos
/// to both "hit the limits" and "regress reproducibly".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChaosConfig {
    pub seed: u64,
    /// Bit-flip probability (‰) when a write lands: simulates flash bit decay / link noise.
    /// Note: the flip happens **after the checksum passed**, so only read-back verification/CRC
    /// can catch it — exactly the guard line ensuring "silent corruption must never pass as success".
    pub bitflip_permille: u32,
    /// Probability (‰) of truncating a read response: simulates a bad cable dropping bytes (short packet).
    pub drop_permille: u32,
    /// Device silence probability (‰): triggers a read timeout and **recovers only after reconnect** (simulates a loose cable).
    pub silence_permille: u32,
    /// Transient timeout probability (‰): a single read times out then recovers (simulates latency spikes / contact jitter).
    pub glitch_permille: u32,
}

impl Default for ChaosConfig {
    fn default() -> Self {
        Self {
            seed: 0x0C0F_FEE0,
            bitflip_permille: 0,
            drop_permille: 0,
            silence_permille: 0,
            glitch_permille: 0,
        }
    }
}

impl ChaosConfig {
    /// Recommended `--sim-chaos` tier: not aggressive, but enough to expose resume/verify defects.
    pub fn typical(seed: Option<u64>) -> Self {
        Self {
            seed: seed.unwrap_or(0x5EED_1234),
            bitflip_permille: 8,
            // Silence = cable unplugged, requires manual replug, so it is not an "auto-recoverable"
            // stress tier; use `fault=silent` or explicit config to test disconnects/resume.
            drop_permille: 8,
            silence_permille: 0,
            glitch_permille: 8,
        }
    }

    /// Whether this is equivalent to "inject no faults".
    pub fn is_noop(&self) -> bool {
        self.bitflip_permille == 0
            && self.drop_permille == 0
            && self.silence_permille == 0
            && self.glitch_permille == 0
    }
}

/// SplitMix64: deterministic PRNG (zero dependencies, consistent across platforms).
#[derive(Debug, Clone)]
struct ChaosRng(u64);

impl ChaosRng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Hit with probability `p` (per-mille).
    fn hit(&mut self, p: u32) -> bool {
        p > 0 && (self.next_u64() % 1000) < p as u64
    }
}

/// Chaos runtime state (config + PRNG + counters).
#[derive(Debug, Clone)]
struct ChaosState {
    cfg: ChaosConfig,
    rng: ChaosRng,
    bitflips: u64,
    drops: u64,
    silences: u64,
    glitches: u64,
}

impl ChaosState {
    /// Bit-flip decision: on hit returns (byte index in payload, bit number).
    fn dirty(&mut self, len: usize) -> Option<(usize, u8)> {
        if len == 0 || !self.rng.hit(self.cfg.bitflip_permille) {
            return None;
        }
        self.bitflips += 1;
        let idx = (self.rng.next_u64() as usize) % len;
        let bit = (self.rng.next_u64() & 7) as u8;
        Some((idx, bit))
    }
}

/// Device profile.
#[derive(Debug, Clone)]
pub struct DeviceProfile {
    pub name: String,
    pub hw_code: u16,
    pub hw_sub: u16,
    pub hw_ver: u16,
    pub sw_ver: u16,
    /// da_code declared in the DA header (0x6768 etc.)
    pub da_code: u16,
    /// true = V6 (XML) platform
    pub v6: bool,
    /// Raw target config bits (security flags)
    pub safety: u32,
    /// SEND_DA returns 0x1D0D (SLA rejected)
    pub sla_required: bool,
    /// BROM requires auth (the BROM path is unreachable without auth)
    pub brom_auth: bool,
    pub preloader_patched: bool,
    pub da_sig_failed: bool,
    pub storage: StorageProfile,
    /// (name, start LBA, byte count)
    pub partitions: Vec<(String, u64, u64)>,
    pub faults: Vec<(usize, DeviceFault)>,
    /// Whether the device supports the eMMC info query
    pub emmc_info: bool,
    /// Exploit names accepted by the device side (simulation only)
    pub exploits: Vec<String>,
    /// Current active A/B slot (None = non-A/B device)
    pub slot: Option<char>,
    /// Whether bootctrl is readable (if false and both _a/_b exist → slot undeterminable)
    pub bootctrl: bool,
    /// lock_state of the seccfg partition (1=locked, 3=unlocked)
    pub seccfg_lock_state: u32,
    /// dm_verity_state of seccfg (offset 0x10)
    pub seccfg_dm_verity: u32,
    /// seccfg uses the V3 struct (`AND_SECCFG_v` header + software SEJ) instead of the default V4
    pub seccfg_v3: bool,
    /// Whether DA1 has applied Carbonara hardening (forces DA2 load address 0x40000000)
    pub da1_hardened: bool,
    /// Whether affected by CVE-2022-20069 (Preloader USB OOB write).
    /// Alias: CVE-2022-20435 (a mistaken id; actually an Android system service bug).
    pub cve_20069_affected: bool,
    /// Whether CVE-2022-20069 has been patched.
    pub cve_20069_patched: bool,
}

impl Default for DeviceProfile {
    fn default() -> Self {
        Self {
            name: "MT6768".into(),
            hw_code: 0x0707,
            hw_sub: 0x8A00,
            hw_ver: 0xCA00,
            sw_ver: 0x0000,
            da_code: 0x6768,
            v6: false,
            safety: 0,
            sla_required: false,
            brom_auth: false,
            preloader_patched: false,
            da_sig_failed: false,
            storage: StorageProfile::default(),
            partitions: Vec::new(),
            faults: Vec::new(),
            emmc_info: true,
            exploits: vec![],
            slot: Some('a'),
            bootctrl: true,
            seccfg_lock_state: 1,
            seccfg_dm_verity: 0,
            seccfg_v3: false,
            da1_hardened: false,
            cve_20069_affected: false,
            cve_20069_patched: false,
        }
    }
}

impl DeviceProfile {
    /// Ensure `storage.user` covers at least the last partition (otherwise GPT partitions exceed
    /// the disk end, which differs from real hardware and hides boundary bugs).
    fn fit_storage(&mut self) {
        let end = self
            .partitions
            .iter()
            .map(|(_, start, size)| (start + size.div_ceil(512)) * 512)
            .max()
            .unwrap_or(0);
        let need = end + 32 * 1024 * 1024;
        if self.storage.user < need {
            self.storage.user = need.next_power_of_two();
        }
        if self.storage.emmc_type != 1 && self.storage.block == 0 {
            self.storage.block = 512;
        }
    }

    /// Convert to the planner-layer device state.
    pub fn to_state(&self) -> crate::core::device::DeviceState {
        use crate::core::device::ProtocolVersion;
        crate::core::device::DeviceState {
            hw_code: self.hw_code,
            da_code: self.da_code,
            name: self.name.clone(),
            protocol: if self.v6 {
                ProtocolVersion::V6
            } else {
                ProtocolVersion::V5
            },
            safety: crate::core::safety::SafetyFlags::from_raw(self.safety),
            brom_auth_required: self.brom_auth,
            preloader_patched: self.preloader_patched,
            da_signature_failed: self.da_sig_failed,
            current_slot: self.slot,
            cve_20069_affected: self.cve_20069_affected,
            cve_20069_patched: self.cve_20069_patched,
        }
    }

    /// Parse from `key=value` text (one per line, `#` starts a comment).
    ///
    /// Partitions use `partition=name:start_lba:size_bytes`; faults use `fault=silent:24`.
    pub fn from_text(text: &str) -> Result<Self, String> {
        let mut p = DeviceProfile::default();
        p.partitions.clear();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("第 {} 行缺少 '=': {}", i + 1, line))?;
            let (k, v) = (k.trim(), v.trim());
            match k {
                "name" => p.name = v.to_string(),
                "hw_code" => p.hw_code = num(v)?.as_num::<u16>(k)?,
                "hw_sub" => p.hw_sub = num(v)?.as_num::<u16>(k)?,
                "hw_ver" => p.hw_ver = num(v)?.as_num::<u16>(k)?,
                "sw_ver" => p.sw_ver = num(v)?.as_num::<u16>(k)?,
                "da_code" => p.da_code = num(v)?.as_num::<u16>(k)?,
                "proto" => p.v6 = matches!(v, "v6" | "V6" | "xml"),
                "safety" => p.safety = num(v)?.as_num::<u32>(k)?,
                "sla" => p.sla_required = truthy(v),
                "brom_auth" => p.brom_auth = truthy(v),
                "preloader_patched" => p.preloader_patched = truthy(v),
                "da_sig_failed" => p.da_sig_failed = truthy(v),
                "emmc_info" => p.emmc_info = truthy(v),
                "storage.boot1" => p.storage.boot1 = num(v)?.0,
                "storage.boot2" => p.storage.boot2 = num(v)?.0,
                "storage.user" => p.storage.user = num(v)?.0,
                "storage.rpmb" => p.storage.rpmb = num(v)?.0,
                "storage.block" => p.storage.block = num(v)?.as_num::<u32>(k)?,
                "storage.type" => {
                    p.storage.emmc_type = match v {
                        "emmc" => 0,
                        "ufs" => 1,
                        "sd" => 2,
                        "mmc" => 3,
                        "ufs_card" => 6,
                        other => return Err(format!("未知 storage.type: {}", other)),
                    }
                }
                "partition" => {
                    let parts: Vec<&str> = v.split(':').collect();
                    if parts.len() != 3 {
                        return Err(format!("partition 需要 name:start:size: {}", v));
                    }
                    let start = num(parts[1])?.0;
                    let size = num(parts[2])?.0;
                    p.partitions.push((parts[0].to_string(), start, size));
                }
                "fault" => {
                    let parts: Vec<&str> = v.split(':').collect();
                    if parts.len() != 2 {
                        return Err(format!("fault 需要 kind:at: {}", v));
                    }
                    let at = num(parts[1])?.0;
                    match parts[0] {
                        "silent" => p.faults.push((at as usize, DeviceFault::Silent)),
                        "dead" => p.faults.push((at as usize, DeviceFault::Dead)),
                        "write_dead" => p.faults.push((0, DeviceFault::WriteDead(at))),
                        "write_silent" => p.faults.push((0, DeviceFault::WriteSilent(at))),
                        other => return Err(format!("未知 fault 类型: {}", other)),
                    }
                }
                "exploit" => p.exploits.push(v.to_string()),
                "slot" => {
                    p.slot = match v {
                        "none" | "off" | "-" => None,
                        "a" | "A" => Some('a'),
                        "b" | "B" => Some('b'),
                        other => return Err(format!("slot 只能是 a/b/none: {}", other)),
                    }
                }
                "bootctrl" => p.bootctrl = truthy(v),
                "seccfg.lock_state" => p.seccfg_lock_state = num(v)?.as_num::<u32>(k)?,
                "seccfg.dm_verity" => p.seccfg_dm_verity = num(v)?.as_num::<u32>(k)?,
                "seccfg.v3" => p.seccfg_v3 = truthy(v),
                "da1_hardened" => p.da1_hardened = truthy(v),
                // CVE-2022-20069; `cve_20435.*` is a compatibility alias for the historical mistaken id.
                "cve_20069.affected" | "cve_20435.affected" => p.cve_20069_affected = truthy(v),
                "cve_20069.patched" | "cve_20435.patched" => p.cve_20069_patched = truthy(v),
                other => return Err(format!("未知配置项: {}", other)),
            }
        }
        if p.partitions.is_empty() {
            p.partitions = default_partitions();
        }
        p.fit_storage();
        Ok(p)
    }

    /// Built-in presets (`--port mock://<preset-name>`).
    pub fn preset(name: &str) -> Option<Self> {
        let mut p = DeviceProfile {
            partitions: default_partitions(),
            ..Default::default()
        };
        match name {
            // Clean V5 machine: no security flags, plain DA load can read the table (cost=690 path)
            "v5-clean" => {
                p.name = "MT6768 (V5 clean)".into();
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // V3 seccfg machine: verifies bootloader unlock/lock support across versions
            "v5-seccfg-v3" => {
                p.name = "MT6768 (V3 seccfg)".into();
                p.seccfg_v3 = true;
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // V5 machine with SLA on: must bypass first, otherwise SEND_DA returns 0x1D0D
            "v5-sla" => {
                p.name = "MT6768 (SLA enabled)".into();
                p.safety = 0x2; // SLA
                p.sla_required = true;
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // Carbonara-hardened machine: SLA on, but DA1 already carries the patch signature → carbonara unusable.
            // Used to verify "the Planner does not plan a Carbonara path when probe returns NotVulnerable".
            "carbonara-hardened" => {
                p.name = "MT6768 (Carbonara hardened)".into();
                p.safety = 0x2;
                p.sla_required = true;
                p.da1_hardened = true;
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // CVE-2022-20069 (Preloader USB OOB write): affected and unpatched → auth can be bypassed
            "cve-20069-vulnerable" => {
                p.name = "MT6768 (CVE-2022-20069 affected)".into();
                p.safety = 0x2;
                p.sla_required = true;
                // BROM needs auth and kamakiri2 is unavailable → the only viable path is Preloader OOB
                p.brom_auth = true;
                p.cve_20069_affected = true;
                // Accept only this CVE's path, to verify the Planner really picks it (rather than
                // being taken by the equally cheap kamakiri2 ranked ahead).
                p.exploits = vec!["cve-20069".into()];
            }
            // Same chip but OEM-patched → probe must report unavailable
            "cve-20069-patched" => {
                p.name = "MT6768 (CVE-2022-20069 patched)".into();
                p.safety = 0x2;
                p.sla_required = true;
                p.brom_auth = true;
                // Also block Carbonara so "no available bypass" is the only conclusion
                p.da1_hardened = true;
                p.cve_20069_affected = true;
                p.cve_20069_patched = true;
                p.exploits = vec!["cve-20069".into()];
            }
            // DAA + SBC both on: only a DA-layer patch can get through
            "v5-daa" => {
                p.name = "MT6785 (DAA+SBC)".into();
                p.hw_code = 0x0707;
                p.da_code = 0x6785;
                p.safety = 0x2 | 0x4; // SLA + DAA
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // V6 platform: BROM unreachable, DA path only
            "v6" => {
                p.name = "MT6983 (V6 XML)".into();
                p.hw_code = 0x0A0A;
                p.da_code = 0x6983;
                p.v6 = true;
                p.partitions = v6_partitions();
                p.exploits = vec!["carbonara".into()];
            }
            // UFS + A/B layout
            "ufs-ab" => {
                p.name = "MT6877 (UFS, A/B)".into();
                p.da_code = 0x6877;
                p.storage = StorageProfile {
                    boot1: 0,
                    boot2: 0,
                    user: 0x3B80_00000,
                    rpmb: 0x1000_0000,
                    block: 4096,
                    emmc_type: 1,
                };
                p.partitions = ab_partitions();
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // Flaky link: device goes silent from host byte 400 (simulates poor cable contact)
            "flaky" => {
                p.name = "MT6768 (flaky link)".into();
                p.faults = vec![(400, DeviceFault::Silent)];
                p.exploits = vec!["kamakiri2".into()];
            }
            // Drop mid-way: disconnect before the DA upload
            "unplug" => {
                p.name = "MT6768 (unplugged)".into();
                p.faults = vec![(120, DeviceFault::Dead)];
            }
            // Unplug mid-write: disconnect at 10MB, for resume tests
            "flaky-write" => {
                p.name = "MT6768 (write drops @10MB)".into();
                p.faults = vec![(0, DeviceFault::WriteDead(10 * 1024 * 1024))];
                p.exploits = vec!["kamakiri2".into()];
            }
            // Mixed A/B layout + unreadable bootctrl: slot undeterminable (vbmeta must error, not guess)
            "ab-unknown-slot" => {
                p.name = "MT6877 (A/B, bootctrl unreadable)".into();
                p.da_code = 0x6877;
                p.partitions = ab_partitions();
                p.slot = None;
                p.bootctrl = false;
                p.exploits = vec!["kamakiri2".into()];
            }
            // DA rejects service (SEND_DA status error): for error propagation tests
            "no-emmc-info" => {
                p.name = "MT6768 (no emmc info)".into();
                p.emmc_info = false;
                p.exploits = vec!["kamakiri2".into()];
            }
            _ => return None,
        }
        p.fit_storage();
        Some(p)
    }
}

/// Lay out partitions from LBA 34 in the given order (1 MiB aligned), returning (name, start LBA, byte count).
fn layout(items: &[(&str, u64)]) -> Vec<(String, u64, u64)> {
    const ALIGN_LBA: u64 = 2048; // 1 MiB
    let mut lba = 34u64;
    let mut out = Vec::with_capacity(items.len());
    for (name, size) in items {
        let start = lba.div_ceil(ALIGN_LBA) * ALIGN_LBA;
        out.push(((*name).to_string(), start, *size));
        lba = start + size.div_ceil(512);
    }
    out
}

fn default_partitions() -> Vec<(String, u64, u64)> {
    layout(&[
        ("seccfg", 0x20_000),
        ("misc", 0x10_0000),
        ("para", 0x80_000),
        ("boot_a", 0x20_00000),
        ("boot_b", 0x20_00000),
        ("recovery", 0x30_00000),
        ("vbmeta_a", 0x8000),
        ("vbmeta_b", 0x8000),
        ("super", 0x800_00000),
        ("userdata", 0x4000_00000),
        ("metadata", 0x200_0000),
    ])
}

fn v6_partitions() -> Vec<(String, u64, u64)> {
    layout(&[
        ("seccfg", 0x20_000),
        ("misc", 0x10_0000),
        ("boot_a", 0x60_00000),
        ("init_boot_a", 0x80_0000),
        ("vendor_boot_a", 0x60_00000),
        ("vbmeta_system_a", 0x8000),
        ("super", 0x1000_00000),
        ("userdata", 0x8000_00000),
    ])
}

fn ab_partitions() -> Vec<(String, u64, u64)> {
    layout(&[
        ("seccfg", 0x20_000),
        ("misc", 0x10_0000),
        ("boot_a", 0x40_00000),
        ("boot_b", 0x40_00000),
        ("vendor_boot_a", 0x40_00000),
        ("vbmeta_a", 0x10000),
        ("vbmeta_b", 0x10000),
        ("vbmeta_system_a", 0x10000),
        ("vbmeta_system_b", 0x10000),
        ("super", 0x2000_00000),
        ("userdata", 0x10000_00000),
    ])
}

fn truthy(v: &str) -> bool {
    matches!(v, "1" | "true" | "yes" | "on")
}

struct ParsedNum(u64);
impl ParsedNum {
    fn as_num<T: TryFrom<u64>>(&self, key: &str) -> Result<T, String> {
        T::try_from(self.0).map_err(|_| format!("{} 数值超出范围: {}", key, self.0))
    }
}

fn num(v: &str) -> Result<ParsedNum, String> {
    let s = v.trim();
    let out = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(&h.replace('_', ""), 16).map_err(|e| format!("{}: {}", s, e))?
    } else {
        s.replace('_', "")
            .parse::<u64>()
            .map_err(|e| format!("{}: {}", s, e))?
    };
    Ok(ParsedNum(out))
}

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

/// Image window size: enough to cover LBA0.. primary GPT + partition entries (32KB reads suffice, with margin).
const IMAGE_LEN: usize = 1024 * 1024;

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

    /// Advance the state machine by one byte.
    fn feed(&mut self, b: u8) -> Result<(), PortError> {
        self.host_bytes += 1;
        self.written.push(b);
        if self.dead {
            return Err(PortError::Disconnected);
        }
        // Fault injection: go silent / disconnect at the trigger point
        if self
            .faults
            .iter()
            .any(|(at, kind)| *at == self.host_bytes && *kind == DeviceFault::Dead)
        {
            self.dead = true;
            self.ev(format!("fault: dead at host byte {}", self.host_bytes));
            return Err(PortError::Disconnected);
        }
        if self
            .faults
            .iter()
            .any(|(at, kind)| *at == self.host_bytes && *kind == DeviceFault::Silent)
        {
            self.silent = true;
            self.ev(format!("fault: silent at host byte {}", self.host_bytes));
            return Ok(());
        }
        if self.silent {
            return Ok(());
        }

        let state = std::mem::replace(&mut self.state, State::Brom);
        self.state = match state {
            State::Handshake(i) => {
                let want = wire::BROM_HANDSHAKE[i];
                if b != want {
                    self.ev(format!("handshake: 期望 0x{:02X} 收到 0x{:02X}", want, b));
                    // Echo a wrong byte so the host retries
                    self.push(&[!b]);
                    State::Handshake(0)
                } else {
                    self.push(&[!b]);
                    if i + 1 == wire::BROM_HANDSHAKE.len() {
                        self.ev("handshake: ok");
                        State::Brom
                    } else {
                        State::Handshake(i + 1)
                    }
                }
            }
            State::Brom => {
                // BROM sync FE does not echo the command itself, only a status byte
                if b == 0xFE {
                    self.push(&[wire::BROM_SYNC_FE_ACK]);
                    State::Brom
                } else if b == wire::SIM_EXPLOIT_CMD {
                    // Sim-only handshake: the command itself is not echoed; response is [cmd, id, status]
                    State::ExploitId
                } else if b == 0x00 {
                    // Possibly a 4-byte word command (BE, high byte 0)
                    State::BromWord {
                        buf: [0, 0, 0, 0],
                        n: 1,
                    }
                } else {
                    self.on_brom_cmd(b)?
                }
            }
            State::BromWord { mut buf, n } => {
                buf[n] = b;
                if n + 1 < 4 {
                    State::BromWord { buf, n: n + 1 }
                } else {
                    let word = u32::from_be_bytes(buf);
                    // Echo the whole word
                    self.push(&buf);
                    let cmd = (word & 0xFF) as u8;
                    self.on_brom_cmd_after_echo(cmd)?
                }
            }
            State::SendDaArgs { idx, mut buf, n } => {
                buf[n] = b;
                if n + 1 < 4 {
                    State::SendDaArgs { idx, buf, n: n + 1 }
                } else {
                    // Echo the argument
                    self.push(&buf);
                    let val = u32::from_be_bytes(buf);
                    match idx {
                        0 => {
                            self.send_addr = val;
                            State::SendDaArgs {
                                idx: 1,
                                buf: [0; 4],
                                n: 0,
                            }
                        }
                        1 => {
                            self.send_size = val as usize;
                            State::SendDaArgs {
                                idx: 2,
                                buf: [0; 4],
                                n: 0,
                            }
                        }
                        _ => {
                            self.send_siglen = val;
                            // Reject if SLA has not been bypassed
                            let status = if self.profile.sla_required && !self.bypassed {
                                wire::BROM_STATUS_SLA_REQUIRED
                            } else {
                                0
                            };
                            self.push(&status.to_be_bytes());
                            self.ev(format!(
                                "SEND_DA: addr=0x{:08X} size={} siglen={} status=0x{:04X}",
                                self.send_addr, self.send_size, self.send_siglen, status
                            ));
                            if status != 0 {
                                State::Brom
                            } else {
                                self.da_buf.clear();
                                State::SendDaData
                            }
                        }
                    }
                }
            }
            State::SendDaData => {
                self.da_buf.push(b);
                if self.da_buf.len() >= self.send_size {
                    let sum: u32 = self.da_buf.iter().map(|x| *x as u32).sum::<u32>() & 0xFFFF;
                    self.push(&(sum as u16).to_be_bytes());
                    self.push(&0u16.to_be_bytes());
                    self.ev(format!(
                        "SEND_DA: 收到 {} 字节, checksum=0x{:04X}",
                        self.da_buf.len(),
                        sum
                    ));
                    self.da_stage = self.da_stage.saturating_add(1);
                    State::Brom
                } else {
                    State::SendDaData
                }
            }
            State::JumpEcho { mut buf, n } => {
                buf[n] = b;
                if n + 1 < 4 {
                    State::JumpEcho { buf, n: n + 1 }
                } else {
                    self.push(&buf);
                    State::JumpTrigger { buf: [0; 4], n: 0 }
                }
            }
            State::JumpTrigger { mut buf, n } => {
                buf[n] = b;
                if n + 1 < 4 {
                    State::JumpTrigger { buf, n: n + 1 }
                } else {
                    let addr = u32::from_be_bytes(buf);
                    self.ev(format!("JUMP_DA: 0x{:08X}", addr));
                    self.push(&[wire::BROM_JUMP_ACK]);
                    // DA started → send SYNC
                    self.push(&wire::XFLASH_SYNC_SIGNAL.to_le_bytes());
                    self.ev("DA: SYNC 已发送");
                    State::DaHeader { buf: [0; 12], n: 0 }
                }
            }
            State::DaHeader { mut buf, n } => {
                buf[n] = b;
                if n + 1 < wire::XFLASH_HEADER_LEN {
                    State::DaHeader { buf, n: n + 1 }
                } else {
                    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
                    let dtype = u32::from_le_bytes(buf[4..8].try_into().unwrap());
                    let len = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;
                    if magic != wire::XFLASH_MAGIC {
                        self.ev(format!("DA: 包头 magic 错误 0x{:08X}", magic));
                        // Resync: discard until the next magic (simplified: return to header state)
                        return Ok(());
                    }
                    let _ = dtype;
                    if len > 1024 * 1024 {
                        self.ev(format!("DA: 包长异常 {}", len));
                        return Ok(());
                    }
                    State::DaPayload {
                        len,
                        buf: Vec::with_capacity(len),
                    }
                }
            }
            State::ExploitId => {
                let id = b as usize;
                let accepted = id < SIM_EXPLOITS.len()
                    && self.profile.exploits.iter().any(|n| n == SIM_EXPLOITS[id]);
                let status = if accepted {
                    self.bypassed = true;
                    self.ev(format!("SIM exploit 接受: {}", SIM_EXPLOITS[id]));
                    0u8
                } else {
                    self.ev(format!("SIM exploit 拒绝: id={}", b));
                    1u8
                };
                self.push(&[wire::SIM_EXPLOIT_CMD, b, status]);
                State::Brom
            }
            State::DaPayload { len, mut buf } => {
                buf.push(b);
                if buf.len() >= len {
                    self.on_da_payload(&buf)?;
                    State::DaHeader { buf: [0; 12], n: 0 }
                } else {
                    State::DaPayload { len, buf }
                }
            }
        };
        Ok(())
    }

    fn on_brom_cmd(&mut self, b: u8) -> Result<State, PortError> {
        // Single-byte command: echo first, then respond
        self.push(&[b]);
        self.on_brom_cmd_after_echo(b)
    }

    fn on_brom_cmd_after_echo(&mut self, cmd: u8) -> Result<State, PortError> {
        Ok(match cmd {
            wire::BROM_GET_HW_CODE => {
                let mut resp = Vec::new();
                resp.extend_from_slice(&self.profile.hw_code.to_be_bytes());
                resp.extend_from_slice(&self.profile.hw_sub.to_be_bytes());
                self.push(&resp);
                self.ev(format!(
                    "GET_HW_CODE: 0x{:04X}/0x{:04X}",
                    self.profile.hw_code, self.profile.hw_sub
                ));
                State::Brom
            }
            wire::BROM_GET_TARGET_CONFIG => {
                let mut resp = Vec::new();
                resp.extend_from_slice(&self.profile.safety.to_be_bytes());
                resp.extend_from_slice(&0u16.to_be_bytes());
                self.push(&resp);
                self.ev(format!("GET_TARGET_CONFIG: 0x{:08X}", self.profile.safety));
                State::Brom
            }
            wire::BROM_SYNC => {
                let mut resp = Vec::new();
                resp.extend_from_slice(&self.profile.hw_ver.to_be_bytes());
                resp.extend_from_slice(&self.profile.sw_ver.to_be_bytes());
                resp.extend_from_slice(&0u16.to_be_bytes());
                resp.extend_from_slice(&[0x01, 0x00]);
                self.push(&resp);
                self.ev("SYNC(0xFC): ok");
                State::Brom
            }
            0xFE => {
                self.push(&[wire::BROM_SYNC_FE_ACK]);
                State::Brom
            }
            0xFF => State::Brom,
            wire::BROM_SEND_DA => State::SendDaArgs {
                idx: 0,
                buf: [0; 4],
                n: 0,
            },
            wire::BROM_JUMP_DA => State::JumpEcho { buf: [0; 4], n: 0 },
            // SIM: exploit handshake `0x1F <id>`, id agreed between host and device
            0xD6 => {
                self.push(&[0x00, 0x00]);
                self.push(&[0x00, 0x00]);
                State::Brom
            }
            other => {
                self.ev(format!("BROM: 未支持命令 0x{:02X}", other));
                State::Brom
            }
        })
    }

    fn on_da_payload(&mut self, payload: &[u8]) -> Result<(), PortError> {
        // ── Write transaction: three parameter packets [0u32]`checksum``data` form one block, with one status at block end
        if self.write.is_some() {
            let done = self.on_write_param(payload)?;
            if done {
                return Ok(());
            }
        }
        // ── FORMAT two-step ack
        if self.format_stage > 0 && payload.len() == 4 {
            self.on_format_ack(payload)?;
            return Ok(());
        }
        // Parameter block (length 56, or immediately following a command that expects parameters)
        if let Some(cmd) = self.pending_cmd {
            if payload.len() != 4 || !is_command_word(payload) {
                self.on_da_param(cmd, payload)?;
                self.pending_cmd = None;
                return Ok(());
            }
            // Length 4 and a known command → still handle as a command (drop the pending parameter to avoid getting stuck)
            self.pending_cmd = None;
        }
        if payload.len() != 4 {
            self.ev(format!("DA: 未知负载 len={}", payload.len()));
            self.push_status(0);
            return Ok(());
        }
        let cmd = u32::from_le_bytes(payload[0..4].try_into().unwrap());
        self.ev(format!("DA cmd 0x{:06X}", cmd));
        match cmd {
            wire::XFLASH_CMD_READ_DATA => {
                self.pending_cmd = Some(cmd);
                self.push_status(0);
            }
            wire::XFLASH_CMD_DEVICE_CTRL => {
                self.push_status(0);
            }
            wire::XFLASH_CMD_EMMC_INFO => {
                if self.profile.emmc_info {
                    self.push_status(0);
                    let info = self.emmc_info_payload();
                    self.push_packet(wire::XFLASH_TYPE_FLOW, &info);
                    self.push_status(0);
                } else {
                    self.push_status(wire::XFLASH_STATUS_UNSUPPORTED);
                }
            }
            wire::XFLASH_CMD_GET_DA_VER => {
                self.push_status(0);
                self.push_packet(wire::XFLASH_TYPE_FLOW, &0x0000_0107u32.to_le_bytes());
                self.push_status(0);
            }
            wire::XFLASH_DEVCTRL_ACK => {
                self.push_status(0);
                self.push_packet(
                    wire::XFLASH_TYPE_FLOW,
                    &wire::XFLASH_ACK_MAGIC.to_le_bytes(),
                );
                self.push_status(0);
            }
            wire::XFLASH_DEVCTRL_SET_STORAGE => {
                self.pending_cmd = Some(cmd);
                self.push_status(0);
            }
            wire::XFLASH_CMD_WRITE_DATA => {
                self.pending_cmd = Some(cmd);
                self.push_status(0);
            }
            wire::XFLASH_CMD_FORMAT => {
                self.pending_cmd = Some(cmd);
                self.push_status(0);
            }
            wire::XFLASH_DEVCTRL_CC_OPTIONAL_DOWNLOAD_ACT => {
                self.push_status(0);
            }
            other => {
                self.ev(format!("DA: 不支持命令 0x{:06X}", other));
                self.push_status(wire::XFLASH_STATUS_UNSUPPORTED);
            }
        }
        Ok(())
    }

    fn on_da_param(&mut self, cmd: u32, param: &[u8]) -> Result<(), PortError> {
        match cmd {
            wire::XFLASH_CMD_READ_DATA => {
                if param.len() < 24 {
                    self.push_status(1);
                    return Ok(());
                }
                let parttype = u32::from_le_bytes(param[4..8].try_into().unwrap());
                let addr = u64::from_le_bytes(param[8..16].try_into().unwrap());
                let size = u64::from_le_bytes(param[16..24].try_into().unwrap());
                self.pending_read = Some((addr, size));
                self.ev(format!(
                    "READ_DATA: addr=0x{:X} size={} parttype={}",
                    addr, size, parttype
                ));
                self.push_status(0);
                let data = self.read_window(addr, size);
                self.push_packet(wire::XFLASH_TYPE_FLOW, &data);
                self.push_status(0);
            }
            wire::XFLASH_DEVCTRL_SET_STORAGE => {
                let t = if param.len() >= 4 {
                    u32::from_le_bytes(param[0..4].try_into().unwrap())
                } else {
                    0
                };
                self.ev(format!("SET_STORAGE: type={}", t));
                self.push_status(0);
            }
            wire::XFLASH_CMD_WRITE_DATA => {
                if param.len() < 24 {
                    self.push_status(1);
                    return Ok(());
                }
                let parttype = u32::from_le_bytes(param[4..8].try_into().unwrap());
                let addr = u64::from_le_bytes(param[8..16].try_into().unwrap());
                let size = u64::from_le_bytes(param[16..24].try_into().unwrap());
                self.ev(format!(
                    "WRITE_DATA: addr=0x{:X} size={} parttype={}",
                    addr, size, parttype
                ));
                self.write = Some(WriteSession {
                    addr,
                    size,
                    written: 0,
                    stage: 0,
                    checksum: 0,
                });
                self.push_status(0);
            }
            wire::XFLASH_CMD_FORMAT => {
                if param.len() < 24 {
                    self.push_status(1);
                    return Ok(());
                }
                let addr = u64::from_le_bytes(param[8..16].try_into().unwrap());
                let size = u64::from_le_bytes(param[16..24].try_into().unwrap());
                self.erase_range(addr, size);
                self.ev(format!("FORMAT: addr=0x{:X} size={}", addr, size));
                self.push_status(0);
                // Device-side time consumption: CONTINUE + wait milliseconds, then send COMPLETE after host ack
                self.push_status(wire::XFLASH_STATUS_CONTINUE);
                self.push_status(2);
                self.format_stage = 1;
            }
            other => {
                self.ev(format!("DA: 命令 0x{:06X} 不接受参数", other));
                self.push_status(wire::XFLASH_STATUS_UNSUPPORTED);
            }
        }
        Ok(())
    }

    /// Read a flash range: check sparse pages (written/erased content) first, then fall back to the GPT image.
    fn read_window(&self, addr: u64, size: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(size as usize);
        let mut pos = addr;
        let end = addr + size;
        while pos < end {
            let page = pos / PAGE;
            let off = (pos % PAGE) as usize;
            let take = ((PAGE - off as u64).min(end - pos)) as usize;
            let buf = self.page_bytes(page);
            out.extend_from_slice(&buf[off..off + take]);
            pos += take as u64;
        }
        out
    }

    /// Whether the address falls within an erased range.
    fn is_erased(&self, addr: u64) -> bool {
        self.erased.iter().any(|(s, e)| addr >= *s && addr < *e)
    }

    /// Get the current content of one page (4 KiB): sparse page first → erased range (0xFF) → GPT image → 0x00.
    fn page_bytes(&self, page: u64) -> Vec<u8> {
        if let Some(v) = self.pages.get(&page) {
            return v.clone();
        }
        let mut v = vec![0u8; PAGE as usize];
        if self.is_erased(page * PAGE) {
            v.fill(0xFF);
            return v;
        }
        let base = page * PAGE;
        if base < self.image.len() as u64 {
            let end = (base + PAGE).min(self.image.len() as u64);
            v[..(end - base) as usize].copy_from_slice(&self.image[base as usize..end as usize]);
        }
        v
    }

    /// Write data into sparse pages (automatic read-modify-write, keeping other bytes in the page intact).
    fn write_range(&mut self, addr: u64, data: &[u8]) {
        let mut pos = addr;
        let end = addr + data.len() as u64;
        let mut src = 0usize;
        while pos < end {
            let page = pos / PAGE;
            let off = (pos % PAGE) as usize;
            let take = ((PAGE - off as u64).min(end - pos)) as usize;
            let mut buf = self.page_bytes(page);
            buf[off..off + take].copy_from_slice(&data[src..src + take]);
            self.pages.insert(page, buf);
            pos += take as u64;
            src += take;
        }
    }

    /// Erase a range: real hardware reads back 0xFF after erase.
    ///
    /// First drop any written pages in the range, then record one erased range (merging adjacent ranges),
    /// so erasing 1 GiB takes only a few records instead of 260,000 4 KiB pages.
    fn erase_range(&mut self, addr: u64, size: u64) {
        let end = addr + size;
        let (p0, p1) = (addr / PAGE, end.div_ceil(PAGE));
        self.pages.retain(|p, _| *p < p0 || *p >= p1);
        self.erased.push((addr, end));
        // Merge adjacent/overlapping ranges
        self.erased.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.erased.len());
        for (s, e) in self.erased.drain(..) {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.erased = merged;
    }

    /// Total number of bytes written (for diagnostics).
    #[allow(dead_code)]
    pub fn written_bytes(&self) -> u64 {
        self.total_written
    }

    /// Write transaction: handle the three parameter packets `[0u32][checksum][data]`.
    /// Returns Ok(true) if the payload was consumed by the write transaction.
    fn on_write_param(&mut self, payload: &[u8]) -> Result<bool, PortError> {
        let Some(mut ws) = self.write.take() else {
            return Ok(false);
        };
        match ws.stage {
            0 => {
                if payload.len() == 4 && payload.iter().all(|b| *b == 0) {
                    ws.stage = 1;
                    self.write = Some(ws);
                    return Ok(true);
                }
            }
            1 => {
                if payload.len() == 4 {
                    ws.checksum = u32::from_le_bytes(payload[..4].try_into().unwrap());
                    ws.stage = 2;
                    self.write = Some(ws);
                    return Ok(true);
                }
            }
            _ => {
                if payload.len() % 512 != 0 {
                    self.ev(format!(
                        "WRITE_DATA: 分块未对齐 512（{} 字节）",
                        payload.len()
                    ));
                    self.push_status(wire::XFLASH_STATUS_CHECKSUM);
                    return Ok(true);
                }
                // Checksum (aligned with mtkclient: sum(data) & 0xFFFF)
                let sum = (payload.iter().map(|b| *b as u32).sum::<u32>()) & 0xFFFF;
                if sum != ws.checksum {
                    self.ev(format!(
                        "WRITE_DATA: checksum 不匹配 期望 0x{:04X} 实际 0x{:04X}",
                        ws.checksum, sum
                    ));
                    self.push_status(wire::XFLASH_STATUS_CHECKSUM);
                    return Ok(true);
                }
                // Mid-write fault injection
                self.total_written += payload.len() as u64;
                let want_dead = self
                    .faults
                    .iter()
                    .find_map(|(_, k)| match k {
                        DeviceFault::WriteDead(at) if self.total_written >= *at => Some(true),
                        _ => None,
                    })
                    .unwrap_or(false);
                if want_dead {
                    self.dead = true;
                    self.ev(format!("fault: write dead at {} bytes", self.total_written));
                    return Err(PortError::Disconnected);
                }
                let want_silent = self.faults.iter().any(
                    |(_, k)| matches!(k, DeviceFault::WriteSilent(at) if self.total_written >= *at),
                );
                if want_silent {
                    self.silent = true;
                    self.ev(format!(
                        "fault: write silent at {} bytes",
                        self.total_written
                    ));
                    return Ok(true);
                }

                // Chaos: bit flip. Deliberately placed after the checksum passes — simulates "transferred correctly, corrupted on disk",
                // so only host-side read-back verification / CRC can catch it; a tool that silently accepts is exposed by this injection.
                match self.chaos.as_mut().and_then(|c| c.dirty(payload.len())) {
                    Some((idx, bit)) => {
                        let mut dirty = payload.to_vec();
                        dirty[idx] ^= 1 << bit;
                        self.ev(format!("chaos: bitflip @写块偏移 {} 位 {}", idx, bit));
                        self.write_range(ws.addr + ws.written, &dirty);
                    }
                    None => self.write_range(ws.addr + ws.written, payload),
                }
                ws.written += payload.len() as u64;
                ws.stage = 0;
                self.push_status(0);
                if ws.written >= ws.size {
                    // Transaction-final status (read by the host end_write)
                    self.push_status(0);
                    self.ev(format!(
                        "WRITE_DATA 完成: 0x{:X} + {} 字节",
                        ws.addr, ws.written
                    ));
                    self.write = None;
                } else {
                    self.write = Some(ws);
                }
                return Ok(true);
            }
        }
        // Unexpected: reject the payload and terminate the write transaction
        self.ev(format!(
            "WRITE_DATA: 非预期参数 len={} stage={}",
            payload.len(),
            ws.stage
        ));
        self.push_status(wire::XFLASH_STATUS_CHECKSUM);
        Ok(true)
    }

    /// FORMAT ack handling: first ack → COMPLETE; second ack → final 0.
    fn on_format_ack(&mut self, payload: &[u8]) -> Result<(), PortError> {
        if payload.len() != 4 {
            return Ok(());
        }
        match self.format_stage {
            1 => {
                self.push_status(wire::XFLASH_STATUS_COMPLETE);
                self.format_stage = 2;
            }
            _ => {
                self.push_status(0);
                self.format_stage = 0;
                self.ev("FORMAT 完成");
            }
        }
        Ok(())
    }

    fn emmc_info_payload(&self) -> Vec<u8> {
        // 104 bytes, strictly aligned with mtkclient `get_emmc_info` parsing order:
        //   0 type(4) | 4 block(4) | 8 boot1(8) | 16 boot2(8) | 24 rpmb(8)
        //   32 gp1 | 40 gp2 | 48 gp3 | 56 gp4 | **64 user(8)** | 72 cid(16) | 88 fwver(8)
        // Note: user_size is at 0x40, not 0x20 (0x20 is gp1),
        // early implementations wrote both at 0x20, "self-consistent but not matching real hardware".
        let mut v = vec![0u8; 104];
        v[0..4].copy_from_slice(&self.profile.storage.emmc_type.to_le_bytes());
        v[4..8].copy_from_slice(&self.profile.storage.block.to_le_bytes());
        v[8..16].copy_from_slice(&self.profile.storage.boot1.to_le_bytes());
        v[16..24].copy_from_slice(&self.profile.storage.boot2.to_le_bytes());
        v[24..32].copy_from_slice(&self.profile.storage.rpmb.to_le_bytes());
        v[64..72].copy_from_slice(&self.profile.storage.user.to_le_bytes());
        v
    }
}

fn is_command_word(payload: &[u8]) -> bool {
    if payload.len() != 4 {
        return false;
    }
    let cmd = u32::from_le_bytes(payload[0..4].try_into().unwrap());
    matches!(
        cmd,
        wire::XFLASH_CMD_READ_DATA
            | wire::XFLASH_CMD_DEVICE_CTRL
            | wire::XFLASH_CMD_EMMC_INFO
            | wire::XFLASH_CMD_GET_DA_VER
            | wire::XFLASH_DEVCTRL_ACK
            | wire::XFLASH_DEVCTRL_SET_STORAGE
            | wire::XFLASH_CMD_WRITE_DATA
            | wire::XFLASH_CMD_FORMAT
            | wire::XFLASH_DEVCTRL_CC_OPTIONAL_DOWNLOAD_ACT
    )
}

impl StreamPort for VirtualDevice {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError> {
        if self.dead {
            return Err(PortError::Disconnected);
        }
        if self.silent {
            // Silence = cable unplugged, semantically equivalent to "disconnected": fail immediately so the upper layer triggers resume / prompts replug,
            // it must not be treated as a retryable transient timeout (otherwise it would retry endlessly on a dead device and wait in vain).
            return Err(PortError::Disconnected);
        }
        // Chaos: transient timeout (not latched, recovers on next read) / silence (latched until replug).
        let mut go_silent = false;
        if let Some(c) = self.chaos.as_mut() {
            if c.rng.hit(c.cfg.glitch_permille) {
                c.glitches += 1;
                return Err(PortError::Timeout);
            }
            if c.rng.hit(c.cfg.silence_permille) {
                c.silences += 1;
                go_silent = true;
            }
        }
        if go_silent {
            self.silent = true;
            return Err(PortError::Disconnected);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        if self.out.is_empty() {
            return Err(PortError::Timeout);
        }
        let m = buf.len().min(self.out.len());
        // Chaos: packet drop — m bytes are still "sent on the wire", but the last d bytes are lost and the host receives only deliver bytes.
        //
        // Only applies to data blocks (≥16 bytes): a real USB short packet does not "drop trailing bytes"; control frames
        // (handshake / command echo / status word, 1~8 bytes) losing bytes would only make the host wait forever,
        // which is physically unrealistic. Packet drop models a large data transfer being truncated by a bad cable / interference.
        let mut deliver = m;
        if let Some(c) = self.chaos.as_mut() {
            if m >= 16 && c.rng.hit(c.cfg.drop_permille) {
                c.drops += 1;
                let d = 1 + (c.rng.next_u64() as usize % (m - 1));
                deliver = m - d;
            }
        }
        for (i, b) in self.out.drain(..m).enumerate() {
            if i < deliver {
                buf[i] = b;
            }
        }
        Ok(deliver)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, PortError> {
        for &b in buf {
            self.feed(b)?;
        }
        if self.dead {
            return Err(PortError::Disconnected);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), PortError> {
        Ok(())
    }

    /// Discard unconsumed input bytes (resync after frame misalignment).
    fn drain(&mut self) -> Result<(), PortError> {
        self.out.clear();
        Ok(())
    }
}

// =============================================================================
// GPT image generation
// =============================================================================

const GPT_ENTRIES: u32 = 128;
const GPT_ENTRY_SIZE: u32 = 128;
/// Type GUID for every partition (Linux filesystem data, a fixed value suffices)
const TYPE_GUID: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Generate initial content for each partition (stored sparsely by 4 KiB page).
///
/// Only partitions "with content" occupy pages: the blank area outside the GPT reads back as 0x00 and does not waste memory.
fn build_pages(profile: &DeviceProfile) -> std::collections::HashMap<u64, Vec<u8>> {
    use std::collections::HashMap;
    let mut pages: HashMap<u64, Vec<u8>> = HashMap::new();

    let mut put = |addr: u64, data: &[u8]| {
        let mut pos = addr;
        let end = addr + data.len() as u64;
        let mut src = 0usize;
        while pos < end {
            let page = pos / PAGE;
            let off = (pos % PAGE) as usize;
            let take = ((PAGE - off as u64).min(end - pos)) as usize;
            let buf = pages
                .entry(page)
                .or_insert_with(|| vec![0u8; PAGE as usize]);
            buf[off..off + take].copy_from_slice(&data[src..src + take]);
            pos += take as u64;
            src += take;
        }
    };

    for (name, start_lba, size) in &profile.partitions {
        let addr = start_lba * 512;
        match name.as_str() {
            // seccfg: real V4 structure (28-byte header + 32-byte hash), for unlock tests
            "seccfg" => {
                if profile.seccfg_v3 {
                    // V3: AND_SECCFG_v header + software SEJ encrypted section, end-to-end verification of unlock across all versions
                    let img = crate::security::seccfg::v3::synthetic_v3_image(
                        profile.seccfg_lock_state == 1,
                    );
                    put(addr, &img);
                } else {
                    let mut hdr = [0u8; 28];
                    hdr[0..4].copy_from_slice(&0x4D4D_4D4Du32.to_le_bytes());
                    hdr[4..8].copy_from_slice(&4u32.to_le_bytes());
                    hdr[8..12].copy_from_slice(&0x3Cu32.to_le_bytes());
                    hdr[12..16].copy_from_slice(&profile.seccfg_lock_state.to_le_bytes());
                    hdr[16..20].copy_from_slice(&profile.seccfg_dm_verity.to_le_bytes());
                    hdr[20..24].copy_from_slice(&0u32.to_le_bytes()); // sboot_runtime
                    hdr[24..28].copy_from_slice(&0x4545_4545u32.to_le_bytes());
                    let digest = crate::sha::Sha256::digest(hdr);
                    let mut img = hdr.to_vec();
                    img.extend_from_slice(&digest);
                    put(addr, &img);
                }
            }
            // vbmeta / vbmeta_system_*: valid AVB image (flags=0, includes boot descriptor)
            n if n.starts_with("vbmeta") => {
                let img = crate::security::avb::synthetic_vbmeta(0, &["boot"]);
                put(addr, &img);
            }
            // misc: boot_control structure ("BOOT" at offset 0x2000 + active slot)
            "misc" => {
                let mut img = vec![0u8; 0x3000];
                if profile.bootctrl {
                    img[0x2000..0x2004].copy_from_slice(b"BOOT");
                    let slot = match profile.slot {
                        Some('b') => 1u32,
                        _ => 0u32,
                    };
                    img[0x2008..0x200C].copy_from_slice(&slot.to_le_bytes());
                }
                put(addr, &img);
            }
            // Other partitions: write recognizable initial content (first 64 KiB) to verify read/write round-trips
            _ => {
                let len = (*size).min(64 * 1024) as usize;
                let mut img = vec![0u8; len];
                let seed = name.bytes().fold(0u8, |a, b| a.wrapping_add(b));
                for (i, b) in img.iter_mut().enumerate() {
                    *b = seed.wrapping_add(i as u8);
                }
                put(addr, &img);
            }
        }
    }
    pages
}

fn build_image(profile: &DeviceProfile) -> Vec<u8> {
    let mut img = vec![0u8; IMAGE_LEN];
    // LBA0: protective MBR
    img[0x1BE] = 0x00; // not bootable
    img[0x1C2] = 0xEE; // type
    img[0x1C6..0x1CA].copy_from_slice(&1u32.to_le_bytes());
    let total_lbas = (profile.storage.user / 512).max(0x100000);
    img[0x1CA..0x1CE]
        .copy_from_slice(&((total_lbas - 1).min(u32::MAX as u64) as u32).to_le_bytes());
    img[0x1FE] = 0x55;
    img[0x1FF] = 0xAA;

    // Partition entry array (starting at LBA2, 128*128)
    let entries_off = 1024usize;
    for (i, (name, start, size)) in profile.partitions.iter().enumerate() {
        let e = entries_off + i * GPT_ENTRY_SIZE as usize;
        img[e..e + 16].copy_from_slice(&TYPE_GUID);
        let mut guid = [0u8; 16];
        guid[0] = (i + 1) as u8;
        guid[15] = 0x42;
        img[e + 16..e + 32].copy_from_slice(&guid);
        let first = *start;
        let last = first + size / 512 - 1;
        img[e + 32..e + 40].copy_from_slice(&first.to_le_bytes());
        img[e + 40..e + 48].copy_from_slice(&last.to_le_bytes());
        // Name: UTF-16LE, up to 36 characters
        for (j, ch) in name.encode_utf16().take(35).enumerate() {
            img[e + 56 + j * 2..e + 58 + j * 2].copy_from_slice(&ch.to_le_bytes());
        }
    }
    let entries_len = (GPT_ENTRIES * GPT_ENTRY_SIZE) as usize;
    let entries_crc = crc32(&img[entries_off..entries_off + entries_len]);

    // LBA1: primary GPT header
    let h = 512usize;
    img[h..h + 8].copy_from_slice(b"EFI PART");
    img[h + 8..h + 12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    img[h + 12..h + 16].copy_from_slice(&92u32.to_le_bytes());
    img[h + 24..h + 32].copy_from_slice(&1u64.to_le_bytes()); // MyLBA
    img[h + 32..h + 40].copy_from_slice(&(total_lbas - 1).to_le_bytes()); // AltLBA
    img[h + 40..h + 48].copy_from_slice(&34u64.to_le_bytes()); // FirstUsable
    img[h + 48..h + 56].copy_from_slice(&(total_lbas - 34).to_le_bytes()); // LastUsable
    img[h + 56..h + 72].copy_from_slice(&[0x11; 16]); // DiskGUID
    img[h + 72..h + 80].copy_from_slice(&2u64.to_le_bytes()); // PartEntryLBA
    img[h + 80..h + 84].copy_from_slice(&GPT_ENTRIES.to_le_bytes());
    img[h + 84..h + 88].copy_from_slice(&GPT_ENTRY_SIZE.to_le_bytes());
    img[h + 88..h + 92].copy_from_slice(&entries_crc.to_le_bytes());
    let hdr_crc = {
        let mut tmp = img[h..h + 92].to_vec();
        tmp[16..20].fill(0);
        crc32(&tmp)
    };
    img[h + 16..h + 20].copy_from_slice(&hdr_crc.to_le_bytes());
    img
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::partition::GptInfo;

    fn handshake(dev: &mut VirtualDevice) {
        for b in wire::BROM_HANDSHAKE {
            dev.write(&[b]).unwrap();
            let mut echo = [0u8; 1];
            dev.read_exact(&mut echo).unwrap();
            assert_eq!(echo[0], !b);
        }
    }

    #[test]
    fn handshake_echoes_inverted_bytes() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        handshake(&mut dev);
        assert!(dev.events.iter().any(|e| e.contains("handshake: ok")));
    }

    #[test]
    fn hw_code_and_target_config() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        handshake(&mut dev);
        dev.write(&[wire::BROM_GET_HW_CODE]).unwrap();
        let mut buf = [0u8; 5];
        dev.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], wire::BROM_GET_HW_CODE);
        assert_eq!(u16::from_be_bytes([buf[1], buf[2]]), 0x0707);

        dev.write(&[wire::BROM_GET_TARGET_CONFIG]).unwrap();
        let mut buf = [0u8; 7];
        dev.read_exact(&mut buf).unwrap();
        assert_eq!(u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]), 0);
    }

    #[test]
    fn gpt_image_parses_with_correct_crc() {
        let p = DeviceProfile::preset("v5-clean").unwrap();
        let img = build_image(&p);
        let info = GptInfo::parse(&img).expect("GPT 可解析");
        let report = info.crc_report().expect("CRC 可计算");
        assert!(report.header_ok(), "头 CRC 必须正确");
        assert!(report.partition_entries_ok(), "分区项 CRC 必须正确");
        let names: Vec<String> = info.partitions().iter().map(|e| e.name.clone()).collect();
        assert!(
            names.iter().any(|name| name == "super"),
            "实际: {:?}",
            names
        );
    }

    #[test]
    fn emmc_info_reports_user_at_0x40() {
        let dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let p = dev.emmc_info_payload();
        assert_eq!(p.len(), 104);
        assert_eq!(
            u64::from_le_bytes(p[0x40..0x48].try_into().unwrap()),
            dev.profile.storage.user
        );
    }

    #[test]
    fn write_with_bad_checksum_is_rejected() {
        use crate::da::engine::DaEngine;
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        e.load_da(&mut dev, &[0xAB; 256]).unwrap();
        e.begin_write(&mut dev, 0x100000, 512, 1, 8).unwrap();
        // Directly construct a chunk with a bad checksum
        let data = [0x11u8; 512];
        let bad = 0x1234u32.to_le_bytes();
        let zero = 0u32.to_le_bytes();
        let hdr = |len: usize| {
            let mut h = Vec::new();
            h.extend_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
            h.extend_from_slice(&wire::XFLASH_TYPE_FLOW.to_le_bytes());
            h.extend_from_slice(&(len as u32).to_le_bytes());
            h
        };
        let mut pkt = Vec::new();
        pkt.extend(hdr(4));
        pkt.extend(zero);
        pkt.extend(hdr(4));
        pkt.extend(bad);
        pkt.extend(hdr(data.len()));
        pkt.extend(data);
        dev.write(&pkt).unwrap();
        let mut st = [0u8; 16];
        let n = dev.read(&mut st).unwrap();
        // Response: 12-byte header + 4-byte status
        let status = u32::from_le_bytes(st[n - 4..n].try_into().unwrap());
        assert_eq!(status, wire::XFLASH_STATUS_CHECKSUM);
        assert!(
            dev.events.iter().any(|e| e.contains("checksum 不匹配")),
            "设备应记录校验和错误: {:?}",
            dev.events
        );
    }

    #[test]
    fn erase_sets_erased_range_without_materializing_pages() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let before = dev.pages.len();
        dev.erase_range(0, 64 * 1024 * 1024);
        assert!(dev.pages.len() <= before, "擦除不应物化页");
        let head = dev.read_window(0, 512);
        assert!(head.iter().all(|b| *b == 0xFF));
        // Idempotent: erasing again yields the same result
        dev.erase_range(0, 64 * 1024 * 1024);
        assert!(dev.read_window(0, 512).iter().all(|b| *b == 0xFF));
    }

    #[test]
    fn cve_20435_alias_maps_to_cve_20069_fields() {
        // The historically mistaken ID is a compatibility alias: parsing must land on the cve_20069_* fields.
        let p =
            DeviceProfile::from_text("cve_20435.affected=true\ncve_20435.patched=false\n").unwrap();
        assert!(p.cve_20069_affected);
        assert!(!p.cve_20069_patched);

        let p2 = DeviceProfile::from_text("cve_20069.patched=true\n").unwrap();
        assert!(p2.cve_20069_patched);
    }

    #[test]
    fn profile_text_roundtrip() {
        let text = "\
name=MT6765
hw_code=0x0707
da_code=0x6765
proto=v6
safety=0x6
sla=true
storage.user=0x10000000
partition=boot:34:0x2000000
fault=silent:512
exploit=kamakiri2
";
        let p = DeviceProfile::from_text(text).unwrap();
        assert_eq!(p.name, "MT6765");
        assert!(p.v6);
        assert!(p.sla_required);
        assert_eq!(p.safety, 6);
        assert_eq!(p.partitions.len(), 1);
        assert_eq!(p.faults, vec![(512, DeviceFault::Silent)]);
        assert!(DeviceProfile::preset("nope").is_none());
    }

    #[test]
    fn fault_injection_turns_silent_then_disconnects() {
        let mut p = DeviceProfile::preset("v5-clean").unwrap();
        p.faults = vec![(2, DeviceFault::Silent)];
        let mut dev = VirtualDevice::new(p);
        dev.write(&[0xA0, 0x0A, 0x50, 0x05]).unwrap();
        let mut b = [0u8; 1];
        // Latched silence = replug required, reported as "disconnected" (not retryable)
        assert_eq!(dev.read(&mut b), Err(PortError::Disconnected));
    }

    #[test]
    fn unknown_brom_command_is_recorded_not_fatal() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        handshake(&mut dev);
        dev.write(&[0x7E]).unwrap();
        assert!(dev.events.iter().any(|e| e.contains("未支持命令")));
    }

    /// The state file read/write layout must be strictly symmetric — cross-process resume depends entirely on it.
    #[test]
    fn state_roundtrip_preserves_pages_and_erased() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        dev.write_range(0x1000, &[0xAB; 8192]); // two pages
        dev.write_range(0x9000, &[0xCD; 4096]); // one page
        dev.erase_range(0x2_0000, 0x1_0000);
        dev.total_written = 0x1234;

        let path = std::env::temp_dir().join(format!("vdev-state-{}.bin", std::process::id()));
        let path = path.to_string_lossy().to_string();
        dev.save_state(&path).unwrap();

        let mut dst = VirtualDevice::from_preset("v5-clean").unwrap();
        dst.load_state(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(dst.pages, dev.pages); // even the preset built-in seed pages must match exactly
        assert_eq!(dst.total_written, 0x1234);
        assert_eq!(dst.erased, dev.erased);
        assert!(
            dev.erased
                .iter()
                .any(|(s, e)| *s <= 0x2_0000 && *e >= 0x3_0000)
        );
        // Content identical byte-for-byte (including untouched blank parts within pages)
        assert_eq!(dst.read_window(0x1000, 8192), vec![0xAB; 8192]);
        assert_eq!(dst.read_window(0x9000, 4096), vec![0xCD; 4096]);
        assert_eq!(dst.read_window(0x2_0000, 4096), vec![0xFF; 4096]);
    }

    #[test]
    fn state_load_rejects_bad_magic_and_version() {
        let dir = std::env::temp_dir();
        let bad = dir.join(format!("vdev-bad-{}.bin", std::process::id()));
        let bad = bad.to_string_lossy().to_string();

        std::fs::write(&bad, b"nope\0\0\0\0\0\0\0\0").unwrap();
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        assert!(dev.load_state(&bad).unwrap_err().contains("格式不匹配"));

        let mut v2 = Vec::from(*STATE_MAGIC);
        v2.extend_from_slice(&99u32.to_le_bytes());
        std::fs::write(&bad, &v2).unwrap();
        assert!(dev.load_state(&bad).unwrap_err().contains("版本不支持"));

        std::fs::write(&bad, &v2[..10]).unwrap();
        assert!(dev.load_state(&bad).unwrap_err().contains("格式不匹配"));
        let _ = std::fs::remove_file(&bad);
    }

    // ── Phase 1.1 Chaos injection ─────────────────────────────────────────

    fn chaos_state(cfg: ChaosConfig) -> ChaosState {
        ChaosState {
            cfg,
            rng: ChaosRng::new(cfg.seed),
            bitflips: 0,
            drops: 0,
            silences: 0,
            glitches: 0,
        }
    }

    #[test]
    fn chaos_rng_is_deterministic() {
        let mut a = ChaosRng::new(12345);
        let mut b = ChaosRng::new(12345);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64(), "同种子必须给出同一序列");
        }
        let mut c = ChaosRng::new(54321);
        assert_ne!(a.next_u64(), c.next_u64(), "不同种子应不同");
    }

    #[test]
    fn chaos_default_is_noop() {
        assert!(ChaosConfig::default().is_noop());
        let dev = VirtualDevice::from_preset("v5-clean")
            .unwrap()
            .with_chaos(ChaosConfig::default());
        assert!(dev.chaos.is_none(), "noop 配置不应启用混沌");
    }

    #[test]
    fn chaos_bitflip_primitive() {
        // 0‰ never flips
        let mut nil = chaos_state(ChaosConfig::default());
        assert_eq!(nil.dirty(64), None);

        // 1000‰ always flips, with index/bit within range
        let cfg = ChaosConfig {
            seed: 42,
            bitflip_permille: 1000,
            ..ChaosConfig::default()
        };
        let mut c = chaos_state(cfg);
        let (idx, bit) = c.dirty(64).expect("1000‰ 必须命中");
        assert!(idx < 64 && bit < 8, "翻转位置越界: ({}, {})", idx, bit);
        assert_eq!(c.bitflips, 1);
    }

    #[test]
    fn chaos_silence_latches_read() {
        let cfg = ChaosConfig {
            seed: 1,
            silence_permille: 1000,
            ..ChaosConfig::default()
        };
        let mut dev = VirtualDevice::from_preset("v5-clean")
            .unwrap()
            .with_chaos(cfg);
        dev.out.push_back(0x5A);
        let mut b = [0u8; 1];
        // Latched silence = replug required, reported as Disconnected (not retryable, fail immediately)
        assert_eq!(dev.read(&mut b), Err(PortError::Disconnected));
        assert!(dev.silent, "静默必须锁存（直到 replug）");
        assert_eq!(dev.read(&mut b), Err(PortError::Disconnected));
        // Recover after replug (and calm the environment to verify replug clears the latch flag, not the chaos config itself)
        dev.replug();
        dev.chaos.as_mut().unwrap().cfg.silence_permille = 0;
        dev.out.push_back(0x5A);
        let n = dev.read(&mut b).unwrap();
        assert_eq!((n, b[0]), (1, 0x5A), "重连后应恢复出字节");
    }

    #[test]
    fn chaos_glitch_is_transient_not_latched() {
        let cfg = ChaosConfig {
            seed: 1,
            glitch_permille: 1000,
            ..ChaosConfig::default()
        };
        let mut dev = VirtualDevice::from_preset("v5-clean")
            .unwrap()
            .with_chaos(cfg);
        dev.out.push_back(0x5A);
        let mut b = [0u8; 1];
        assert_eq!(dev.read(&mut b), Err(PortError::Timeout));
        assert!(!dev.silent, "瞬时超时不得锁存");
        assert_eq!(dev.chaos_counts().3, 1);
    }

    /// Phase 1.1b acceptance: real silence (link down) must fail **immediately**, never retry and wait in vain on a dead device.
    #[test]
    fn chaos_silence_fails_fast_without_retry() {
        use crate::da::engine::{DaEngine, EngineError};
        let cfg = ChaosConfig {
            seed: 1,
            silence_permille: 1000,
            ..ChaosConfig::default()
        };
        let mut dev = VirtualDevice::from_preset("v5-clean")
            .unwrap()
            .with_chaos(cfg);
        let mut e = DaEngine::new();
        let start = std::time::Instant::now();
        let err = e.connect(&mut dev).unwrap_err();
        assert!(
            matches!(err, EngineError::Port(PortError::Disconnected)),
            "静默应上报断开，实际 {:?}",
            err
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "静默必须立刻失败（不得退避重试）"
        );
    }

    /// Phase 1.1b acceptance: when a data block loses bytes (length mismatch), recover by clearing the input buffer + resending the
    /// whole READ_DATA command — byte-level resume cannot recover lost bytes.
    #[test]
    fn chaos_drop_recovers_via_read_command_retry() {
        use crate::da::engine::DaEngine;
        let cfg = ChaosConfig {
            seed: 3,
            drop_permille: 60,
            ..ChaosConfig::default()
        };
        let mut dev = VirtualDevice::from_preset("v5-clean")
            .unwrap()
            .with_chaos(cfg);
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        e.load_da(&mut dev, &[0xAB; 256]).unwrap();
        let data = e
            .read_flash(&mut dev, 0, 32768)
            .expect("丢包后应通过整条 READ_DATA 重试恢复");
        assert_eq!(data.len(), 32768);
        assert!(dev.chaos_counts().1 > 0, "本用例必须真的注入过丢包");
    }

    /// Phase 1.1b acceptance: chaos transient timeouts are absorbed by bounded retries; DA loading must not be interrupted by a single glitch.
    #[test]
    fn chaos_glitch_is_absorbed_by_bounded_retry() {
        use crate::da::engine::DaEngine;
        let cfg = ChaosConfig {
            seed: 7,
            glitch_permille: 30,
            ..ChaosConfig::default()
        };
        let mut dev = VirtualDevice::from_preset("v5-clean")
            .unwrap()
            .with_chaos(cfg);
        let mut e = DaEngine::new();
        e.connect(&mut dev).expect("握手应能扛住瞬时超时");
        e.load_da(&mut dev, &[0xAB; 256])
            .expect("DA 加载应能扛住瞬时超时");
        assert!(
            dev.chaos_counts().3 > 0,
            "本用例必须真的注入过瞬时超时，否则测不到重试"
        );
    }

    #[test]
    fn chaos_drop_truncates_read_and_loses_tail() {
        let cfg = ChaosConfig {
            seed: 3,
            drop_permille: 1000,
            ..ChaosConfig::default()
        };
        let mut dev = VirtualDevice::from_preset("v5-clean")
            .unwrap()
            .with_chaos(cfg);
        for i in 0..16u8 {
            dev.out.push_back(i);
        }
        let mut buf = [0u8; 16];
        let n = dev.read(&mut buf).unwrap();
        assert!(n >= 1 && n < 16, "丢包应截断返回，实际 {}", n);
        assert_eq!(dev.out.len(), 0, "尾部被丢的字节应真的丢弃（真丢包）");
        assert!(buf[..n].iter().enumerate().all(|(i, b)| *b == i as u8));
    }

    #[test]
    fn chaos_off_keeps_read_intact() {
        let mut dev = VirtualDevice::from_preset("v5-clean")
            .unwrap()
            .with_chaos(ChaosConfig::default());
        for i in 0..8u8 {
            dev.out.push_back(i);
        }
        let mut buf = [0u8; 8];
        let n = dev.read(&mut buf).unwrap();
        assert_eq!(n, 8);
        assert_eq!(buf, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(dev.chaos_counts(), (0, 0, 0, 0));
    }

    /// End-to-end: the bit flip happens after the checksum passes, corrupting data on disk,
    /// detectable only through host-side read-back verification — proving "silent corruption never passes as success".
    #[test]
    fn chaos_bitflip_corrupts_after_checksum_accepted() {
        use crate::da::engine::DaEngine;
        let cfg = ChaosConfig {
            seed: 9,
            bitflip_permille: 1000,
            ..ChaosConfig::default()
        };
        let mut dev = VirtualDevice::from_preset("v5-clean")
            .unwrap()
            .with_chaos(cfg);
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        e.load_da(&mut dev, &[0xAB; 256]).unwrap();
        e.begin_write(&mut dev, 0x100000, 512, 1, 8).unwrap();

        let data = [0x11u8; 512];
        let sum = (data.iter().map(|b| *b as u32).sum::<u32>()) & 0xFFFF;
        let hdr = |len: usize| {
            let mut h = Vec::new();
            h.extend_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
            h.extend_from_slice(&wire::XFLASH_TYPE_FLOW.to_le_bytes());
            h.extend_from_slice(&(len as u32).to_le_bytes());
            h
        };
        let mut pkt = Vec::new();
        pkt.extend(hdr(4));
        pkt.extend(0u32.to_le_bytes());
        pkt.extend(hdr(4));
        pkt.extend(sum.to_le_bytes());
        pkt.extend(hdr(data.len()));
        pkt.extend(data);
        dev.write(&pkt).unwrap();

        // The checksum itself passes (device returns status=0); the on-disk bit flip happens afterwards.
        let mut st = [0u8; 64];
        let n = dev.read(&mut st).unwrap();
        let status = u32::from_le_bytes(st[n - 4..n].try_into().unwrap());
        assert_eq!(status, 0, "校验和应被接受（翻转在之后）");

        let back = dev.read_window(0x100000, 512);
        assert_ne!(back, data.to_vec(), "位翻转必须让落盘数据与写入不一致");
        assert_eq!(dev.chaos_counts().0, 1, "应恰好翻转一次");
    }
}

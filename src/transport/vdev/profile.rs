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

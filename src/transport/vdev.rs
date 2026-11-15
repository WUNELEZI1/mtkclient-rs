//! 虚拟 MTK 设备（100% 离线仿真）。
//!
//! 这是**设备侧**实现：它接收主机发出的字节，按 BROM / XFlash 协议解析，
//! 并生成设备应答。配合 `da::engine`（主机侧状态机）即可在没有真机的情况下
//! 跑完 `连接 → DA 上传 → 开存储 → 读 GPT` 全流程。
//!
//! 设计取舍：
//! - 解析按**单字节喂入**，状态机状态显式保存，天然支持分片到达（模拟 USB 短包）；
//! - 设备画像（[`DeviceProfile`]）描述"这是一台什么机器"：芯片、安全标志、
//!   存储尺寸、分区布局、故障注入点；内置若干预设（见 [`preset`]）；
//! - GPT 由分区列表**现场生成**（含两处 CRC），因此 `GptInfo::parse` 的校验路径
//!   会被真实覆盖，不是写死的一段字节；
//! - exploit 无法在字节流里复现，用仿真专用命令 [`wire::SIM_EXPLOIT_CMD`] 表示
//!   "设备侧接受该 exploit"，用于验证编排链路（文档中已标注 sim-only）。

use std::collections::VecDeque;

use super::wire;
use super::{PortError, PortKind, StreamPort};

// =============================================================================
// 设备画像
// =============================================================================

/// 存储画像。
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

/// 故障注入（设备侧）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFault {
    /// 从第 N 个主机字节起，设备不再应答（主机侧表现为超时）
    Silent,
    /// 从第 N 个主机字节起，设备断开（主机侧表现为 Disconnected）
    Dead,
    /// 累计写入字节达到 N 时断开（模拟写入中途拔线，用于验证断点续传）
    WriteDead(u64),
    /// 累计写入字节达到 N 时静默（模拟写入卡死）
    WriteSilent(u64),
}

/// 混沌故障注入配置（`--sim-chaos`）。
///
/// 概率一律用**千分比**表达；`0` 表示关闭该项。默认全 0（最稳，行为与普通仿真完全一致）。
/// 用**固定种子**驱动确定性 PRNG，同一种子可完整复现同一条故障序列——这是让混沌
/// 既能"逼出极限"又能"可复现回归"的前提。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChaosConfig {
    pub seed: u64,
    /// 写入落盘时的位翻转概率（‰）：模拟闪存位衰变 / 传输噪声。
    /// 注意：翻转发生在**校验和已通过之后**，因此只能靠回读校验/CRC 抓到——
    /// 正是"静默损坏绝不能被当成成功"的防线。
    pub bitflip_permille: u32,
    /// 读回数据时截断本次返回的概率（‰）：模拟劣质线材丢字节（短包）。
    pub drop_permille: u32,
    /// 设备静默概率（‰）：触发后读超时，**直到重连才恢复**（模拟线缆脱落）。
    pub silence_permille: u32,
    /// 瞬时超时概率（‰）：单次读超时后自动恢复（模拟延迟毛刺 / 接触不良抖动）。
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
    /// `--sim-chaos` 的推荐档位：不激进，但足以逼出续传/校验缺陷。
    pub fn typical(seed: Option<u64>) -> Self {
        Self {
            seed: seed.unwrap_or(0x5EED_1234),
            bitflip_permille: 8,
            // 静默 = 线缆脱落，必须人工重插，不属于"可自动恢复"的压力档位；
            // 需要验证掉线/续传时用 `fault=silent` 或显式配置，不在默认档位随机触发。
            drop_permille: 8,
            silence_permille: 0,
            glitch_permille: 8,
        }
    }

    /// 是否等价于"不注入任何故障"。
    pub fn is_noop(&self) -> bool {
        self.bitflip_permille == 0
            && self.drop_permille == 0
            && self.silence_permille == 0
            && self.glitch_permille == 0
    }
}

/// SplitMix64：确定性 PRNG（零依赖，跨平台一致）。
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

    /// 命中概率 `p`（千分比）。
    fn hit(&mut self, p: u32) -> bool {
        p > 0 && (self.next_u64() % 1000) < p as u64
    }
}

/// 混沌运行态（配置 + PRNG + 计数）。
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
    /// 位翻转判定：命中则返回 (payload 内字节下标, 位号)。
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

/// 设备画像。
#[derive(Debug, Clone)]
pub struct DeviceProfile {
    pub name: String,
    pub hw_code: u16,
    pub hw_sub: u16,
    pub hw_ver: u16,
    pub sw_ver: u16,
    /// DA 头里声明的 da_code（0x6768 等）
    pub da_code: u16,
    /// true = V6(XML) 平台
    pub v6: bool,
    /// target config 原始位（安全标志）
    pub safety: u32,
    /// SEND_DA 返回 0x1D0D（SLA 拒绝）
    pub sla_required: bool,
    /// BROM 需要认证（拿不到 auth 时 BROM 路径不可达）
    pub brom_auth: bool,
    pub preloader_patched: bool,
    pub da_sig_failed: bool,
    pub storage: StorageProfile,
    /// (名称, 起始 LBA, 字节数)
    pub partitions: Vec<(String, u64, u64)>,
    pub faults: Vec<(usize, DeviceFault)>,
    /// 设备是否支持 eMMC 信息查询
    pub emmc_info: bool,
    /// 设备侧接受的 exploit 名称（仿真专用）
    pub exploits: Vec<String>,
    /// 当前活动 A/B 槽（None = 非 A/B 设备）
    pub slot: Option<char>,
    /// bootctrl 是否可读（false 时若 _a/_b 同时存在 → 槽位不可判定）
    pub bootctrl: bool,
    /// seccfg 分区的 lock_state（1=locked, 3=unlocked）
    pub seccfg_lock_state: u32,
    /// seccfg 的 dm_verity_state（0x10 偏移）
    pub seccfg_dm_verity: u32,
    /// DA1 是否已应用 Carbonara 加固（强制 DA2 加载地址为 0x40000000）
    pub da1_hardened: bool,
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
            da1_hardened: false,
        }
    }
}

impl DeviceProfile {
    /// 让 `storage.user` 至少覆盖最后一个分区（否则 GPT 里分区会超出盘尾，
    /// 与真机不符且容易掩盖边界 bug）。
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

    /// 转换为规划层的设备状态。
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
        }
    }

    /// 从 `key=value` 文本解析（每行一条，`#` 开头为注释）。
    ///
    /// 分区用 `partition=name:start_lba:size_bytes`，故障用 `fault=silent:24`。
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
                "da1_hardened" => p.da1_hardened = truthy(v),
                other => return Err(format!("未知配置项: {}", other)),
            }
        }
        if p.partitions.is_empty() {
            p.partitions = default_partitions();
        }
        p.fit_storage();
        Ok(p)
    }

    /// 内置预设（`--port mock://<预设名>`）。
    pub fn preset(name: &str) -> Option<Self> {
        let mut p = DeviceProfile::default();
        p.partitions = default_partitions();
        match name {
            // 干净的 V5 机器：无安全标志，直接 DA 加载即可读表（cost=690 路径）
            "v5-clean" => {
                p.name = "MT6768 (V5 clean)".into();
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // SLA 打开的 V5 机器：必须先绕过，否则 SEND_DA 返回 0x1D0D
            "v5-sla" => {
                p.name = "MT6768 (SLA enabled)".into();
                p.safety = 0x2; // SLA
                p.sla_required = true;
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // Carbonara 加固机：SLA 打开，但 DA1 已含补丁特征 → carbonara 不可用。
            // 用于验证"probe 返回 NotVulnerable 时 Planner 不规划 Carbonara 路径"。
            "carbonara-hardened" => {
                p.name = "MT6768 (Carbonara hardened)".into();
                p.safety = 0x2;
                p.sla_required = true;
                p.da1_hardened = true;
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // DAA + SBC 双开：只有 DA 层 patch 能过
            "v5-daa" => {
                p.name = "MT6785 (DAA+SBC)".into();
                p.hw_code = 0x0707;
                p.da_code = 0x6785;
                p.safety = 0x2 | 0x4; // SLA + DAA
                p.exploits = vec!["kamakiri2".into(), "carbonara".into()];
            }
            // V6 平台：BROM 不可达，只能走 DA
            "v6" => {
                p.name = "MT6983 (V6 XML)".into();
                p.hw_code = 0x0A0A;
                p.da_code = 0x6983;
                p.v6 = true;
                p.partitions = v6_partitions();
                p.exploits = vec!["carbonara".into()];
            }
            // UFS + A/B 布局
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
            // 传输不稳：第 400 个主机字节起设备静默（模拟线缆接触不良）
            "flaky" => {
                p.name = "MT6768 (flaky link)".into();
                p.faults = vec![(400, DeviceFault::Silent)];
                p.exploits = vec!["kamakiri2".into()];
            }
            // 半路掉线：DA 上传前断开
            "unplug" => {
                p.name = "MT6768 (unplugged)".into();
                p.faults = vec![(120, DeviceFault::Dead)];
            }
            // 写入中途拔线：写到 10MB 断开，用于验证断点续传
            "flaky-write" => {
                p.name = "MT6768 (write drops @10MB)".into();
                p.faults = vec![(0, DeviceFault::WriteDead(10 * 1024 * 1024))];
                p.exploits = vec!["kamakiri2".into()];
            }
            // A/B 混合布局 + bootctrl 不可读：槽位不可判定（vbmeta 必须报错而不是猜）
            "ab-unknown-slot" => {
                p.name = "MT6877 (A/B, bootctrl unreadable)".into();
                p.da_code = 0x6877;
                p.partitions = ab_partitions();
                p.slot = None;
                p.bootctrl = false;
                p.exploits = vec!["kamakiri2".into()];
            }
            // DA 拒绝服务（SEND_DA 状态错误）：用于验证错误传播
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

/// 按给定顺序从 LBA 34 起排布分区（1 MiB 对齐），返回 (名称, 起始 LBA, 字节数)。
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
// 虚拟设备
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// 握手：已收到 i 个字节
    Handshake(usize),
    /// BROM 单字节命令态
    Brom,
    /// 4 字节字命令（首字节为 0x00 时进入）
    BromWord { buf: [u8; 4], n: usize },
    /// SEND_DA 参数：0=addr 1=size 2=siglen
    SendDaArgs { idx: u8, buf: [u8; 4], n: usize },
    /// SEND_DA 数据体
    SendDaData,
    /// JUMP_DA 第一次地址回显
    JumpEcho { buf: [u8; 4], n: usize },
    /// JUMP_DA 第二次地址（触发跳转）
    JumpTrigger { buf: [u8; 4], n: usize },
    /// DA 包头
    DaHeader { buf: [u8; 12], n: usize },
    /// DA 负载
    DaPayload { len: usize, buf: Vec<u8> },
    /// 仿真专用 exploit 握手的 id 字节
    ExploitId,
}

/// 仿真设备认识的 exploit 名称（id 即下标）。
pub const SIM_EXPLOITS: [&str; 4] = ["kamakiri2", "carbonara", "heapbait", "hashimoto"];

/// exploit 名称 → id（未登记的名字返回 None）。
pub fn exploit_id(name: &str) -> Option<u8> {
    SIM_EXPLOITS
        .iter()
        .position(|n| *n == name)
        .map(|i| i as u8)
}

/// 虚拟设备。
pub struct VirtualDevice {
    pub profile: DeviceProfile,
    out: VecDeque<u8>,
    state: State,
    /// 已上传的 DA 段数（0 → 下一个是 DA1，1 → DA2）
    da_stage: u8,
    send_addr: u32,
    send_size: usize,
    send_siglen: u32,
    da_buf: Vec<u8>,
    /// 安全标志是否已被 exploit 清除
    bypassed: bool,
    /// 当前等待参数的 DA 命令
    pending_cmd: Option<u32>,
    /// 最近一次 READ_DATA 请求
    pending_read: Option<(u64, u64)>,
    /// 存储镜像（GPT 区域）
    image: Vec<u8>,
    /// 分区内容（按 4 KiB 页存，稀疏；未命中回落到 image / 0x00）
    pages: std::collections::HashMap<u64, Vec<u8>>,
    /// 已擦除区间（读回 0xFF）。用区间表示而不是逐页填充，
    /// 否则擦一个 1 GiB 的分区会直接吃掉 1 GiB 内存。
    erased: Vec<(u64, u64)>,
    /// 当前写入事务
    write: Option<WriteSession>,
    /// FORMAT 事务阶段（0=空闲 1=等第一个 ack 2=等收尾 ack）
    format_stage: u8,
    /// 本设备累计被写入的字节数（故障注入用）
    total_written: u64,
    faults: Vec<(usize, DeviceFault)>,
    host_bytes: usize,
    dead: bool,
    silent: bool,
    /// 设备侧事件日志（测试断言用）
    pub events: Vec<String>,
    /// 主机侧写入缓冲区（诊断用）
    pub written: Vec<u8>,
    /// 混沌注入（`--sim-chaos`）；`None` = 不注入，行为与普通仿真完全一致。
    chaos: Option<ChaosState>,
}

/// 镜像窗口大小：足够覆盖 LBA0.. 主 GPT + 分区项（32KB 读取足够，留余量）。
const IMAGE_LEN: usize = 1024 * 1024;

/// 存储页大小（稀疏存储粒度）。
const PAGE: u64 = 4096;
/// 仿真状态文件的魔数（8 字节，紧跟 u32 版本号）。
const STATE_MAGIC: &[u8; 8] = b"VDEV1\0\0\0";
/// 仿真状态文件布局版本；布局变更时递增，避免读到旧格式后静默错乱。
const STATE_VERSION: u32 = 1;

/// 一次写入事务（设备侧）。
#[derive(Debug)]
struct WriteSession {
    addr: u64,
    size: u64,
    written: u64,
    /// 0=期待 0u32 1=期待 checksum 2=期待数据
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

    /// 由预设名构建设备（测试与外部工具入口）。
    #[allow(dead_code)]
    pub fn from_preset(name: &str) -> Option<Self> {
        DeviceProfile::preset(name).map(Self::new)
    }

    /// 启用混沌注入（`--sim-chaos`）。配置为 noop 时等价于不启用。
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

    /// 混沌计数（位翻转, 丢包, 静默, 瞬时超时）——诊断/测试用。
    #[allow(dead_code)]
    pub fn chaos_counts(&self) -> (u64, u64, u64, u64) {
        match &self.chaos {
            Some(c) => (c.bitflips, c.drops, c.silences, c.glitches),
            None => (0, 0, 0, 0),
        }
    }

    /// 由画像文本构建设备（外部工具入口）。
    #[allow(dead_code)]
    pub fn profile_text(text: &str) -> Result<Self, String> {
        DeviceProfile::from_text(text).map(Self::new)
    }

    /// 保存 flash 状态（页 + 擦除区间）到文件，用于跨进程复现"拔插重连"。
    pub fn save_state(&self, path: &str) -> Result<(), String> {
        // 头部固定 12 字节：magic(8) + 版本(4)，与 `load_state` 的 p=12 严格对称
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

    /// 载入 flash 状态（`save_state` 的逆操作）。
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
        // 页数上限：防止损坏文件导致巨额预分配（4 GiB 的 4 KiB 页 = 1M，留 8 倍余量）
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

    /// 模拟"拔插一次"：清故障与协议状态，**保留 flash 内容**（写入/擦除结果不丢）。
    ///
    /// 断点续传的语义正是建立在这条假设上：重连后存储里的数据还在，
    /// 因此可以从 `.resume` 记录的偏移继续写。
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

    /// 单字节推进状态机。
    fn feed(&mut self, b: u8) -> Result<(), PortError> {
        self.host_bytes += 1;
        self.written.push(b);
        if self.dead {
            return Err(PortError::Disconnected);
        }
        // 故障注入：到点后静默/断开
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
                    // 回错字节，主机侧会重试
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
                // BROM sync FE 不回显命令本身，只回一个状态字节
                if b == 0xFE {
                    self.push(&[wire::BROM_SYNC_FE_ACK]);
                    State::Brom
                } else if b == wire::SIM_EXPLOIT_CMD {
                    // 仿真专用握手：命令本身不回显，应答为 [cmd, id, status]
                    State::ExploitId
                } else if b == 0x00 {
                    // 可能是 4 字节字命令（BE，高字节为 0）
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
                    // 回显整个字
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
                    // 回显参数
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
                            // SLA 未绕过时拒绝
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
                    // DA 启动 → 发 SYNC
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
                        // 重新同步：丢弃到下一个 magic（简化：回到头态）
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
        // 单字节命令：先回显，再给应答
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
            // SIM: exploit 握手 `0x1F <id>`，id 由主机与设备约定
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
        // ── 写入事务：三个参数包 [0u32][checksum][data] 为一块，块末回一个 status
        if self.write.is_some() {
            let done = self.on_write_param(payload)?;
            if done {
                return Ok(());
            }
        }
        // ── FORMAT 的两步 ack
        if self.format_stage > 0 && payload.len() == 4 {
            self.on_format_ack(payload)?;
            return Ok(());
        }
        // 参数块（长度 56 或紧跟"需要参数"的命令）
        if let Some(cmd) = self.pending_cmd {
            if payload.len() != 4 || !is_command_word(payload) {
                self.on_da_param(cmd, payload)?;
                self.pending_cmd = None;
                return Ok(());
            }
            // 长度 4 且是已知命令 → 仍按命令处理（放弃上一次待参数，避免卡死）
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
                // 设备侧耗时：CONTINUE + 等待毫秒，等主机 ack 后给 COMPLETE
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

    /// 读一段 flash：先查稀疏页（写入/擦除过的内容），再回落到 GPT 镜像。
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

    /// 该地址是否落在已擦除区间内。
    fn is_erased(&self, addr: u64) -> bool {
        self.erased.iter().any(|(s, e)| addr >= *s && addr < *e)
    }

    /// 取一页（4 KiB）当前内容：稀疏页优先 → 已擦除区间（0xFF）→ GPT 镜像 → 0x00。
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

    /// 写一段数据到稀疏页（自动读-改-写，保证页内其他字节不变）。
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

    /// 擦除一段区域：真机擦除后读回 0xFF。
    ///
    /// 先丢弃该区间内被写过的页，再记录一条擦除区间（合并相邻区间），
    /// 这样擦 1 GiB 也只占几条记录，而不是 26 万个 4 KiB 页。
    fn erase_range(&mut self, addr: u64, size: u64) {
        let end = addr + size;
        let (p0, p1) = (addr / PAGE, (end + PAGE - 1) / PAGE);
        self.pages.retain(|p, _| *p < p0 || *p >= p1);
        self.erased.push((addr, end));
        // 合并相邻/重叠区间
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

    /// 已写入的总字节数（诊断用）。
    #[allow(dead_code)]
    pub fn written_bytes(&self) -> u64 {
        self.total_written
    }

    /// 写入事务：处理 [0u32][checksum][data] 三个参数包。
    /// 返回 Ok(true) 表示该负载已被写入事务消费。
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
                // 校验和（对齐 mtkclient: sum(data) & 0xFFFF）
                let sum = (payload.iter().map(|b| *b as u32).sum::<u32>()) & 0xFFFF;
                if sum != ws.checksum {
                    self.ev(format!(
                        "WRITE_DATA: checksum 不匹配 期望 0x{:04X} 实际 0x{:04X}",
                        ws.checksum, sum
                    ));
                    self.push_status(wire::XFLASH_STATUS_CHECKSUM);
                    return Ok(true);
                }
                // 写入中途故障注入
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

                // 混沌：位翻转。故意放在校验和通过之后——模拟"传输正确、落盘损坏"，
                // 因此只能靠主机侧回读校验 / CRC 抓到；若工具静默接受即被此注入测出。
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
                    // 事务收尾 status（主机 end_write 会读它）
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
        // 不符合预期：拒绝该负载，终止写入事务
        self.ev(format!(
            "WRITE_DATA: 非预期参数 len={} stage={}",
            payload.len(),
            ws.stage
        ));
        self.push_status(wire::XFLASH_STATUS_CHECKSUM);
        Ok(true)
    }

    /// FORMAT 的 ack 处理：第一次 ack → COMPLETE；第二次 ack → 收尾 0。
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
        // 104 字节，严格对齐 mtkclient `get_emmc_info` 的解析顺序：
        //   0 type(4) | 4 block(4) | 8 boot1(8) | 16 boot2(8) | 24 rpmb(8)
        //   32 gp1 | 40 gp2 | 48 gp3 | 56 gp4 | **64 user(8)** | 72 cid(16) | 88 fwver(8)
        // 注意：user_size 在 0x40 而不是 0x20（0x20 是 gp1），
        // 早期实现两边都写在 0x20，"互相自洽但与真机不符"。
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
            // 静默 = 线缆脱落，语义等同"断开"：必须立刻失败让上层触发续传/提示重插，
            // 不能被当成可重试的瞬时超时（否则会在死设备上反复重试、白白干等）。
            return Err(PortError::Disconnected);
        }
        // 混沌：瞬时超时（不锁存，下一次读自动恢复）/ 静默（锁存到 replug）。
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
        // 混沌：丢包——仍"在线上发出"了 m 字节，但尾部 d 字节丢失，主机只收到 deliver 字节。
        //
        // 只对数据块（≥16 字节）生效：真实 USB 短包不会"丢尾部字节"，控制帧
        // （握手 / 命令回显 / 状态字，1~8 字节）丢字节只会让主机无限等待，
        // 与物理现实不符。丢包要模拟的是大块数据传输被劣质线材/干扰截断。
        let mut deliver = m;
        if let Some(c) = self.chaos.as_mut() {
            if m >= 16 && c.rng.hit(c.cfg.drop_permille) {
                c.drops += 1;
                let d = 1 + (c.rng.next_u64() as usize % (m - 1));
                deliver = m - d;
            }
        }
        for i in 0..m {
            let b = self.out.pop_front().unwrap();
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

    fn kind(&self) -> PortKind {
        PortKind::Mock
    }

    /// 清空未消费的输入字节（帧错位后重新同步）。
    fn drain(&mut self) -> Result<(), PortError> {
        self.out.clear();
        Ok(())
    }
}

// =============================================================================
// GPT 镜像生成
// =============================================================================

const GPT_ENTRIES: u32 = 128;
const GPT_ENTRY_SIZE: u32 = 128;
/// 每个分区的类型 GUID（Linux filesystem data，固定值即可）
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

/// 生成各分区的初始内容（按 4 KiB 页稀疏存放）。
///
/// 只有"有内容的分区"才占页：GPT 之外的空白区域读回 0x00，不会白白吃内存。
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
            // seccfg：真实 V4 结构（28 字节头 + 32 字节哈希），供 unlock 测试
            "seccfg" => {
                let mut hdr = [0u8; 28];
                hdr[0..4].copy_from_slice(&0x4D4D_4D4Du32.to_le_bytes());
                hdr[4..8].copy_from_slice(&4u32.to_le_bytes());
                hdr[8..12].copy_from_slice(&0x3Cu32.to_le_bytes());
                hdr[12..16].copy_from_slice(&profile.seccfg_lock_state.to_le_bytes());
                hdr[16..20].copy_from_slice(&profile.seccfg_dm_verity.to_le_bytes());
                hdr[20..24].copy_from_slice(&0u32.to_le_bytes()); // sboot_runtime
                hdr[24..28].copy_from_slice(&0x4545_4545u32.to_le_bytes());
                let digest = crate::sha::Sha256::digest(&hdr);
                let mut img = hdr.to_vec();
                img.extend_from_slice(&digest);
                put(addr, &img);
            }
            // vbmeta / vbmeta_system_*：合法 AVB 镜像（flags=0，含 boot descriptor）
            n if n.starts_with("vbmeta") => {
                let img = crate::security::avb::synthetic_vbmeta(0, &["boot"]);
                put(addr, &img);
            }
            // misc：boot_control 结构（偏移 0x2000 的 "BOOT" + 活动槽）
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
            // 其余分区：写入可辨识的初始内容（前 64 KiB），便于验证读写回环
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
    // LBA0: 保护性 MBR
    img[0x1BE] = 0x00; // not bootable
    img[0x1C2] = 0xEE; // type
    img[0x1C6..0x1CA].copy_from_slice(&1u32.to_le_bytes());
    let total_lbas = (profile.storage.user / 512).max(0x100000);
    img[0x1CA..0x1CE]
        .copy_from_slice(&((total_lbas - 1).min(u32::MAX as u64) as u32).to_le_bytes());
    img[0x1FE] = 0x55;
    img[0x1FF] = 0xAA;

    // 分区项数组（LBA2 起，128*128）
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
        // 名称：UTF-16LE，最多 36 字符
        for (j, ch) in name.encode_utf16().take(35).enumerate() {
            img[e + 56 + j * 2..e + 58 + j * 2].copy_from_slice(&ch.to_le_bytes());
        }
    }
    let entries_len = (GPT_ENTRIES * GPT_ENTRY_SIZE) as usize;
    let entries_crc = crc32(&img[entries_off..entries_off + entries_len]);

    // LBA1: 主 GPT 头
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
// 测试
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
        assert!(names.contains(&"super".to_string()), "实际: {:?}", names);
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
        // 直接构造一个校验和错误的分块
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
        // 回包：12 字节头 + 4 字节状态
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
        // 幂等：再擦一次，结果相同
        dev.erase_range(0, 64 * 1024 * 1024);
        assert!(dev.read_window(0, 512).iter().all(|b| *b == 0xFF));
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
        // 静默锁存 = 需要重插，按"断开"上报（不可重试）
        assert_eq!(dev.read(&mut b), Err(PortError::Disconnected));
    }

    #[test]
    fn unknown_brom_command_is_recorded_not_fatal() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        handshake(&mut dev);
        dev.write(&[0x7E]).unwrap();
        assert!(dev.events.iter().any(|e| e.contains("未支持命令")));
    }

    /// 状态文件的读写布局必须严格对称——跨进程断点续传全靠它。
    #[test]
    fn state_roundtrip_preserves_pages_and_erased() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        dev.write_range(0x1000, &[0xAB; 8192]); // 两页
        dev.write_range(0x9000, &[0xCD; 4096]); // 一页
        dev.erase_range(0x2_0000, 0x1_0000);
        dev.total_written = 0x1234;

        let path = std::env::temp_dir().join(format!("vdev-state-{}.bin", std::process::id()));
        let path = path.to_string_lossy().to_string();
        dev.save_state(&path).unwrap();

        let mut dst = VirtualDevice::from_preset("v5-clean").unwrap();
        dst.load_state(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(dst.pages, dev.pages); // 预设自带的种子页也要一页不差
        assert_eq!(dst.total_written, 0x1234);
        assert_eq!(dst.erased, dev.erased);
        assert!(
            dev.erased
                .iter()
                .any(|(s, e)| *s <= 0x2_0000 && *e >= 0x3_0000)
        );
        // 内容逐字节一致（含页内未被写的空白部分）
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

    // ── Phase 1.1 混沌注入 ─────────────────────────────────────────

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
        // 0‰ 永不翻转
        let mut nil = chaos_state(ChaosConfig::default());
        assert_eq!(nil.dirty(64), None);

        // 1000‰ 必然翻转，且下标/位号在范围内
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
        // 静默锁存 = 必须重插，上报为 Disconnected（不可重试，立刻失败）
        assert_eq!(dev.read(&mut b), Err(PortError::Disconnected));
        assert!(dev.silent, "静默必须锁存（直到 replug）");
        assert_eq!(dev.read(&mut b), Err(PortError::Disconnected));
        // replug 后恢复（并让环境恢复平静，验证 replug 清掉的是锁存标志而非混沌本身）
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

    /// Phase 1.1b 验收：真静默（掉线）必须**立刻**失败，绝不在死设备上重试干等。
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

    /// Phase 1.1b 验收：数据块被丢字节（长度不符）时，靠"清空输入缓冲 + 重发
    /// READ_DATA"整条重来恢复——字节级续读救不了已丢失的字节。
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

    /// Phase 1.1b 验收：混沌瞬时超时被有界重试吸收，DA 加载不应被单次抖动打断。
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

    /// 端到端：位翻转发生在校验和通过之后，落盘数据被改坏，
    /// 只能靠主机侧回读校验发现——证明"静默损坏不会冒充成功"。
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

        // 校验和本身是通过的（设备回 status=0），随后才发生落盘位翻转。
        let mut st = [0u8; 64];
        let n = dev.read(&mut st).unwrap();
        let status = u32::from_le_bytes(st[n - 4..n].try_into().unwrap());
        assert_eq!(status, 0, "校验和应被接受（翻转在之后）");

        let back = dev.read_window(0x100000, 512);
        assert_ne!(back, data.to_vec(), "位翻转必须让落盘数据与写入不一致");
        assert_eq!(dev.chaos_counts().0, 1, "应恰好翻转一次");
    }
}

/// MediaTek USB 厂商 ID
pub const MEDIATEK_VID: u16 = 0x0E8D;

/// 设备类型（用于判断握手策略和模式识别）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Brom,
    Preloader,
    PreloaderVariant,
    Unknown,
}

impl DeviceType {
    /// 根据 VID/PID 判断设备类型
    pub fn from_vid_pid(vid: u16, pid: u16) -> Self {
        if vid != MEDIATEK_VID {
            return DeviceType::Unknown;
        }
        match pid {
            0x0003 | 0xF200 | 0xD1E9 | 0xD1E2 | 0xD1EC | 0xD1DD => DeviceType::Brom,
            0x2000 => DeviceType::Preloader,
            0x2001 => DeviceType::PreloaderVariant,
            _ => DeviceType::Unknown,
        }
    }

    /// 判断是否为 BROM 设备（影响握手协议）
    pub fn is_brom(&self) -> bool {
        matches!(self, DeviceType::Brom)
    }
}

/// USB 设备配置（VID/PID 集中管理）
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    pub vid: u16,
    pub pid: u16,
    pub device_type: DeviceType,
    pub name: &'static str,
    pub description: &'static str,
}

/// 支持的设备配置表（按尝试顺序排列）
pub const SUPPORTED_DEVICES: &[DeviceConfig] = &[
    DeviceConfig {
        vid: MEDIATEK_VID,
        pid: 0x0003,
        device_type: DeviceType::Brom,
        name: "MTK BROM",
        description: "MediaTek Boot ROM mode",
    },
    DeviceConfig {
        vid: MEDIATEK_VID,
        pid: 0x2000,
        device_type: DeviceType::Preloader,
        name: "MTK Preloader",
        description: "MediaTek Preloader mode",
    },
    DeviceConfig {
        vid: MEDIATEK_VID,
        pid: 0x2001,
        device_type: DeviceType::PreloaderVariant,
        name: "MTK Preloader Variant",
        description: "MediaTek Preloader variant mode",
    },
];

/// 用户指定的工作模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkMode {
    /// BROM 模式：完整流程（握手 → 关看门狗 → bypass → send_da）
    Brom,
    /// Preloader 模式：跳过 bypass，直接 send_da
    Preloader,
    /// 自动检测（预留）
    Auto,
}

impl WorkMode {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "brom" => Some(WorkMode::Brom),
            "preloader" => Some(WorkMode::Preloader),
            "auto" => Some(WorkMode::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub log_level: log::LevelFilter,
    pub da_path: Option<String>,
    pub preloader_path: Option<String>,
    pub work_mode: WorkMode,
    pub da_x_speed: u8,
    pub skip_partitions: Option<String>,
    pub verify: bool,
    pub command: Option<String>,
    pub cmd_args: Vec<String>,
}

impl AppConfig {
    pub fn from_cli(cli: &crate::cmd::cli::Cli) -> Self {
        let log_level = match cli.log_level {
            2 => log::LevelFilter::Debug,
            3 => log::LevelFilter::Trace,
            _ => log::LevelFilter::Info,
        };

        let work_mode = WorkMode::from_str(&cli.mode).unwrap_or(WorkMode::Brom);

        AppConfig {
            log_level,
            da_path: cli.loader_path.clone(),
            preloader_path: cli.preloader_path.clone(),
            work_mode,
            da_x_speed: cli.da_x_speed,
            skip_partitions: cli.skip_partitions.clone(),
            verify: cli.verify,
            command: cli.command.clone(),
            cmd_args: cli.args.clone(),
        }
    }
}

/// 芯片完整配置（对齐 Python ChipConfig + Mt6768Config）
#[derive(Debug, Clone, Copy)]
pub struct ChipConfig {
    pub hw_code: u16,
    /// DA 文件内部使用的 hw_code（对齐 Python Chipconfig.dacode）
    /// BROM 返回 0x0707 但 DA 文件内匹配 0x6768，两者不同
    pub da_code: u16,
    pub name: &'static str,
    pub loader: &'static str,
    pub watchdog: u32,
    pub brom_payload_addr: u32,
    pub sej_base: u32,
    pub send_ptr: (u32, u32),             // (地址, 偏移)
    pub brom_register_access: (u32, u32), // (地址1, 地址2)
    /// Kamakiri2 基准地址（da_read/da_write 内部 kamakiri2 步骤使用）
    /// Python 用 brom_register_access[0][1]，但某些芯片需要特殊值
    /// None 时自动使用 brom_register_access.1
    pub ptr_da_bra: Option<u32>,
    /// da_read 读取 ptr_send 的地址
    /// None 时自动使用 send_ptr.1
    pub ptr_send_addr: Option<u32>,
}

/// 芯片配置表（按 hw_code 索引）
/// 从 mtkclient brom_config.py 自动提取，包含 67 种芯片配置
pub use crate::system::chips_generated::CHIP_CONFIGS;

/// 设备安全配置（对齐 penumbra `Preloader::get_target_config` 位域文档）
///
/// 位定义（`target_config` 低 9 位，逐位独立）：
/// - `0x001` SBC          — Secure Boot Control
/// - `0x002` SLA          — Serial Link Authentication
/// - `0x004` DAA          — Download Agent Authentication
/// - `0x008` EppParam     — EPP 参数
/// - `0x010` RootCert     — 需要根证书
/// - `0x020` MemReadAuth  — 内存读取鉴权
/// - `0x040` MemWriteAuth — 内存写入鉴权
/// - `0x080` CacheOpAuth  — Cache 操作鉴权
/// - `0x100` SctrlCert    — Sctrl 证书
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetConfig {
    /// 原始 32 位配置值
    pub raw: u32,
    /// Secure Boot Control
    pub sbc: bool,
    /// Serial Link Authentication
    pub sla: bool,
    /// Download Agent Authentication
    pub daa: bool,
    /// EPP 参数
    pub epp_param: bool,
    /// 需要根证书
    pub root_cert: bool,
    /// 内存读取鉴权
    pub mem_read_auth: bool,
    /// 内存写入鉴权
    pub mem_write_auth: bool,
    /// Cache 操作鉴权
    pub cache_op_auth: bool,
    /// Sctrl 证书
    pub sctrl_cert: bool,
}

impl TargetConfig {
    pub const BIT_SBC: u32 = 0x001;
    pub const BIT_SLA: u32 = 0x002;
    pub const BIT_DAA: u32 = 0x004;
    pub const BIT_EPP_PARAM: u32 = 0x008;
    pub const BIT_ROOT_CERT: u32 = 0x010;
    pub const BIT_MEM_READ_AUTH: u32 = 0x020;
    pub const BIT_MEM_WRITE_AUTH: u32 = 0x040;
    pub const BIT_CACHE_OP_AUTH: u32 = 0x080;
    pub const BIT_SCTRL_CERT: u32 = 0x100;

    /// 由原始 32 位配置解析各安全标志位
    pub fn from_raw(raw: u32) -> Self {
        TargetConfig {
            raw,
            sbc: raw & Self::BIT_SBC != 0,
            sla: raw & Self::BIT_SLA != 0,
            daa: raw & Self::BIT_DAA != 0,
            epp_param: raw & Self::BIT_EPP_PARAM != 0,
            root_cert: raw & Self::BIT_ROOT_CERT != 0,
            mem_read_auth: raw & Self::BIT_MEM_READ_AUTH != 0,
            mem_write_auth: raw & Self::BIT_MEM_WRITE_AUTH != 0,
            cache_op_auth: raw & Self::BIT_CACHE_OP_AUTH != 0,
            sctrl_cert: raw & Self::BIT_SCTRL_CERT != 0,
        }
    }

    /// 是否需要执行 Kamakiri2 bypass
    /// 除了 SBC/SLA/DAA 外，Mem Read/Write Auth 也会阻止 BROM 的内存读写命令
    pub fn needs_bypass(&self) -> bool {
        self.sbc || self.sla || self.daa || self.mem_read_auth || self.mem_write_auth
    }

    /// 是否需要 SLA 认证（SLA 或 DAA 任一置位）
    pub fn requires_auth(&self) -> bool {
        self.sla || self.daa
    }

    pub fn format_info(&self) -> String {
        let t = |b: bool| if b { "True" } else { "False" };
        format!(
            "设备信息: 0x{:03X}\n  SBC: {} / SLA: {} / DAA: {}\n  \
             EppParam: {} / RootCert: {} / SctrlCert: {}\n  \
             Mem Read Auth: {} / Mem Write Auth: {} / Cache Op Auth: {}",
            self.raw,
            t(self.sbc),
            t(self.sla),
            t(self.daa),
            t(self.epp_param),
            t(self.root_cert),
            t(self.sctrl_cert),
            t(self.mem_read_auth),
            t(self.mem_write_auth),
            t(self.cache_op_auth),
        )
    }
}

// =============================================================================
// 单元测试 — target_config 位域解析（P1-2）
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_config_zero_has_no_flags() {
        let cfg = TargetConfig::from_raw(0);
        assert!(!cfg.needs_bypass());
        assert!(!cfg.requires_auth());
        assert_eq!(cfg.raw, 0);
    }

    #[test]
    fn target_config_each_bit_is_independent() {
        // 逐位独立：每个掩码只置位自身对应的字段
        let sbc = TargetConfig::from_raw(TargetConfig::BIT_SBC);
        assert!(sbc.sbc && !sbc.sla && !sbc.daa && !sbc.mem_read_auth);

        let sla = TargetConfig::from_raw(TargetConfig::BIT_SLA);
        assert!(sla.sla && !sla.sbc && !sla.daa);

        let daa = TargetConfig::from_raw(TargetConfig::BIT_DAA);
        assert!(daa.daa && !daa.sbc && !daa.sla);

        let epp = TargetConfig::from_raw(TargetConfig::BIT_EPP_PARAM);
        assert!(epp.epp_param && !epp.root_cert);

        let cert = TargetConfig::from_raw(TargetConfig::BIT_ROOT_CERT);
        assert!(cert.root_cert && !cert.epp_param);

        let mra = TargetConfig::from_raw(TargetConfig::BIT_MEM_READ_AUTH);
        assert!(mra.mem_read_auth && !mra.mem_write_auth);

        let mwa = TargetConfig::from_raw(TargetConfig::BIT_MEM_WRITE_AUTH);
        assert!(mwa.mem_write_auth && !mwa.mem_read_auth);

        let coa = TargetConfig::from_raw(TargetConfig::BIT_CACHE_OP_AUTH);
        assert!(coa.cache_op_auth && !coa.sctrl_cert);

        let scc = TargetConfig::from_raw(TargetConfig::BIT_SCTRL_CERT);
        assert!(scc.sctrl_cert && !scc.cache_op_auth);
    }

    #[test]
    fn target_config_bit_masks_match_penumbra() {
        // 对齐 penumbra 文档：SBC..SctrlCert 共 9 位
        assert_eq!(TargetConfig::BIT_SBC, 0x001);
        assert_eq!(TargetConfig::BIT_SLA, 0x002);
        assert_eq!(TargetConfig::BIT_DAA, 0x004);
        assert_eq!(TargetConfig::BIT_EPP_PARAM, 0x008);
        assert_eq!(TargetConfig::BIT_ROOT_CERT, 0x010);
        assert_eq!(TargetConfig::BIT_MEM_READ_AUTH, 0x020);
        assert_eq!(TargetConfig::BIT_MEM_WRITE_AUTH, 0x040);
        assert_eq!(TargetConfig::BIT_CACHE_OP_AUTH, 0x080);
        assert_eq!(TargetConfig::BIT_SCTRL_CERT, 0x100);
    }

    #[test]
    fn target_config_needs_bypass_follows_security_bits() {
        assert!(TargetConfig::from_raw(TargetConfig::BIT_SBC).needs_bypass());
        assert!(TargetConfig::from_raw(TargetConfig::BIT_MEM_READ_AUTH).needs_bypass());
        assert!(TargetConfig::from_raw(TargetConfig::BIT_MEM_WRITE_AUTH).needs_bypass());
        // 仅高位（CacheOpAuth/SctrlCert/RootCert/EppParam）不触发 bypass
        assert!(!TargetConfig::from_raw(TargetConfig::BIT_CACHE_OP_AUTH).needs_bypass());
        assert!(!TargetConfig::from_raw(TargetConfig::BIT_SCTRL_CERT).needs_bypass());
    }

    #[test]
    fn target_config_requires_auth_on_sla_or_daa() {
        assert!(TargetConfig::from_raw(TargetConfig::BIT_SLA).requires_auth());
        assert!(TargetConfig::from_raw(TargetConfig::BIT_DAA).requires_auth());
        assert!(!TargetConfig::from_raw(TargetConfig::BIT_MEM_READ_AUTH).requires_auth());
    }

    #[test]
    fn target_config_format_info_reports_flags() {
        let cfg = TargetConfig::from_raw(TargetConfig::BIT_SBC | TargetConfig::BIT_MEM_READ_AUTH);
        let s = cfg.format_info();
        assert!(s.contains("SBC: True"), "{}", s);
        assert!(s.contains("Mem Read Auth: True"), "{}", s);
        assert!(s.contains("Mem Write Auth: False"), "{}", s);
    }
}

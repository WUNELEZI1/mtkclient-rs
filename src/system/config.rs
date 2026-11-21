/// MediaTek USB vendor ID
pub const MEDIATEK_VID: u16 = 0x0E8D;

/// Device type (drives the handshake strategy and mode detection)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Brom,
    Preloader,
    PreloaderVariant,
    Unknown,
}

impl DeviceType {
    /// Determine the device type from VID/PID
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

    /// Whether this is a BROM device (affects the handshake protocol)
    pub fn is_brom(&self) -> bool {
        matches!(self, DeviceType::Brom)
    }
}

/// USB device configuration (centralizes VID/PID)
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    pub vid: u16,
    pub pid: u16,
    pub device_type: DeviceType,
    pub name: &'static str,
    pub description: &'static str,
}

/// Supported device table (in probe order)
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

/// User-selected work mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkMode {
    /// BROM mode: full flow (handshake → disable watchdog → bypass → send_da)
    Brom,
    /// Preloader mode: skip bypass and call send_da directly
    Preloader,
    /// Auto-detect (reserved)
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
    /// Skip confirmation for dangerous operations (`--yes`)
    pub assume_yes: bool,
    /// Simulated flash state file (`--sim-state`)
    pub sim_state: Option<String>,
    /// Chaos simulation (`--sim-chaos`): random packet loss / silence / transient timeouts / bit flips
    pub sim_chaos: bool,
    /// Chaos RNG seed (`--sim-chaos-seed`); a fixed seed reproduces the failure sequence
    pub sim_chaos_seed: Option<u64>,
    /// Transient-timeout retry count (`--max-retries`, default 3; read direction only)
    pub max_retries: u32,
}

/// Look up a boolean switch (`--flag`) among the positional arguments.
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Look up the value of `--flag value` or `--flag=value` among the positional arguments.
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let eq = format!("{}=", flag);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
    }
    None
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
            // `args` is an `allow_hyphen_values` positional argument, so a `--yes` written
            // after the command is swallowed by it; accept both positions here.
            assume_yes: cli.yes || has_flag(&cli.args, "--yes"),
            // `args` is an `allow_hyphen_values` positional argument: global switches placed
            // after the command (`--yes` / `--sim-state` / `--sim-chaos` …) are swallowed by it.
            // Accept both positions so flags never fail silently — the hardest bug class to trace.
            sim_state: cli
                .sim_state
                .clone()
                .or_else(|| arg_value(&cli.args, "--sim-state")),
            sim_chaos: cli.sim_chaos || has_flag(&cli.args, "--sim-chaos"),
            sim_chaos_seed: cli
                .sim_chaos_seed
                .or_else(|| arg_value(&cli.args, "--sim-chaos-seed").and_then(|v| v.parse().ok())),
            max_retries: arg_value(&cli.args, "--max-retries")
                .and_then(|v| v.parse().ok())
                .unwrap_or(cli.max_retries),
        }
    }
}

/// Full chip configuration (mirrors Python ChipConfig + Mt6768Config)
#[derive(Debug, Clone, Copy)]
pub struct ChipConfig {
    pub hw_code: u16,
    /// hw_code used inside the DA file (mirrors Python Chipconfig.dacode)
    /// BROM returns 0x0707 but the DA file matches 0x6768, so the two differ
    pub da_code: u16,
    pub name: &'static str,
    pub loader: &'static str,
    pub watchdog: u32,
    pub brom_payload_addr: u32,
    pub sej_base: u32,
    pub send_ptr: (u32, u32),             // (address, offset)
    pub brom_register_access: (u32, u32), // (address1, address2)
    /// Kamakiri2 base address (used by the kamakiri2 step inside da_read/da_write)
    /// Python uses brom_register_access`0``1`, but some chips need a special value
    /// When None, brom_register_access.1 is used automatically
    pub ptr_da_bra: Option<u32>,
    /// Address from which da_read reads ptr_send
    /// When None, send_ptr.1 is used automatically
    pub ptr_send_addr: Option<u32>,
}

/// Chip configuration table (indexed by hw_code)
/// Auto-extracted from mtkclient brom_config.py; covers 67 chip configurations
pub use crate::system::chips_generated::CHIP_CONFIGS;

/// Device security configuration (mirrors penumbra `Preloader::get_target_config` bitfield docs)
///
/// Bit definitions (low 9 bits of `target_config`, each independent):
/// - `0x001` SBC          — Secure Boot Control
/// - `0x002` SLA          — Serial Link Authentication
/// - `0x004` DAA          — Download Agent Authentication
/// - `0x008` EppParam     — EPP parameters
/// - `0x010` RootCert     — requires a root certificate
/// - `0x020` MemReadAuth  — memory read authentication
/// - `0x040` MemWriteAuth — memory write authentication
/// - `0x080` CacheOpAuth  — cache operation authentication
/// - `0x100` SctrlCert    — Sctrl certificate
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetConfig {
    /// Raw 32-bit configuration value
    pub raw: u32,
    /// Secure Boot Control
    pub sbc: bool,
    /// Serial Link Authentication
    pub sla: bool,
    /// Download Agent Authentication
    pub daa: bool,
    /// EPP parameters
    pub epp_param: bool,
    /// Requires a root certificate
    pub root_cert: bool,
    /// Memory read authentication
    pub mem_read_auth: bool,
    /// Memory write authentication
    pub mem_write_auth: bool,
    /// Cache operation authentication
    pub cache_op_auth: bool,
    /// Sctrl certificate
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

    /// Parse the security flags from the raw 32-bit configuration
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

    /// Whether a Kamakiri2 bypass is required
    /// Besides SBC/SLA/DAA, Mem Read/Write Auth also blocks BROM memory read/write commands
    pub fn needs_bypass(&self) -> bool {
        self.sbc || self.sla || self.daa || self.mem_read_auth || self.mem_write_auth
    }

    /// Whether SLA authentication is required (SLA or DAA set)
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
// Unit tests — target_config bitfield parsing (P1-2)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_after_command_are_recognised() {
        // Positional arguments swallow global switches written after the command; accept both positions.
        let args: Vec<String> = ["w", "boot_a", "x.img", "--yes", "--sim-state", "/tmp/s.bin"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(has_flag(&args, "--yes"));
        assert_eq!(
            arg_value(&args, "--sim-state").as_deref(),
            Some("/tmp/s.bin")
        );
        assert_eq!(arg_value(&args, "--sim-state=/tmp/t.bin"), None);
        let eq_args: Vec<String> = ["w", "--sim-state=/tmp/t.bin"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            arg_value(&eq_args, "--sim-state").as_deref(),
            Some("/tmp/t.bin")
        );
    }

    #[test]
    fn target_config_zero_has_no_flags() {
        let cfg = TargetConfig::from_raw(0);
        assert!(!cfg.needs_bypass());
        assert!(!cfg.requires_auth());
        assert_eq!(cfg.raw, 0);
    }

    #[test]
    fn target_config_each_bit_is_independent() {
        // Bits are independent: each mask sets only its own field
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
        // Mirrors the penumbra docs: SBC..SctrlCert, 9 bits in total
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
        // High bits alone (CacheOpAuth/SctrlCert/RootCert/EppParam) do not trigger the bypass
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

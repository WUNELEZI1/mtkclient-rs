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

    /// 判断是否为 Preloader 设备
    #[allow(dead_code)] // 预留：Preloader 模式下区分设备类型
    pub fn is_preloader(&self) -> bool {
        matches!(self, DeviceType::Preloader | DeviceType::PreloaderVariant)
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
pub enum 工作模式 {
    /// BROM 模式：完整流程（握手 → 关看门狗 → bypass → send_da）
    Brom,
    /// Preloader 模式：跳过 bypass，直接 send_da
    Preloader,
    /// 自动检测（预留）
    Auto,
}

impl 工作模式 {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "brom" => Some(工作模式::Brom),
            "preloader" => Some(工作模式::Preloader),
            "auto" => Some(工作模式::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // 预留：CLI/设备配置映射字段，部分在当前命令集下不会全部读取
pub struct AppConfig {
    pub log_level: log::LevelFilter,
    pub da2_path: Option<String>,
    pub preloader_path: Option<String>,
    pub loader_path: Option<String>,
    pub 工作模式: 工作模式,
    pub parttype: Option<String>,
    pub offset: Option<u64>,
    pub length: Option<u64>,
    pub sector: Option<u32>,
    pub sectors: Option<u32>,
    pub verify: bool,
    pub command: Option<String>,
    pub cmd_args: Vec<String>,
}

impl AppConfig {
    pub fn from_cli(cli: &crate::cli::Cli) -> Self {
        let log_level = match cli.log_level {
            2 => log::LevelFilter::Debug,
            3 => log::LevelFilter::Trace,
            _ => log::LevelFilter::Info,
        };

        let 工作模式 = 工作模式::from_str(&cli.工作模式)
            .unwrap_or(工作模式::Brom);

        AppConfig {
            log_level,
            da2_path: cli.da2_path.clone(),
            preloader_path: cli.preloader_path.clone(),
            loader_path: cli.loader_path.clone(),
            工作模式,
            parttype: cli.parttype.clone(),
            offset: cli.offset,
            length: cli.length,
            sector: cli.sector,
            sectors: cli.sectors,
            verify: cli.verify,
            command: cli.command.clone(),
            cmd_args: cli.args.clone(),
        }
    }
}

/// 芯片完整配置（对齐 Python ChipConfig + Mt6768Config）
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // 预留：芯片能力表，当前机型只会用到其中一部分字段
pub struct ChipConfig {
    pub hw_code: u16,
    pub name: &'static str,
    pub description: &'static str,
    pub loader: &'static str,
    pub var1: u8,
    pub watchdog: u32,
    pub uart: u32,
    pub brom_payload_addr: u32,
    pub da_payload_addr: u32,
    pub pl_payload_addr: u32,
    pub gcpu_base: u32,
    pub sej_base: u32,
    pub dxcc_base: u32,
    pub cqdma_base: u32,
    pub ap_dma_mem: u32,
    pub send_ptr: (u32, u32), // (地址, 偏移)
    pub ctrl_buffer: u32,
    pub cmd_handler: u32,
    pub brom_register_access: (u32, u32), // (地址1, 地址2)
    pub meid_addr: u32,
    pub socid_addr: u32,
    pub prov_addr: u32,
    pub misc_lock: u32,
    pub efuse_addr: u32,
    pub blacklist: &'static [(u32, u32)],
    pub blacklist_count: u32,
    /// Kamakiri2 基准地址（da_read/da_write 内部 kamakiri2 步骤使用）
    /// Python 用 brom_register_access[0][1]，但某些芯片需要特殊值
    /// None 时自动使用 brom_register_access.1
    pub ptr_da_bra: Option<u32>,
    /// da_read 读取 ptr_send 的地址
    /// None 时自动使用 send_ptr.1
    pub ptr_send_addr: Option<u32>,
}

/// 芯片配置表（按 hw_code 索引）
pub static CHIP_CONFIGS: &[ChipConfig] = &[
    // MT6768 / MT6769 (Helio P65/G85)
    ChipConfig {
        hw_code: 0x0707,
        name: "MT6768/MT6769",
        description: "Helio P65/G85 k68v1",
        loader: "mt6768_payload.bin",
        var1: 0x25,
        watchdog: 0x10007000,
        uart: 0x11002000,
        brom_payload_addr: 0x100A00,
        da_payload_addr: 0x200000,
        pl_payload_addr: 0x40200000,
        gcpu_base: 0x10050000,
        sej_base: 0x1000A000,
        dxcc_base: 0x10210000,
        cqdma_base: 0x10212000,
        ap_dma_mem: 0x110001A0,
        send_ptr: (0x10286C, 0xC190),
        ctrl_buffer: 0x00102A28,
        cmd_handler: 0x0000CF15,
        brom_register_access: (0xC598, 0xC650),
        meid_addr: 0x102AF8,
        socid_addr: 0x102B08,
        prov_addr: 0x1054F4,
        misc_lock: 0x1001A100,
        efuse_addr: 0x11CE0000,
        blacklist: &[(0x10282C, 0x0), (0x00105994, 0)],
        blacklist_count: 0x0000000A,
        // MT6768 特殊值（从实际设备逆向，已验证可工作）
        ptr_da_bra: Some(0xC650),
        ptr_send_addr: Some(0xC190),
    },
    // MT6771 (Helio P60/P70)
    ChipConfig {
        hw_code: 0x0788,
        name: "MT6771",
        description: "Helio P60/P70 k71v1",
        loader: "mt6771_payload.bin",
        var1: 0x0A,
        watchdog: 0x10007000,
        uart: 0x11002000,
        brom_payload_addr: 0x100A00,
        da_payload_addr: 0x201000,
        pl_payload_addr: 0x40200000,
        gcpu_base: 0x10050000,
        sej_base: 0x1000A000,
        dxcc_base: 0x10210000,
        cqdma_base: 0x10212000,
        ap_dma_mem: 0x11000158,
        send_ptr: (0x102878, 0xDEBC),
        ctrl_buffer: 0x00102A80,
        cmd_handler: 0x0000EBE9,
        brom_register_access: (0xE2D0, 0xE388),
        meid_addr: 0x102B38,
        socid_addr: 0x102B48,
        prov_addr: 0x1065C0,
        misc_lock: 0x1001A100,
        efuse_addr: 0x11F10000,
        blacklist: &[(0x102834, 0x0), (0x106A60, 0x0)],
        blacklist_count: 0x0000000A,
        // 无特殊值，使用 brom_register_access.1 和 send_ptr.1 作为默认
        ptr_da_bra: None,
        ptr_send_addr: None,
    },
];

/// 设备安全配置（对齐 Python get_target_config）
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // 预留：安全标志位扩展，部分位当前仅用于显示/后续功能
pub struct TargetConfig {
    pub raw: u32,
    pub sbc: bool,      // Bit 0x01 - Secure Boot Control
    pub sla: bool,      // Bit 0x02 - Serial Link Authentication
    pub daa: bool,      // Bit 0x04 - Download Agent Authentication
    pub swjtag: bool,   // Bit 0x06 - Software JTAG
    pub epp: bool,      // Bit 0x08 - EPP Parameter
    pub cert: bool,     // Bit 0x10 - Root Certificate Required
    pub memread: bool,  // Bit 0x20 - Memory Read Authentication
    pub memwrite: bool, // Bit 0x40 - Memory Write Authentication
    pub cmd_c8: bool,   // Bit 0x80 - Command 0xC8 Blocked
}

impl TargetConfig {
    pub fn from_raw(raw: u32) -> Self {
        TargetConfig::from_raw_u64(raw as u64)
    }

    pub fn from_raw_u64(raw: u64) -> Self {
        let raw32 = (raw & 0xFFFFFFFF) as u32;
        TargetConfig {
            raw: raw32,
            sbc: (raw32 & 0x01) != 0,
            sla: (raw32 & 0x02) != 0,
            daa: (raw32 & 0x04) != 0,
            swjtag: (raw32 & 0x06) != 0,
            epp: (raw32 & 0x08) != 0,
            cert: (raw32 & 0x10) != 0,
            memread: (raw32 & 0x20) != 0,
            memwrite: (raw32 & 0x40) != 0,
            cmd_c8: (raw32 & 0x80) != 0,
        }
    }

    /// 判断是否需要执行 Kamakiri2 bypass
    /// 除了 SBC/SLA/DAA 外，Mem Read Auth 也会阻止 BROM 0xD1 读命令
    pub fn needs_bypass(&self) -> bool {
        self.sbc || self.sla || self.daa || self.memread
    }

    pub fn format_info(&self) -> String {
        format!(
            "设备信息: 0x{:02X}\n  SBC: {} / SLA: {} / DAA: {}\n  Mem Read Auth: {} / Mem Write Auth: {}\n  Cmd 0xC8 blocked: {}",
            self.raw,
            if self.sbc { "True" } else { "False" },
            if self.sla { "True" } else { "False" },
            if self.daa { "True" } else { "False" },
            if self.memread { "True" } else { "False" },
            if self.memwrite { "True" } else { "False" },
            if self.cmd_c8 { "True" } else { "False" },
        )
    }
}

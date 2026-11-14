//! 设备状态：Planner 的输入之一。
//!
//! 这里只放**规划需要**的字段，不放连接句柄、不放大缓冲区。

use super::safety::SafetyFlags;

/// DA 协议版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolVersion {
    /// XFlash，DA 包头部不含 `MTK_DA_v6`
    V5,
    /// XML，DA 包头部含 `MTK_DA_v6`
    V6,
    /// 尚未解析 DA（`--loader` 未提供或解析失败）
    Unknown,
}

impl ProtocolVersion {
    pub const fn label(self) -> &'static str {
        match self {
            ProtocolVersion::V5 => "V5(XFlash)",
            ProtocolVersion::V6 => "V6(XML)",
            ProtocolVersion::Unknown => "unknown",
        }
    }

    /// DA 文件头 0x68 字节内是否含 `MTK_DA_v6`。
    ///
    /// 依据 `mtkclient/Library/DA/daconfig.py`：`v6 = b"MTK_DA_v6" in hdr`。
    /// 这是**权威来源**；芯片型号表只用于未提供 DA 时的低置信度预判。
    pub fn from_da_header(header: &[u8]) -> Self {
        if header.len() >= 0x68 {
            if header[..0x68].windows(9).any(|w| w == b"MTK_DA_v6") {
                return ProtocolVersion::V6;
            }
            return ProtocolVersion::V5;
        }
        if header.windows(9).any(|w| w == b"MTK_DA_v6") {
            ProtocolVersion::V6
        } else {
            ProtocolVersion::Unknown
        }
    }
}

/// README 明确“BootROM 已修复、必须 `--loader`”的 V6 芯片（DA code）。
///
/// 来源：mtkclient README 中文版。**未在源码表中二次验证**，
/// 因此 Planner 只把它当“低置信度预判”，并与 DA 头解析结果取或。
pub const KNOWN_V6_DA_CODES: [u16; 7] = [
    0x6781, // MT6781
    0x6789, // MT6789
    0x6855, // MT6855
    0x6886, // MT6886
    0x6895, // MT6895
    0x6983, // MT6983
    0x8985, // MT8985
];

/// 规划所需的设备状态快照。
#[derive(Debug, Clone)]
pub struct DeviceState {
    /// BROM 返回的 hw_code（通常是 0x0707 之类）
    pub hw_code: u16,
    /// DA 文件内使用的 da_code（如 0x6768）
    pub da_code: u16,
    /// 芯片名（展示用）
    pub name: String,
    /// 协议版本（来自 DA 头，权威）
    pub protocol: ProtocolVersion,
    /// 安全标志
    pub safety: SafetyFlags,
    /// BROM 要求认证（拿到 auth 前 BROM 路径不可达）
    pub brom_auth_required: bool,
    /// Preloader 已修补（DA 签名验证失败）
    pub preloader_patched: bool,
    /// DA 签名验证失败
    pub da_signature_failed: bool,
    /// 当前活动槽（A/B）
    pub current_slot: Option<char>,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            hw_code: 0,
            da_code: 0,
            name: String::new(),
            protocol: ProtocolVersion::Unknown,
            safety: SafetyFlags::NONE,
            brom_auth_required: false,
            preloader_patched: false,
            da_signature_failed: false,
            current_slot: None,
        }
    }
}

impl DeviceState {
    /// 是否 V6：DA 头优先；未解析时用芯片名单低置信度预判。
    pub fn is_v6(&self) -> bool {
        match self.protocol {
            ProtocolVersion::V6 => true,
            ProtocolVersion::V5 => false,
            ProtocolVersion::Unknown => KNOWN_V6_DA_CODES.contains(&self.da_code),
        }
    }

    /// BROM 路径是否可达。
    ///
    /// 三条排除条件：
    /// 1. V6 芯片（BROM 漏洞已修复）；
    /// 2. BROM 要求认证且尚未通过；
    /// 3. Preloader 已修补且 DA 签名失败（说明 BROM 侧已无绕过）。
    pub fn brom_reachable(&self) -> bool {
        if self.is_v6() {
            return false;
        }
        if self.brom_auth_required {
            return false;
        }
        !(self.preloader_patched && self.da_signature_failed)
    }

    /// 加载 DA 前是否必须先绕过校验。
    pub fn needs_da_bypass(&self) -> bool {
        self.safety.requires_da_bypass() || self.da_signature_failed
    }

    /// 简要描述（决策日志用）。
    pub fn summary(&self) -> String {
        format!(
            "chip={}(0x{:04X}) proto={} safety=[{}] brom_reachable={}",
            if self.name.is_empty() {
                "unknown"
            } else {
                &self.name
            },
            self.da_code,
            self.protocol.label(),
            self.safety.names().join(","),
            self.brom_reachable()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn da_header_detects_v6() {
        let mut hdr = vec![0u8; 0x100];
        hdr[0x40..0x49].copy_from_slice(b"MTK_DA_v6");
        assert_eq!(ProtocolVersion::from_da_header(&hdr), ProtocolVersion::V6);

        let hdr2 = vec![0xAAu8; 0x100];
        assert_eq!(ProtocolVersion::from_da_header(&hdr2), ProtocolVersion::V5);
        assert_eq!(
            ProtocolVersion::from_da_header(&[0u8; 8]),
            ProtocolVersion::Unknown
        );
    }

    #[test]
    fn v6_blocks_brom() {
        let mut s = DeviceState::default();
        s.da_code = 0x6983;
        assert!(s.is_v6());
        assert!(!s.brom_reachable());

        s.protocol = ProtocolVersion::V5;
        assert!(!s.is_v6(), "DA 头为 V5 时以 DA 为准，覆盖名单预判");
        assert!(s.brom_reachable());
    }

    #[test]
    fn auth_required_blocks_brom() {
        let mut s = DeviceState::default();
        s.protocol = ProtocolVersion::V5;
        s.brom_auth_required = true;
        assert!(!s.brom_reachable());
    }

    #[test]
    fn bypass_needed_on_safety_or_sig_fail() {
        let mut s = DeviceState::default();
        assert!(!s.needs_da_bypass());
        s.safety = SafetyFlags::from_raw(SafetyFlags::DAA);
        assert!(s.needs_da_bypass());
    }
}

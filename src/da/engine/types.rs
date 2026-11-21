//! Public data types and error definitions for the protocol engine.

use crate::transport::PortError;

/// Device info from the connect phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BromInfo {
    pub hw_code: u16,
    pub hw_sub: u16,
    /// Raw target config bits (security flags)
    pub target_config: u32,
}

/// Storage info (from the eMMC info query).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageInfo {
    pub emmc_type: u32,
    pub block: u32,
    pub boot1: u64,
    pub boot2: u64,
    pub user: u64,
}

impl StorageInfo {
    pub fn type_name(&self) -> &'static str {
        match self.emmc_type {
            1 => "UFS",
            2 => "SD",
            3 => "MMC",
            6 => "UFS_CARD",
            _ => "eMMC",
        }
    }

    /// Type value used as the `CUSTOM_SET_STORAGE` argument.
    pub fn storage_type(&self) -> u32 {
        self.emmc_type
    }
}

/// Protocol phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Not connected
    Idle,
    /// Handshake done, in BROM
    Brom,
    /// DA1 running (XFlash commands available)
    Da1,
    /// DA2 loaded + storage opened
    Ready,
}

/// Engine error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// Port-layer error
    Port(PortError),
    /// Protocol mismatch (bad magic / length / status code)
    Protocol(String),
    /// DA rejected by SLA
    SlaRequired,
    /// Step out of order (e.g. READ_DATA sent before DA connect)
    OutOfOrder { phase: Phase, want: &'static str },
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Port(e) => write!(f, "端口错误: {}", e),
            EngineError::Protocol(m) => write!(f, "协议错误: {}", m),
            EngineError::SlaRequired => write!(f, "DA 被 SLA 拒绝（需先绕过安全启动）"),
            EngineError::OutOfOrder { phase, want } => {
                write!(f, "阶段错误: 当前 {:?}，需要 {}", phase, want)
            }
        }
    }
}

impl std::error::Error for EngineError {}

impl From<PortError> for EngineError {
    fn from(e: PortError) -> Self {
        EngineError::Port(e)
    }
}

use std::fmt;

// =============================================================================
// Device status code classification
// =============================================================================

/// Device status code classification (simplified penumbra `XFlashErrorKind`)
///
/// A status code whose top 4 bits are `0xC` means error; it is further split by domain:
/// - `0xC002____` → security domain error (SecurityError)
/// - `0xC004____` → device domain error (DeviceError)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    /// Status 0: success
    Success,
    /// Unsupported command
    UnsupportedCommand,
    /// Protocol error
    ProtocolError,
    /// Security domain error (SLA/auth, etc.)
    SecurityError(u32),
    /// Device domain error (storage/partition, etc.)
    DeviceError(u32),
    /// Other/unknown error code
    Unknown(u32),
}

impl StatusCode {
    /// Known specific error codes
    pub const CODE_UNSUPPORTED_COMMAND: u32 = 0xC001_0003;
    pub const CODE_PROTOCOL_ERROR: u32 = 0xC001_0005;

    /// Map a raw status code to its classification
    pub fn from_u32(code: u32) -> Self {
        match code {
            0 => Self::Success,
            Self::CODE_UNSUPPORTED_COMMAND => Self::UnsupportedCommand,
            Self::CODE_PROTOCOL_ERROR => Self::ProtocolError,
            c if (c >> 28) == 0xC && ((c >> 16) & 0xFF) == 0x02 => Self::SecurityError(c),
            c if (c >> 28) == 0xC && ((c >> 16) & 0xFF) == 0x04 => Self::DeviceError(c),
            c => Self::Unknown(c),
        }
    }

    /// Whether this is the success status
    pub fn is_success(self) -> bool {
        matches!(self, StatusCode::Success)
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StatusCode::Success => write!(f, "成功"),
            StatusCode::UnsupportedCommand => write!(f, "不支持的命令"),
            StatusCode::ProtocolError => write!(f, "协议错误"),
            StatusCode::SecurityError(c) => write!(f, "安全域错误(0x{:08X})", c),
            StatusCode::DeviceError(c) => write!(f, "设备域错误(0x{:08X})", c),
            StatusCode::Unknown(c) => write!(f, "未知错误(0x{:08X})", c),
        }
    }
}

// =============================================================================
// DA device status errors
// =============================================================================

/// DA device status error carrying the classification and raw code for user messages/retry decisions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XFlashError {
    /// Status code classification
    pub kind: StatusCode,
    /// Raw status code
    pub code: u32,
}

impl XFlashError {
    /// Build from a raw status code; returns `None` when the status is 0 (success)
    pub fn from_status(code: u32) -> Option<Self> {
        let kind = StatusCode::from_u32(code);
        if kind.is_success() {
            None
        } else {
            Some(XFlashError { kind, code })
        }
    }
}

impl fmt::Display for XFlashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DA 状态错误: {} (0x{:08X})", self.kind, self.code)
    }
}

impl std::error::Error for XFlashError {}

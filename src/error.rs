//! Unified error types
//!
//! Historically the project used `Result<T, String>` everywhere, so error messages were flat
//! strings that callers could not programmatically split into timeout / protocol / device
//! rejection / session expiry. While keeping `AppError` compatible, this module adds a
//! `Error → UsbError / ProtocolError / XFlashError / SessionError`）：
//!
//! - [`UsbError`]        — USB transport layer (open failure / timeout / missing interface)
//! - [`ProtocolError`]   — protocol layer (magic / length / ACK / handshake)
//! - [`StatusCode`]      — device status code classification (`from_u32` maps automatically)
//! - [`XFlashError`]     — DA device status error (carries [`StatusCode`] and the raw code)
//! - [`SessionError`]    — `.state` session persistence errors
//!
//! Migration: new code may return the types above directly (they implement `std::error::Error`
//! and provide `From` conversions to [`AppError`]); old `Result<T, String>` code stays
//! unchanged and is migrated gradually.

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
// USB transport errors
// =============================================================================

/// USB transport errors
///
/// Note: each variant maps to a real USB failure cause (enumeration/open/interface/control/timeout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbError {
    /// No matching device found
    NotFound,
    /// Failed to open the device
    OpenFailed(String),
    /// Single transfer timed out
    Timeout,
    /// No usable bulk/control interface found
    InterfaceNotFound,
    /// Control transfer failed
    CtrlTransferFailed,
}

impl fmt::Display for UsbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsbError::NotFound => write!(f, "未找到 USB 设备"),
            UsbError::OpenFailed(msg) => write!(f, "打开 USB 设备失败: {}", msg),
            UsbError::Timeout => write!(f, "USB 传输超时"),
            UsbError::InterfaceNotFound => write!(f, "未找到 USB 接口/端点"),
            UsbError::CtrlTransferFailed => write!(f, "USB 控制传输失败"),
        }
    }
}

impl std::error::Error for UsbError {}

// =============================================================================
// Protocol layer errors
// =============================================================================

/// Protocol layer errors (XFlash / BROM handshake / frame parsing)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Handshake still failing after N attempts
    HandshakeFailed(u32),
    /// Packet header magic mismatch
    BadMagic { got: u32, expected: u32 },
    /// Invalid packet length
    InvalidPacketLength(u32),
    /// Device status is non-zero
    StatusNonZero(u32),
    /// ACK rejected
    AckRejected(u32),
    /// Too many consecutive device message packets; protocol desync suspected
    MessageFlood { count: u32 },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::HandshakeFailed(n) => write!(f, "握手失败（已重试 {} 次）", n),
            ProtocolError::BadMagic { got, expected } => write!(
                f,
                "XFlash 头 magic 错误: 0x{:08X}（期望 0x{:08X}）",
                got, expected
            ),
            ProtocolError::InvalidPacketLength(len) => write!(f, "非法包长度: {}", len),
            ProtocolError::StatusNonZero(code) => write!(f, "设备状态非 0: 0x{:08X}", code),
            ProtocolError::AckRejected(code) => write!(f, "ACK 被拒绝: 0x{:08X}", code),
            ProtocolError::MessageFlood { count } => {
                write!(f, "连续收到 {} 个设备 Message 包，疑似协议失步", count)
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

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

// =============================================================================
// Session persistence errors
// =============================================================================

/// `.state` session persistence errors
///
/// Raised by the `.state` load path so a silent degradation becomes a diagnosable failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// State file corrupted or unparsable
    StateFileCorrupted(String),
    /// Device fingerprint mismatch (different device)
    DeviceMismatch(String),
    /// DA not loaded yet
    DaNotLoaded,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::StateFileCorrupted(msg) => write!(f, "会话状态文件损坏: {}", msg),
            SessionError::DeviceMismatch(msg) => write!(f, "设备指纹不匹配: {}", msg),
            SessionError::DaNotLoaded => write!(f, "DA 尚未加载"),
        }
    }
}

impl std::error::Error for SessionError {}

// =============================================================================
// XML (V6) DA protocol errors
// =============================================================================

/// Error classification returned by the XML/V6 DA protocol (mirrors penumbra `XmlErrorKind`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlErrorKind {
    /// Unknown/unclassified
    Unknown,
    /// Device does not support the command (`ERR!UNSUPPORTED`)
    UnsupportedCmd,
    /// Command cancelled (`ERR!CANCEL`)
    Cancel,
    /// DA anti-rollback check failed
    AntiRollbackViolation,
    /// Expected `CMD:DOWNLOAD-FILE` but got another command
    ExpectedCmdDownloadFile,
    /// Expected `CMD:UPLOAD-FILE` but got another command
    ExpectedCmdUploadFile,
    /// Expected `CMD:PROGRESS-REPORT` but got another command
    ExpectedCmdProgressReport,
    /// Expected `CMD:FILE-SYS-OPERATION` but got another command
    ExpectedFileSysOp,
    /// DA SLA signature rejected
    SlaSignatureRejected,
    /// Unknown path separator
    UnknownPathSep,
    /// Other error carrying raw text
    Other(String),
    /// Invalid KDF output key length (Extensions)
    InvalidKeyDeriveLength,
    /// Invalid KDF label/salt length (Extensions)
    InvalidLabelOrSaltLength,
    /// SEJ AES data length exceeds the limit (Extensions)
    SejAesLengthExceeded,
    /// Unknown storage type; cannot perform RPMB operations
    StorageUnknown,
    /// Invalid RPMB key format
    InvalidRpmbKey,
    /// RPMB partition not initialized
    RpmbNotInitialized,
    /// RPMB initialization failed
    RpmbInitFailed,
    /// RPMB read failed
    RpmbReadFailed,
    /// RPMB write failed
    RpmbWriteFailed,
}

impl fmt::Display for XmlErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            XmlErrorKind::Unknown => "未知错误",
            XmlErrorKind::UnsupportedCmd => "设备不支持该命令",
            XmlErrorKind::Cancel => "命令被取消",
            XmlErrorKind::AntiRollbackViolation => "DA 反回滚校验失败",
            XmlErrorKind::ExpectedCmdDownloadFile => "期望 CMD:DOWNLOAD-FILE",
            XmlErrorKind::ExpectedCmdUploadFile => "期望 CMD:UPLOAD-FILE",
            XmlErrorKind::ExpectedCmdProgressReport => "期望 CMD:PROGRESS-REPORT",
            XmlErrorKind::ExpectedFileSysOp => "期望 CMD:FILE-SYS-OPERATION",
            XmlErrorKind::SlaSignatureRejected => "DA SLA 签名被拒绝",
            XmlErrorKind::UnknownPathSep => "未知路径分隔符",
            XmlErrorKind::Other(msg) => msg.as_str(),
            XmlErrorKind::InvalidKeyDeriveLength => "KDF 输出密钥长度非法",
            XmlErrorKind::InvalidLabelOrSaltLength => "KDF label/salt 长度非法",
            XmlErrorKind::SejAesLengthExceeded => "SEJ AES 数据长度超限",
            XmlErrorKind::StorageUnknown => "存储类型未知，无法执行 RPMB 操作",
            XmlErrorKind::InvalidRpmbKey => "RPMB key 格式/长度非法",
            XmlErrorKind::RpmbNotInitialized => "RPMB 分区未初始化",
            XmlErrorKind::RpmbInitFailed => "RPMB 初始化失败",
            XmlErrorKind::RpmbReadFailed => "RPMB 读取失败",
            XmlErrorKind::RpmbWriteFailed => "RPMB 写入失败",
        };
        write!(f, "{}", s)
    }
}

/// XML/V6 DA protocol error carrying the classification and the raw device text
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError {
    /// Raw error text from the device (trailing `\0` removed)
    pub message: String,
    /// Error classification
    pub kind: XmlErrorKind,
}

impl XmlError {
    /// Build from a classification (`Other` keeps its inner text)
    pub fn from_kind(kind: XmlErrorKind) -> Self {
        let message = match &kind {
            XmlErrorKind::Other(msg) => msg.clone(),
            other => other.to_string(),
        };
        XmlError { message, kind }
    }

    /// Parse device error text and map it to [`XmlErrorKind`]
    ///
    /// Mirrors penumbra `XmlError::from_message`: strip the trailing `\0`, then match the known
    /// error texts exactly; unmatched text becomes [`XmlErrorKind::Other`] (keeping it), and
    /// empty text becomes [`XmlErrorKind::Unknown`].
    pub fn from_message(resp: &[u8]) -> Self {
        let msg = String::from_utf8_lossy(resp);
        let msg = msg.trim_end_matches('\0');
        // Empty text (no reason from the device) maps to Unknown instead of degrading to Other("")
        if msg.is_empty() {
            return XmlError::from_kind(XmlErrorKind::Unknown);
        }
        let kind = match msg {
            "ERR!UNSUPPORTED" => XmlErrorKind::UnsupportedCmd,
            "ERR!CANCEL" => XmlErrorKind::Cancel,
            "Invalid DA Version" => XmlErrorKind::AntiRollbackViolation,
            "Server is not authenticated. Locked." => XmlErrorKind::SlaSignatureRejected,
            "Unknow path separator." => XmlErrorKind::UnknownPathSep,
            "Invalid key derive output length" => XmlErrorKind::InvalidKeyDeriveLength,
            "Invalid label or salt length" => XmlErrorKind::InvalidLabelOrSaltLength,
            "SEJ AES data length exceeds maximum allowed" => XmlErrorKind::SejAesLengthExceeded,
            "Storage type unknown, cannot initialize RPMB"
            | "Storage type unknown, cannot read RPMB"
            | "Storage type unknown, cannot write RPMB" => XmlErrorKind::StorageUnknown,
            "RPMB key must be 64 hex chars (32 bytes)" | "Invalid RPMB key format" => {
                XmlErrorKind::InvalidRpmbKey
            }
            "RPMB partition not initialized" => XmlErrorKind::RpmbNotInitialized,
            "RPMB initialization failed" => XmlErrorKind::RpmbInitFailed,
            "RPMB read failed" => XmlErrorKind::RpmbReadFailed,
            "RPMB write failed" => XmlErrorKind::RpmbWriteFailed,
            other => XmlErrorKind::Other(other.to_string()),
        };
        XmlError::from_kind(kind)
    }
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "XML DA 错误: {}", self.message)
    }
}

impl std::error::Error for XmlError {}

// =============================================================================
// Unified application error (backwards compatible)
// =============================================================================

/// Unified application error type
///
/// Replaces the project-wide scattered `Result<..., String>` and distinguishes error kinds.
/// Currently used only in new code and migrated gradually.
#[derive(Debug)]
pub enum AppError {
    Usb(String),
    Protocol(String),
    Security(String),
    Io(std::io::Error),
    Parse(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Usb(msg) => write!(f, "USB 错误: {}", msg),
            AppError::Protocol(msg) => write!(f, "协议错误: {}", msg),
            AppError::Security(msg) => write!(f, "安全错误: {}", msg),
            AppError::Io(err) => write!(f, "IO 错误: {}", err),
            AppError::Parse(msg) => write!(f, "解析错误: {}", msg),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Io(err)
    }
}

impl From<&str> for AppError {
    fn from(msg: &str) -> Self {
        AppError::Protocol(msg.to_string())
    }
}

impl From<String> for AppError {
    fn from(msg: String) -> Self {
        AppError::Protocol(msg)
    }
}

// --- Structured error → AppError conversions (transport failures stay visible to is_transport_error) ---

impl From<UsbError> for AppError {
    fn from(err: UsbError) -> Self {
        AppError::Usb(err.to_string())
    }
}

impl From<ProtocolError> for AppError {
    fn from(err: ProtocolError) -> Self {
        AppError::Protocol(err.to_string())
    }
}

impl From<XFlashError> for AppError {
    fn from(err: XFlashError) -> Self {
        // Device status errors are protocol-layer interaction failures, not USB transport failures;
        // their messages carry no transport keywords, so is_transport_error correctly returns false.
        AppError::Protocol(err.to_string())
    }
}

impl From<SessionError> for AppError {
    fn from(err: SessionError) -> Self {
        AppError::Protocol(err.to_string())
    }
}

impl From<XmlError> for AppError {
    fn from(err: XmlError) -> Self {
        AppError::Protocol(err.to_string())
    }
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_code_zero_is_success() {
        let kind = StatusCode::from_u32(0);
        assert_eq!(kind, StatusCode::Success);
        assert!(kind.is_success());
        assert!(XFlashError::from_status(0).is_none());
    }

    #[test]
    fn status_code_known_errors() {
        assert_eq!(
            StatusCode::from_u32(StatusCode::CODE_UNSUPPORTED_COMMAND),
            StatusCode::UnsupportedCommand
        );
        assert_eq!(
            StatusCode::from_u32(StatusCode::CODE_PROTOCOL_ERROR),
            StatusCode::ProtocolError
        );
    }

    #[test]
    fn status_code_domain_classification() {
        // 0xC002____ → security domain
        assert_eq!(
            StatusCode::from_u32(0xC002_0001),
            StatusCode::SecurityError(0xC002_0001)
        );
        // 0xC004____ → device domain
        assert_eq!(
            StatusCode::from_u32(0xC004_0040),
            StatusCode::DeviceError(0xC004_0040)
        );
        // Codes without the 0xC prefix → unknown
        assert_eq!(
            StatusCode::from_u32(0x1234_5678),
            StatusCode::Unknown(0x1234_5678)
        );
    }

    #[test]
    fn xflash_error_from_status_preserves_code() {
        let e = XFlashError::from_status(0xC001_0005).expect("应为错误");
        assert_eq!(e.code, 0xC001_0005);
        assert_eq!(e.kind, StatusCode::ProtocolError);
        // Display contains both the classification and the raw code
        let s = e.to_string();
        assert!(s.contains("协议错误"), "{}", s);
        assert!(s.contains("C0010005"), "{}", s);
    }

    #[test]
    fn protocol_error_bad_magic_message() {
        let e = ProtocolError::BadMagic {
            got: 0xDEAD_BEEF,
            expected: 0xFEEE_EEEF,
        };
        let s = e.to_string();
        assert!(s.contains("DEADBEEF"), "{}", s);
        assert!(s.contains("FEEEEEEF"), "{}", s);
    }

    #[test]
    fn structured_errors_convert_to_app_error() {
        let usb_err: AppError = UsbError::Timeout.into();
        assert!(matches!(usb_err, AppError::Usb(_)));
        let proto_err: AppError = ProtocolError::MessageFlood { count: 70 }.into();
        assert!(matches!(proto_err, AppError::Protocol(_)));
    }

    #[test]
    fn session_error_display() {
        assert_eq!(SessionError::DaNotLoaded.to_string(), "DA 尚未加载");
    }

    #[test]
    fn xml_error_maps_known_messages() {
        assert_eq!(
            XmlError::from_message(b"ERR!UNSUPPORTED\0").kind,
            XmlErrorKind::UnsupportedCmd
        );
        assert_eq!(
            XmlError::from_message(b"Invalid DA Version\0").kind,
            XmlErrorKind::AntiRollbackViolation
        );
        assert_eq!(
            XmlError::from_message(b"RPMB read failed\0").kind,
            XmlErrorKind::RpmbReadFailed
        );
        // No match → Other(msg), keeping the raw text
        let e = XmlError::from_message(b"some vendor text\0");
        assert_eq!(e.kind, XmlErrorKind::Other("some vendor text".into()));
        assert_eq!(e.message, "some vendor text");
        // Empty text → Unknown
        assert_eq!(XmlError::from_message(b"\0").kind, XmlErrorKind::Unknown);
    }

    #[test]
    fn xml_error_from_kind_other_keeps_message() {
        let e = XmlError::from_kind(XmlErrorKind::Other("boom".into()));
        assert_eq!(e.message, "boom");
        // Non-Other variants use the classification's readable text
        let e2 = XmlError::from_kind(XmlErrorKind::UnsupportedCmd);
        assert_eq!(e2.message, "设备不支持该命令");
    }

    #[test]
    fn xml_error_converts_to_app_error() {
        let app: AppError = XmlError::from_message(b"ERR!CANCEL").into();
        assert!(matches!(app, AppError::Protocol(_)));
    }
}

//! 统一错误类型
//!
//! 历史上全项目以 `Result<T, String>` 为主，错误信息是扁平字符串，调用方
//! 无法程序化区分"超时 / 协议错误 / 设备拒绝 / 会话失效"。本模块在保持
//! `AppError` 兼容的前提下，引入结构化错误层级（参考 penumbra 的
//! `Error → UsbError / ProtocolError / XFlashError / SessionError`）：
//!
//! - [`UsbError`]        — USB 传输层（打开失败 / 超时 / 接口缺失）
//! - [`ProtocolError`]   — 协议层（magic / 长度 / ACK / 握手）
//! - [`StatusCode`]      — 设备状态码分类（`from_u32` 自动映射）
//! - [`XFlashError`]     — DA 设备状态错误（携带 [`StatusCode`] 与原始码）
//! - [`SessionError`]    — `.state` 会话持久化错误
//!
//! 迁移策略：新代码可直接返回上述类型（它们实现 `std::error::Error` 且
//! 提供到 [`AppError`] 的 `From` 转换），旧代码的 `Result<T, String>` 保持
//! 不变，逐步替换。

use std::fmt;

// =============================================================================
// 设备状态码分类
// =============================================================================

/// 设备状态码分类（对齐 penumbra `XFlashErrorKind` 的简化版）
///
/// DA/BROM 返回的状态码高 4 位为 `0xC` 表示错误；按功能域进一步细分：
/// - `0xC002____` → 安全域错误（SecurityError）
/// - `0xC004____` → 设备域错误（DeviceError）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    /// 状态 0：成功
    Success,
    /// 不支持的命令
    UnsupportedCommand,
    /// 协议错误
    ProtocolError,
    /// 安全域错误（SLA/认证等）
    SecurityError(u32),
    /// 设备域错误（存储/分区等）
    DeviceError(u32),
    /// 其他/未知错误码
    Unknown(u32),
}

impl StatusCode {
    /// 已知的具体错误码
    pub const CODE_UNSUPPORTED_COMMAND: u32 = 0xC001_0003;
    pub const CODE_PROTOCOL_ERROR: u32 = 0xC001_0005;

    /// 由原始状态码映射到分类
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

    /// 是否为成功状态
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
// USB 传输层错误
// =============================================================================

/// USB 传输层错误
///
/// 注：各变体对应 USB 路径真实失败原因（枚举设备/打开/接口/控制传输/超时）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbError {
    /// 未找到匹配设备
    NotFound,
    /// 打开设备失败
    OpenFailed(String),
    /// 单笔传输超时
    Timeout,
    /// 未找到可用的 bulk/控制接口
    InterfaceNotFound,
    /// 控制传输失败
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
// 协议层错误
// =============================================================================

/// 协议层错误（XFlash / BROM 握手 / 帧解析）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// 握手在 N 次尝试后仍失败
    HandshakeFailed(u32),
    /// 包头 magic 不匹配
    BadMagic { got: u32, expected: u32 },
    /// 包长度非法
    InvalidPacketLength(u32),
    /// 设备状态非 0
    StatusNonZero(u32),
    /// ACK 被拒绝
    AckRejected(u32),
    /// 连续收到过多设备消息包，疑似协议失步
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
// DA 设备状态错误
// =============================================================================

/// DA 设备状态错误：携带分类与原始状态码，便于上层做友好提示/重试决策
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XFlashError {
    /// 状态码分类
    pub kind: StatusCode,
    /// 原始状态码
    pub code: u32,
}

impl XFlashError {
    /// 由原始状态码构造；状态为 0（成功）时返回 `None`
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
// 会话持久化错误
// =============================================================================

/// `.state` 会话持久化错误
///
/// 由 `.state` 加载路径构造并上报，用于把"静默降级"变为可诊断的失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// 状态文件损坏/无法解析
    StateFileCorrupted(String),
    /// 设备指纹不匹配（换了设备）
    DeviceMismatch(String),
    /// DA 尚未加载
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
// XML (V6) DA 协议错误
// =============================================================================

/// XML/V6 DA 协议返回的错误分类（对齐 penumbra `XmlErrorKind`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlErrorKind {
    /// 未知/未分类
    Unknown,
    /// 设备不支持该命令（`ERR!UNSUPPORTED`）
    UnsupportedCmd,
    /// 命令被取消（`ERR!CANCEL`）
    Cancel,
    /// DA 反回滚校验失败
    AntiRollbackViolation,
    /// 期望收到 `CMD:DOWNLOAD-FILE` 却收到其他命令
    ExpectedCmdDownloadFile,
    /// 期望收到 `CMD:UPLOAD-FILE` 却收到其他命令
    ExpectedCmdUploadFile,
    /// 期望收到 `CMD:PROGRESS-REPORT` 却收到其他命令
    ExpectedCmdProgressReport,
    /// 期望收到 `CMD:FILE-SYS-OPERATION` 却收到其他命令
    ExpectedFileSysOp,
    /// DA SLA 签名被拒绝
    SlaSignatureRejected,
    /// 未知路径分隔符
    UnknownPathSep,
    /// 其他携带原始文本的错误
    Other(String),
    /// KDF 输出的密钥长度非法（Extensions）
    InvalidKeyDeriveLength,
    /// KDF 的 label/salt 长度非法（Extensions）
    InvalidLabelOrSaltLength,
    /// SEJ AES 数据长度超限（Extensions）
    SejAesLengthExceeded,
    /// 存储类型未知，无法执行 RPMB 操作
    StorageUnknown,
    /// RPMB key 格式非法
    InvalidRpmbKey,
    /// RPMB 分区未初始化
    RpmbNotInitialized,
    /// RPMB 初始化失败
    RpmbInitFailed,
    /// RPMB 读取失败
    RpmbReadFailed,
    /// RPMB 写入失败
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

/// XML/V6 DA 协议错误：携带分类与设备原始文本
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError {
    /// 设备返回的原始错误文本（已去除结尾 `\0`）
    pub message: String,
    /// 错误分类
    pub kind: XmlErrorKind,
}

impl XmlError {
    /// 由分类构造（`Other` 会保留其内部文本）
    pub fn from_kind(kind: XmlErrorKind) -> Self {
        let message = match &kind {
            XmlErrorKind::Other(msg) => msg.clone(),
            other => other.to_string(),
        };
        XmlError { message, kind }
    }

    /// 解析设备返回的错误文本，映射到 [`XmlErrorKind`]
    ///
    /// 对齐 penumbra `XmlError::from_message`：先去除结尾 `\0`，再按已知
    /// 错误文本精确匹配；未命中则归类为 [`XmlErrorKind::Other`]（保留原文），
    /// 空文本归类为 [`XmlErrorKind::Unknown`]。
    pub fn from_message(resp: &[u8]) -> Self {
        let msg = String::from_utf8_lossy(resp);
        let msg = msg.trim_end_matches('\0');
        // 空文本（设备未给出原因）归为 Unknown，避免退化成 Other("")
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
// 统一应用错误（兼容旧接口）
// =============================================================================

/// 统一应用错误类型
///
/// 替代全项目散落的 `Result<..., String>`，区分不同错误种类。
/// 目前仅在新代码中使用，逐步替换旧代码。
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

// --- 结构化错误 → AppError 的转换（保证传输层失败仍被 is_transport_error 识别）---

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
        // 设备状态错误属于协议层交互失败，但并非 USB 传输失败；
        // 其消息不含传输层关键词，is_transport_error 会正确判为 false。
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
// 单元测试
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
        // 0xC002____ → 安全域
        assert_eq!(
            StatusCode::from_u32(0xC002_0001),
            StatusCode::SecurityError(0xC002_0001)
        );
        // 0xC004____ → 设备域
        assert_eq!(
            StatusCode::from_u32(0xC004_0040),
            StatusCode::DeviceError(0xC004_0040)
        );
        // 非 0xC 前缀的错误码 → 未知
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
        // Display 同时包含分类与原始码
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
        // 未命中 → Other(msg)，并保留原始文本
        let e = XmlError::from_message(b"some vendor text\0");
        assert_eq!(e.kind, XmlErrorKind::Other("some vendor text".into()));
        assert_eq!(e.message, "some vendor text");
        // 空文本 → Unknown
        assert_eq!(XmlError::from_message(b"\0").kind, XmlErrorKind::Unknown);
    }

    #[test]
    fn xml_error_from_kind_other_keeps_message() {
        let e = XmlError::from_kind(XmlErrorKind::Other("boom".into()));
        assert_eq!(e.message, "boom");
        // 非 Other 变体使用分类的可读文本
        let e2 = XmlError::from_kind(XmlErrorKind::UnsupportedCmd);
        assert_eq!(e2.message, "设备不支持该命令");
    }

    #[test]
    fn xml_error_converts_to_app_error() {
        let app: AppError = XmlError::from_message(b"ERR!CANCEL").into();
        assert!(matches!(app, AppError::Protocol(_)));
    }
}

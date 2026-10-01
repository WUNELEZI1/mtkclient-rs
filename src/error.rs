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
/// 注：部分变体（NotFound / InterfaceNotFound / CtrlTransferFailed）为对齐 penumbra
/// 的分类预留，当前 USB 路径大多以 `String` 返回，迁移完成后移除 `allow`。
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
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
    /// 设备状态非 0（预留：供 status 调用方按需构造）
    #[allow(dead_code)]
    StatusNonZero(u32),
    /// ACK 被拒绝（预留：供 ack 路径按需构造）
    #[allow(dead_code)]
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
/// 注：当前 `SessionState::load/from_string` 以 `Option` 静默降级，尚未构造这些变体；
/// 待会话加载路径改为返回 `Result` 后接入，届时移除 `allow`。
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
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
}

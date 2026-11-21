use std::fmt;

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

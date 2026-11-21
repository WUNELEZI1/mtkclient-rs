use std::fmt;

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

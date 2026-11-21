use std::fmt;

/// Transport-layer error classification (retryable / non-retryable / needs user action).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    /// Timeout, retryable
    Timeout,
    /// Device disconnected, user must replug
    Disconnected,
    #[cfg(test)]
    /// Operation not supported by the backend
    NotSupported(&'static str),
    #[cfg(test)]
    /// IO / driver error
    Io(String),
}

impl PortError {
    /// Whether automatic retry is worthwhile.
    pub const fn retryable(&self) -> bool {
        matches!(self, PortError::Timeout)
    }

    #[cfg(test)]
    /// Whether user action is required (replug / install driver).
    pub const fn needs_user(&self) -> bool {
        matches!(self, PortError::Disconnected)
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortError::Timeout => write!(f, "传输超时"),
            PortError::Disconnected => write!(f, "设备已断开"),
            #[cfg(test)]
            PortError::NotSupported(op) => write!(f, "后端不支持: {}", op),
            #[cfg(test)]
            PortError::Io(e) => write!(f, "IO 错误: {}", e),
        }
    }
}

impl std::error::Error for PortError {}

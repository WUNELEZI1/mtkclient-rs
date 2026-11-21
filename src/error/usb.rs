use std::fmt;

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

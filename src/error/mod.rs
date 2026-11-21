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

mod protocol;
mod session;
mod usb;
mod xflash;
mod xml;

#[cfg(test)]
mod tests;

pub use protocol::ProtocolError;
pub use session::SessionError;
pub use usb::UsbError;
#[allow(unused_imports)]
pub use xflash::{StatusCode, XFlashError};
pub use xml::{XmlError, XmlErrorKind};

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

// Legacy command handlers return boxed errors; keep them at the protocol layer, matching the
// previous explicit `.map_err(|e| AppError::Protocol(e.to_string()))` behaviour.
impl From<Box<dyn std::error::Error>> for AppError {
    fn from(err: Box<dyn std::error::Error>) -> Self {
        AppError::Protocol(err.to_string())
    }
}

// Executor failures are protocol-layer interaction failures; preserve the previous explicit
// `.map_err(|e| AppError::Protocol(e.to_string()))` behaviour so `?` can be used at call sites.
impl From<crate::exec::ExecError> for AppError {
    fn from(err: crate::exec::ExecError) -> Self {
        AppError::Protocol(err.to_string())
    }
}

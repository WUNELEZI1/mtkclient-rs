//! XML (V6) DA protocol session layer
//!
//! Mirrors penumbra `core/src/da/xml/protocol.rs`: on top of the low-level transport ([`BromTransport`])
//! it implements XML DA frame encoding/decoding and the session state machine.
//!
//! # Frame format
//! Same as XFlash: `magic(4) | data_type(4) | length(4)`, little-endian.
//! - `data_type = 1` (Flow) → normal flow packet;
//! - `data_type = 2` (Message) → DA async log packet; its payload must be consumed or later headers desync.
//!
//! # Command lifecycle
//! The device sends `CMD:START` before each command and `CMD:END` after it completes:
//! 1. The host reads `CMD:START` and replies `OK\0`;
//! 2. the host sends the command XML (with its trailing `\0`);
//! 3. the device replies `OK\0` (or `ERR!...` text);
//! 4. when required, the host reads `CMD:END` and replies `OK\0`.

use std::fmt;
use std::time::Duration;

use crate::error::{XmlError, XmlErrorKind};
use crate::preloader::transport::BromTransport;

/// Long timeout used during session operations (same as penumbra `MAX_TIMEOUT`)
pub const MAX_TIMEOUT: Duration = Duration::from_millis(10000);
/// Max accepted device frame payload length; mirrors the XFlash 64 MiB guard.
pub const MAX_XML_PACKET_LEN: u32 = 64 * 1024 * 1024;
/// Max consecutive Message packets skipped while reading a flow header (prevents an endless loop from DA log spam)
const MAX_MESSAGE_DRAINS: u32 = 64;
/// Send chunk limit before the packet length is negotiated
const DEFAULT_WRITE_CHUNK: usize = 0x8000;

mod frames;
mod session;

// =============================================================================
// Public re-exports (keeps crate::da::xml::protocol::* reachable)
// =============================================================================

pub use frames::{get_tag, get_tag_usize};

// =============================================================================
// Errors
// =============================================================================

/// XML protocol session error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlProtoError {
    /// Transport layer (port read/write timeout or failure)
    Transport(String),
    /// Protocol layer (bad frame header / bad ACK / XML parse failure)
    Protocol(String),
    /// XML error returned by the device
    Xml(XmlError),
}

impl XmlProtoError {
    pub(crate) fn proto<S: Into<String>>(msg: S) -> Self {
        XmlProtoError::Protocol(msg.into())
    }

    /// Whether this is a "command not supported" error
    pub fn is_unsupported(&self) -> bool {
        matches!(self, XmlProtoError::Xml(e) if e.kind == XmlErrorKind::UnsupportedCmd)
    }
}

impl fmt::Display for XmlProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XmlProtoError::Transport(m) => write!(f, "USB 传输错误: {}", m),
            XmlProtoError::Protocol(m) => write!(f, "XML 协议错误: {}", m),
            XmlProtoError::Xml(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for XmlProtoError {}

impl From<String> for XmlProtoError {
    fn from(msg: String) -> Self {
        XmlProtoError::Transport(msg)
    }
}

impl From<XmlError> for XmlProtoError {
    fn from(err: XmlError) -> Self {
        XmlProtoError::Xml(err)
    }
}

impl From<XmlErrorKind> for XmlProtoError {
    fn from(kind: XmlErrorKind) -> Self {
        XmlProtoError::Xml(XmlError::from_kind(kind))
    }
}

// =============================================================================
// Frame header
// =============================================================================

/// Common XML/XFlash packet header (12 bytes LE)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmlPacketHeader {
    /// Fixed magic (`CMD_MAGIC`)
    pub magic: u32,
    /// Packet type (Flow / Message)
    pub data_type: u32,
    /// Payload length
    pub length: u32,
}

// =============================================================================
// Session
// =============================================================================

/// XML DA protocol session
pub struct XmlProtocol<'a> {
    pub(crate) port: &'a mut dyn BromTransport,
    /// Packet length negotiated by `CMD:DOWNLOAD-FILE`; sends are chunked by it once set
    pub write_packed_length: Option<usize>,
}

// =============================================================================
// Private helpers (shared by session and tests)
// =============================================================================

/// Parse a size report of the form `OK@0x<hex>\0`
fn parse_ok_hex(text: &str) -> Result<usize, XmlProtoError> {
    let trimmed = text.trim_end_matches('\0').trim();
    let hex = trimmed
        .strip_prefix("OK@0x")
        .ok_or_else(|| XmlProtoError::proto(format!("非法大小回报: {:?}", trimmed)))?;
    usize::from_str_radix(hex, 16)
        .map_err(|_| XmlProtoError::proto(format!("大小解析失败: {:?}", hex)))
}

/// Substring search (to avoid extra dependencies)
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests;

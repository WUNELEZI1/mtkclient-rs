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
use std::io::{Read, Write};
use std::time::Duration;

use log::{debug, trace};

use crate::da::xflash::protocol::{CMD_MAGIC, DATA_TYPE_FLOW, DATA_TYPE_MESSAGE};
use crate::da::xml::cmd::{self, FileSystemOp, XmlCmdLifetime};
use crate::error::{XmlError, XmlErrorKind};
use crate::preloader::transport::BromTransport;

/// Long timeout used during session operations (same as penumbra `MAX_TIMEOUT`)
pub const MAX_TIMEOUT: Duration = Duration::from_millis(10000);
/// Max consecutive Message packets skipped while reading a flow header (prevents an endless loop from DA log spam)
const MAX_MESSAGE_DRAINS: u32 = 64;
/// Send chunk limit before the packet length is negotiated
const DEFAULT_WRITE_CHUNK: usize = 0x8000;

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
    /// Fixed magic ([`CMD_MAGIC`])
    pub magic: u32,
    /// Packet type (Flow / Message)
    pub data_type: u32,
    /// Payload length
    pub length: u32,
}

impl XmlPacketHeader {
    /// Fixed header length
    pub const SIZE: usize = 12;

    /// Flow packet (normal flow)
    pub fn flow(length: u32) -> Self {
        XmlPacketHeader {
            magic: CMD_MAGIC,
            data_type: DATA_TYPE_FLOW,
            length,
        }
    }

    /// Parse a header; returns `None` if magic mismatches or the length is invalid
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic != CMD_MAGIC {
            return None;
        }
        let data_type = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let length = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        Some(XmlPacketHeader {
            magic,
            data_type,
            length,
        })
    }

    /// Serialize to 12 bytes LE
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..4].copy_from_slice(&self.magic.to_le_bytes());
        buf[4..8].copy_from_slice(&self.data_type.to_le_bytes());
        buf[8..12].copy_from_slice(&self.length.to_le_bytes());
        buf
    }

    /// Whether this is a device async message packet
    pub fn is_message(&self) -> bool {
        self.data_type == DATA_TYPE_MESSAGE
    }
}

// =============================================================================
// Lightweight XML parsing
// =============================================================================

/// Get the text content at a tag path (e.g. `"arg/message"`)
pub fn get_tag(xml: &str, path: &str) -> Result<String, String> {
    let mut current = xml.to_string();
    for seg in path.split('/') {
        current = extract_tag(&current, seg).ok_or_else(|| format!("XML 标签 `{}` 未找到", seg))?;
    }
    Ok(current.trim().to_string())
}

/// Get a tag path and parse it as hex (accepts a `0x` prefix), e.g. `"arg/packet_length"`
pub fn get_tag_usize(xml: &str, path: &str) -> Result<usize, String> {
    let raw = get_tag(xml, path)?;
    let trimmed = raw.trim_start_matches("0x");
    usize::from_str_radix(trimmed, 16)
        .map_err(|_| format!("XML 标签 `{}` 不是合法十六进制: {}", path, raw))
}

/// Extract the inner text of the first `<tag>...</tag>`
fn extract_tag(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)? + start;
    Some(content[start..end].to_string())
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

impl<'a> XmlProtocol<'a> {
    /// Bind to an already opened transport port
    pub fn new(port: &'a mut dyn BromTransport) -> Self {
        XmlProtocol {
            port,
            write_packed_length: None,
        }
    }

    // --- Transport primitives ---

    fn write_all(&mut self, data: &[u8]) -> Result<(), XmlProtoError> {
        let mut pos = 0;
        while pos < data.len() {
            let n = self.port.write(&data[pos..])?;
            if n == 0 {
                return Err(XmlProtoError::Transport("端口写入返回 0 字节".into()));
            }
            pos += n;
        }
        Ok(())
    }

    /// Read the next Flow packet header (skipping and consuming any Message packets on the way)
    pub fn read_next_flow_header(&mut self) -> Result<XmlPacketHeader, XmlProtoError> {
        let mut drains = 0u32;
        loop {
            let mut buf = [0u8; XmlPacketHeader::SIZE];
            self.port.read_exact(&mut buf)?;
            let hdr = XmlPacketHeader::parse(&buf).ok_or_else(|| {
                debug!("[XML-RX] 非法包头: {:02X?}", buf);
                XmlProtoError::proto("非法包头（magic/长度）")
            })?;

            if hdr.is_message() {
                drains += 1;
                if drains > MAX_MESSAGE_DRAINS {
                    return Err(XmlProtoError::proto(
                        "连续收到过多 Message 包，疑似协议失步",
                    ));
                }
                self.drain_message(hdr.length)?;
                continue;
            }
            return Ok(hdr);
        }
    }

    /// Consume the payload of one Message packet (DA async log)
    fn drain_message(&mut self, length: u32) -> Result<(), XmlProtoError> {
        if length == 0 {
            return Ok(());
        }
        let mut payload = vec![0u8; length as usize];
        self.port.read_exact(&mut payload)?;
        // The first 4 bytes are a length prefix; the rest is log text
        if payload.len() > 4 {
            trace!(
                "[XML DA Message] {}",
                String::from_utf8_lossy(&payload[4..])
            );
        }
        Ok(())
    }

    /// Read the payload of one complete Flow packet
    pub fn read_data(&mut self) -> Result<Vec<u8>, XmlProtoError> {
        let hdr = self.read_next_flow_header()?;
        let mut data = vec![0u8; hdr.length as usize];
        self.port.read_exact(&mut data)?;
        Ok(data)
    }

    /// Send one data segment (adds a Flow header and chunks by the negotiated packet length)
    pub fn send(&mut self, data: &[u8]) -> Result<(), XmlProtoError> {
        self.send_data(&[data])
    }

    /// Send multiple segments, each with its own Flow header
    pub fn send_data(&mut self, parts: &[&[u8]]) -> Result<(), XmlProtoError> {
        let chunk_size = self.write_packed_length.unwrap_or(DEFAULT_WRITE_CHUNK);
        for part in parts {
            let hdr = XmlPacketHeader::flow(part.len() as u32);
            self.write_all(&hdr.to_bytes())?;
            let mut pos = 0;
            while pos < part.len() {
                let end = (pos + chunk_size).min(part.len());
                self.write_all(&part[pos..end])?;
                pos = end;
            }
        }
        Ok(())
    }

    // --- ACK / lifecycle ---

    /// Send an ACK: `OK\0` without a value, `OK@<v>\0` with one
    pub fn ack(&mut self, value: Option<usize>) -> Result<(), XmlProtoError> {
        match value {
            Some(v) => self.send(format!("OK@{}\0", v).as_bytes()),
            None => self.send(b"OK\0"),
        }
    }

    /// Read the device ACK (`OK\0` / `OK@0x0\0` count as success)
    pub fn read_ack(&mut self) -> Result<(), XmlProtoError> {
        let resp = self.read_data()?;
        let s = String::from_utf8_lossy(&resp);
        let trimmed = s.trim_end_matches('\0');

        if trimmed == "OK" || trimmed == "OK@0x0" {
            return Ok(());
        }
        if s.contains("ERR!") {
            return Err(XmlError::from_message(&resp).into());
        }
        Err(XmlProtoError::proto(format!("非法 ACK: {:?}", trimmed)))
    }

    /// Verify the command lifecycle marker (`CMD:START` / `CMD:END`)
    pub fn check_lifetime(&mut self, lifetime: XmlCmdLifetime) -> Result<(), XmlProtoError> {
        let data = match self.read_data() {
            Ok(d) => d,
            // When reusing an existing session the device may have already sent CMD:END, so a timeout is acceptable
            Err(XmlProtoError::Transport(_)) => return Ok(()),
            Err(e) => return Err(e),
        };

        let text = String::from_utf8_lossy(&data);
        if text.contains("<result>ERR</result>") {
            let msg = get_tag(&text, "arg/message").unwrap_or_default();
            return Err(XmlError::from_message(msg.as_bytes()).into());
        }
        if !contains_bytes(&data, &lifetime.pattern()) {
            return Err(XmlProtoError::proto("生命周期标记不匹配"));
        }
        Ok(())
    }

    /// Verify the lifecycle marker and send the ACK
    pub fn lifetime_ack(&mut self, lifetime: XmlCmdLifetime) -> Result<(), XmlProtoError> {
        let resp = self.check_lifetime(lifetime);
        self.ack(None)?;
        resp
    }

    /// Send one XML command and read the device ACK
    ///
    /// `Ok(false)` means the device reported "unsupported" (the `CMD:END` has been consumed).
    pub fn send_cmd(&mut self, xml_cmd: &str) -> Result<bool, XmlProtoError> {
        self.lifetime_ack(XmlCmdLifetime::CmdStart)?;
        self.send(xml_cmd.as_bytes())?;
        trace!("[XML-TX] {}", xml_cmd);

        match self.read_ack() {
            Ok(()) => Ok(true),
            Err(e) if e.is_unsupported() => {
                // Unsupported command: consume CMD:END to keep the state machine in sync
                self.lifetime_ack(XmlCmdLifetime::CmdEnd)?;
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    // --- Data transfer ---

    /// Receive data uploaded by the device (reads the `CMD:UPLOAD-FILE` command packet first)
    pub fn upload_data<W: Write, F: FnMut(usize, usize)>(
        &mut self,
        writer: &mut W,
        progress: &mut F,
    ) -> Result<usize, XmlProtoError> {
        let resp = self.read_data()?;
        let resp = String::from_utf8_lossy(&resp).into_owned();
        self.process_upload_data(&resp, writer, progress)
    }

    /// Read the full text response of `CMD:UPLOAD-FILE` (for GET-HW-INFO and similar)
    pub fn get_upload_file_resp(&mut self) -> Result<String, XmlProtoError> {
        let mut buf: Vec<u8> = Vec::new();
        self.upload_data(&mut buf, &mut |_, _| {})?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Handle the `CMD:UPLOAD-FILE` data stream
    pub fn process_upload_data<W: Write, F: FnMut(usize, usize)>(
        &mut self,
        resp: &str,
        writer: &mut W,
        progress: &mut F,
    ) -> Result<usize, XmlProtoError> {
        self.expect_command(resp, cmd::CMD_UPLOAD_FILE, || {
            XmlErrorKind::ExpectedCmdUploadFile
        })?;

        let packet_length =
            get_tag_usize(resp, "arg/packet_length").map_err(XmlProtoError::proto)?;

        // 1) Acknowledge receipt of the command
        self.ack(None)?;
        // 2) The device reports the total size as `OK@0x`<size>`\0`
        let size_resp = self.read_data()?;
        let size_text = String::from_utf8_lossy(&size_resp).into_owned();
        let size = parse_ok_hex(&size_text)?;
        // 3) Acknowledge again
        self.ack(None)?;

        let mut received = 0usize;
        let prev = self.port.get_timeout();
        self.port.set_timeout(MAX_TIMEOUT);

        let result: Result<(), XmlProtoError> = 'upload: {
            while received < size {
                let to_read = packet_length.min(size - received);
                if let Err(e) = self.read_ack() {
                    break 'upload Err(e);
                }
                if let Err(e) = self.ack(None) {
                    break 'upload Err(e);
                }
                let data = match self.read_data() {
                    Ok(d) => d,
                    Err(e) => break 'upload Err(e),
                };
                if let Err(e) = writer.write_all(&data) {
                    break 'upload Err(XmlProtoError::Transport(e.to_string()));
                }
                if let Err(e) = self.ack(None) {
                    break 'upload Err(e);
                }
                received += to_read;
                progress(received, size);
            }
            Ok(())
        };

        self.port.set_timeout(prev);
        result?;
        debug!("[XML] UPLOAD-FILE 完成，共 0x{:X} 字节", received);
        Ok(received)
    }

    /// Send data to the device (reads the `CMD:DOWNLOAD-FILE` command packet first)
    pub fn download_data<R: Read, F: FnMut(usize, usize)>(
        &mut self,
        size: usize,
        reader: &mut R,
        progress: &mut F,
    ) -> Result<usize, XmlProtoError> {
        let resp = self.read_data()?;
        let resp = String::from_utf8_lossy(&resp).into_owned();
        self.process_download_data(&resp, size, MAX_TIMEOUT, reader, progress)
    }

    /// Handle the `CMD:DOWNLOAD-FILE` data stream
    pub fn process_download_data<R: Read, F: FnMut(usize, usize)>(
        &mut self,
        resp: &str,
        size: usize,
        timeout: Duration,
        reader: &mut R,
        progress: &mut F,
    ) -> Result<usize, XmlProtoError> {
        self.expect_command(resp, cmd::CMD_DOWNLOAD_FILE, || {
            XmlErrorKind::ExpectedCmdDownloadFile
        })?;

        // 1) Acknowledge receipt of the command
        self.ack(None)?;
        // 2) Tell the device the size to be sent
        self.ack(Some(size))?;
        // 3) Read the device ready ACK
        self.read_ack()?;

        let packet_length =
            get_tag_usize(resp, "arg/packet_length").map_err(XmlProtoError::proto)?;
        // After negotiation, send no longer splits into smaller chunks
        self.write_packed_length = Some(packet_length);

        let mut chunk = vec![0u8; packet_length];
        let mut sent = 0usize;
        let prev = self.port.get_timeout();
        self.port.set_timeout(timeout);

        let result: Result<(), XmlProtoError> = 'download: {
            while sent < size {
                let to_read = packet_length.min(size - sent);
                if let Err(e) = reader.read_exact(&mut chunk[..to_read]) {
                    break 'download Err(XmlProtoError::Transport(e.to_string()));
                }
                // Per-chunk status handshake: ack(0) → read_ack → data → read_ack
                if let Err(e) = self.ack(Some(0)) {
                    break 'download Err(e);
                }
                if let Err(e) = self.read_ack() {
                    break 'download Err(e);
                }
                if let Err(e) = self.send(&chunk[..to_read]) {
                    break 'download Err(e);
                }
                if let Err(e) = self.read_ack() {
                    break 'download Err(e);
                }
                sent += to_read;
                progress(sent, size);
            }
            Ok(())
        };

        self.port.set_timeout(prev);
        result?;
        debug!("[XML] DOWNLOAD-FILE 完成，共 0x{:X} 字节", sent);
        Ok(sent)
    }

    /// Handle `CMD:PROGRESS-REPORT` (erase/write progress until the device replies `OK!EOT`)
    pub fn progress_report<F: FnMut(usize, usize)>(
        &mut self,
        progress: &mut F,
    ) -> Result<(), XmlProtoError> {
        let resp = self.read_data()?;
        let resp = String::from_utf8_lossy(&resp).into_owned();
        self.process_progress_report(&resp, progress)
    }

    /// Handle the `CMD:PROGRESS-REPORT` data stream
    pub fn process_progress_report<F: FnMut(usize, usize)>(
        &mut self,
        resp: &str,
        progress: &mut F,
    ) -> Result<(), XmlProtoError> {
        self.expect_command(resp, cmd::CMD_PROGRESS_REPORT, || {
            XmlErrorKind::ExpectedCmdProgressReport
        })?;

        let msg = get_tag(resp, "arg/message").unwrap_or_default();
        debug!("[XML] 进度上报: {}", msg);

        self.ack(None)?;

        let prev = self.port.get_timeout();
        self.port.set_timeout(MAX_TIMEOUT);

        let mut resp: Vec<u8> = Vec::new();
        let result: Result<(), XmlProtoError> = 'progress: {
            while resp != b"OK!EOT\0" {
                resp = match self.read_data() {
                    Ok(r) => r,
                    Err(e) => break 'progress Err(e),
                };
                if let Err(e) = self.ack(None) {
                    break 'progress Err(e);
                }
                let s = String::from_utf8_lossy(&resp);
                if !s.starts_with("OK!PROGRESS@") {
                    continue;
                }
                let Some(value) = s.trim_end_matches('\0').split('@').nth(1) else {
                    break 'progress Err(XmlProtoError::proto("进度响应格式非法"));
                };
                let Ok(value) = value.parse::<usize>() else {
                    break 'progress Err(XmlProtoError::proto("进度值解析失败"));
                };
                progress(value, 100);
            }
            Ok(())
        };

        self.port.set_timeout(prev);
        result?;
        progress(100, 100);
        Ok(())
    }

    /// Handle `CMD:FILE-SYS-OPERATION` (read the device request and reply `OK@<op>\0`)
    pub fn file_system_op(&mut self, op: FileSystemOp) -> Result<(), XmlProtoError> {
        let resp = self.read_data()?;
        let resp = String::from_utf8_lossy(&resp).into_owned();
        self.process_file_sys_op(&resp, op)
    }

    /// Handle the `CMD:FILE-SYS-OPERATION` response
    pub fn process_file_sys_op(
        &mut self,
        resp: &str,
        op: FileSystemOp,
    ) -> Result<(), XmlProtoError> {
        let command = get_tag(resp, "command").map_err(XmlProtoError::proto)?;
        if command != cmd::CMD_FILE_SYSTEM_OP {
            let message = get_tag(resp, "arg/message").unwrap_or_default();
            if message.is_empty() {
                return Err(XmlErrorKind::ExpectedFileSysOp.into());
            }
            return Err(XmlErrorKind::Other(message).into());
        }

        self.ack(None)?;
        let payload = format!("OK@{}\0", op.default_value());
        self.send(payload.as_bytes())
    }

    // --- Internal helpers ---

    /// Verify the response command name; on mismatch, report a more specific error from `arg/message`
    fn expect_command<C: FnOnce() -> XmlErrorKind>(
        &self,
        resp: &str,
        expected: &str,
        fallback: C,
    ) -> Result<(), XmlProtoError> {
        let command = get_tag(resp, "command").map_err(XmlProtoError::proto)?;
        if command == expected {
            return Ok(());
        }
        let message = get_tag(resp, "arg/message").unwrap_or_default();
        if message.is_empty() {
            Err(XmlErrorKind::from_kind_fallback(fallback()).into())
        } else {
            Err(XmlErrorKind::Other(message).into())
        }
    }
}

impl XmlErrorKind {
    /// Convenience constructor (for `XmlProtoError`'s `into()`)
    fn from_kind_fallback(kind: XmlErrorKind) -> XmlError {
        XmlError::from_kind(kind)
    }
}

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

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    /// Scriptable mock port
    struct MockPort {
        read_buf: Vec<u8>,
        pos: usize,
        written: Rc<RefCell<Vec<u8>>>,
        timeout: Duration,
    }

    impl MockPort {
        fn new(read_buf: Vec<u8>) -> Self {
            MockPort {
                read_buf,
                pos: 0,
                written: Rc::new(RefCell::new(Vec::new())),
                timeout: Duration::from_millis(1000),
            }
        }

        /// Shared write-buffer handle; take it before borrowing the port so it can be asserted on later
        fn written_handle(&self) -> Rc<RefCell<Vec<u8>>> {
            Rc::clone(&self.written)
        }
    }

    impl BromTransport for MockPort {
        fn write(&mut self, data: &[u8]) -> Result<usize, String> {
            self.written.borrow_mut().extend_from_slice(data);
            Ok(data.len())
        }
        fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            if self.pos + buf.len() > self.read_buf.len() {
                return Err("mock: 读越界/超时".to_string());
            }
            buf.copy_from_slice(&self.read_buf[self.pos..self.pos + buf.len()]);
            self.pos += buf.len();
            Ok(buf.len())
        }
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            let n = buf.len().min(self.read_buf.len() - self.pos);
            buf[..n].copy_from_slice(&self.read_buf[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
        fn set_timeout(&mut self, duration: Duration) {
            self.timeout = duration;
        }
        fn get_timeout(&self) -> Duration {
            self.timeout
        }
        fn do_handshake(&mut self) -> Result<bool, String> {
            Ok(true)
        }
        fn is_libusb(&self) -> bool {
            false
        }
    }

    /// Build a framed Flow packet (header + payload)
    fn flow_packet(payload: &[u8]) -> Vec<u8> {
        let mut v = XmlPacketHeader::flow(payload.len() as u32)
            .to_bytes()
            .to_vec();
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn header_roundtrip_and_reject_bad_magic() {
        let h = XmlPacketHeader::flow(0x1234);
        let bytes = h.to_bytes();
        assert_eq!(XmlPacketHeader::parse(&bytes), Some(h));
        assert_eq!(h.length, 0x1234);
        assert!(!h.is_message());

        let mut bad = bytes;
        bad[0] = 0;
        assert_eq!(XmlPacketHeader::parse(&bad), None);
        assert_eq!(XmlPacketHeader::parse(&[0u8; 4]), None);
    }

    #[test]
    fn tag_extraction_supports_nested_paths() {
        let xml = "<da><command>CMD:UPLOAD-FILE</command>\
                   <arg><packet_length>0x10000</packet_length><info>hello</info></arg></da>";
        assert_eq!(get_tag(xml, "command").unwrap(), "CMD:UPLOAD-FILE");
        assert_eq!(get_tag(xml, "arg/info").unwrap(), "hello");
        assert_eq!(get_tag_usize(xml, "arg/packet_length").unwrap(), 0x10000);
        assert!(get_tag(xml, "arg/missing").is_err());
    }

    #[test]
    fn ack_and_read_ack_accept_ok_forms() {
        let mut port = MockPort::new(flow_packet(b"OK\0"));
        let mut xml = XmlProtocol::new(&mut port);
        xml.ack(None).unwrap();
        assert_eq!(xml.read_ack(), Ok(()));

        let mut port = MockPort::new(flow_packet(b"OK@0x0\0"));
        let mut xml = XmlProtocol::new(&mut port);
        assert_eq!(xml.read_ack(), Ok(()));
    }

    #[test]
    fn read_ack_maps_unsupported_error() {
        let mut port = MockPort::new(flow_packet(b"ERR!UNSUPPORTED\0"));
        let mut xml = XmlProtocol::new(&mut port);
        let err = xml.read_ack().unwrap_err();
        assert!(err.is_unsupported(), "{:?}", err);
    }

    #[test]
    fn ack_with_value_uses_decimal() {
        let mut port = MockPort::new(vec![]);
        let handle = port.written_handle();
        let mut xml = XmlProtocol::new(&mut port);
        xml.ack(Some(4096)).unwrap();
        // Written bytes = header (12) + "OK@4096\0"
        let written = handle.borrow().clone();
        assert!(written.ends_with(b"OK@4096\0"));
        // Header length is correct
        let hdr = XmlPacketHeader::parse(&written[..12]).unwrap();
        assert_eq!(hdr.length as usize, b"OK@4096\0".len());
    }

    #[test]
    fn send_cmd_performs_lifetime_handshake() {
        // Script: CMD:START packet → (host replies OK) → command ACK "OK\0"
        let mut scripted = flow_packet(b"<command>CMD:START</command>");
        scripted.extend_from_slice(&flow_packet(b"OK\0"));
        let mut port = MockPort::new(scripted);
        let handle = port.written_handle();
        let mut xml = XmlProtocol::new(&mut port);

        let ok = xml.send_cmd(&cmd::reboot(false)).unwrap();
        assert!(ok, "设备应接受命令");
        let written = handle.borrow().clone();
        // First reply OK to CMD:START (with a 12-byte header), then send the XML command body
        assert!(written.windows(3).any(|w| w == b"OK\0"));
        assert!(written.windows(9).any(|w| w == b"CMD:REBOO"));
    }

    #[test]
    fn read_next_flow_header_skips_message_packets() {
        // A Message packet (4-byte length prefix + text) followed by a Flow packet
        let mut msg_payload = vec![0u8, 0, 0, 0];
        msg_payload.extend_from_slice(b"da log");
        let mut scripted = XmlPacketHeader {
            magic: CMD_MAGIC,
            data_type: DATA_TYPE_MESSAGE,
            length: msg_payload.len() as u32,
        }
        .to_bytes()
        .to_vec();
        scripted.extend_from_slice(&msg_payload);
        scripted.extend_from_slice(&flow_packet(b"payload"));

        let mut port = MockPort::new(scripted);
        let mut xml = XmlProtocol::new(&mut port);
        let data = xml.read_data().unwrap();
        assert_eq!(data, b"payload");
    }

    #[test]
    fn parse_ok_hex_handles_forms() {
        assert_eq!(parse_ok_hex("OK@0x1000\0").unwrap(), 0x1000);
        assert_eq!(parse_ok_hex("OK@0x0\0").unwrap(), 0);
        assert!(parse_ok_hex("OK@1000").is_err());
    }

    #[test]
    fn proto_error_is_unsupported_only_for_xml_unsupported() {
        let e = XmlProtoError::from(XmlError::from_kind(XmlErrorKind::UnsupportedCmd));
        assert!(e.is_unsupported());
        let e2 = XmlProtoError::proto("x");
        assert!(!e2.is_unsupported());
        assert!(e2.to_string().contains("XML 协议错误"));
    }
}

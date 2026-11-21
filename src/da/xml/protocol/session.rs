use std::io::{Read, Write};

use log::{debug, trace};

use crate::da::xml::cmd::{self, FileSystemOp, XmlCmdLifetime};
use crate::error::{XmlError, XmlErrorKind};

use super::*;

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

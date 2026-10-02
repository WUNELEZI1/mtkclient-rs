//! XML (V6) DA 协议会话层
//!
//! 对齐 penumbra `core/src/da/xml/protocol.rs`：在底层传输（[`BromTransport`]）
//! 之上实现 XML DA 的帧编解码与会话状态机。
//!
//! # 帧格式
//! 与 XFlash 相同：`magic(4) | data_type(4) | length(4)`，小端。
//! - `data_type = 1`（Flow）→ 正常流程包；
//! - `data_type = 2`（Message）→ DA 异步日志包，必须消费其负载，否则后续包头错位。
//!
//! # 命令生命周期
//! 每条命令前设备回发 `CMD:START`，命令执行后回发 `CMD:END`：
//! 1. 主机读取 `CMD:START` 并回 `OK\0`；
//! 2. 主机发送命令 XML（自带结尾 `\0`）；
//! 3. 设备回 `OK\0`（或 `ERR!...` 文本）；
//! 4. 需要时主机读 `CMD:END` 并回 `OK\0`。

#![allow(dead_code)] // V6 协议会话：CLI 目前仅接入 reboot 子集，其余待 v6 设备验证后启用

use std::fmt;
use std::io::{Read, Write};
use std::time::Duration;

use log::{debug, trace};

use crate::da::xflash::protocol::{CMD_MAGIC, DATA_TYPE_FLOW, DATA_TYPE_MESSAGE};
use crate::da::xml::cmd::{self, FileSystemOp, XmlCmdLifetime};
use crate::error::{XmlError, XmlErrorKind};
use crate::preloader::transport::BromTransport;

/// 会话默认/最短超时（与 penumbra `MIN_TIMEOUT` 一致）
pub const MIN_TIMEOUT: Duration = Duration::from_millis(1000);
/// 会话操作期间的长超时（与 penumbra `MAX_TIMEOUT` 一致）
pub const MAX_TIMEOUT: Duration = Duration::from_millis(10000);
/// 读取 flow 头时最多连续跳过多少个 Message 包（防止 DA 刷屏导致死循环）
const MAX_MESSAGE_DRAINS: u32 = 64;
/// 未协商包长前的发送分块上限
const DEFAULT_WRITE_CHUNK: usize = 0x8000;
/// 探测 USB 日志通道用的短超时
const USB_LOG_PROBE_TIMEOUT: Duration = Duration::from_millis(10);

// =============================================================================
// 错误
// =============================================================================

/// XML 协议会话错误
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlProtoError {
    /// 传输层（端口读写超时/失败）
    Transport(String),
    /// 协议层（帧头非法 / ACK 非法 / XML 解析失败）
    Protocol(String),
    /// 设备返回的 XML 错误
    Xml(XmlError),
}

impl XmlProtoError {
    pub(crate) fn proto<S: Into<String>>(msg: S) -> Self {
        XmlProtoError::Protocol(msg.into())
    }

    /// 转为扁平字符串（兼容既有 `Result<_, String>` 调用方）
    pub fn into_string(self) -> String {
        match self {
            XmlProtoError::Transport(m) => format!("USB 传输错误: {}", m),
            XmlProtoError::Protocol(m) => format!("XML 协议错误: {}", m),
            XmlProtoError::Xml(e) => e.to_string(),
        }
    }

    /// 是否为"命令不支持"错误
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
// 帧头
// =============================================================================

/// XML/XFlash 通用包头（12 字节 LE）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmlPacketHeader {
    /// 固定 magic（[`CMD_MAGIC`]）
    pub magic: u32,
    /// 包类型（Flow / Message）
    pub data_type: u32,
    /// 负载长度
    pub length: u32,
}

impl XmlPacketHeader {
    /// 包头固定长度
    pub const SIZE: usize = 12;

    /// Flow 包（正常流程）
    pub fn flow(length: u32) -> Self {
        XmlPacketHeader {
            magic: CMD_MAGIC,
            data_type: DATA_TYPE_FLOW,
            length,
        }
    }

    /// 解析包头；magic 不匹配或长度非法返回 `None`
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

    /// 序列化为 12 字节 LE
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..4].copy_from_slice(&self.magic.to_le_bytes());
        buf[4..8].copy_from_slice(&self.data_type.to_le_bytes());
        buf[8..12].copy_from_slice(&self.length.to_le_bytes());
        buf
    }

    /// 是否为设备异步消息包
    pub fn is_message(&self) -> bool {
        self.data_type == DATA_TYPE_MESSAGE
    }
}

// =============================================================================
// XML 轻量解析
// =============================================================================

/// 取标签路径对应的文本内容（如 `"arg/message"`）
pub fn get_tag(xml: &str, path: &str) -> Result<String, String> {
    let mut current = xml.to_string();
    for seg in path.split('/') {
        current = extract_tag(&current, seg).ok_or_else(|| format!("XML 标签 `{}` 未找到", seg))?;
    }
    Ok(current.trim().to_string())
}

/// 取标签路径并按十六进制解析（支持 `0x` 前缀），如 `"arg/packet_length"`
pub fn get_tag_usize(xml: &str, path: &str) -> Result<usize, String> {
    let raw = get_tag(xml, path)?;
    let trimmed = raw.trim_start_matches("0x");
    usize::from_str_radix(trimmed, 16)
        .map_err(|_| format!("XML 标签 `{}` 不是合法十六进制: {}", path, raw))
}

/// 提取第一个 `<tag>...</tag>` 的内部文本
fn extract_tag(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)? + start;
    Some(content[start..end].to_string())
}

// =============================================================================
// 会话
// =============================================================================

/// XML DA 协议会话
pub struct XmlProtocol<'a> {
    pub(crate) port: &'a mut dyn BromTransport,
    /// `CMD:DOWNLOAD-FILE` 协商出的包长；设置后发送按此分块
    pub write_packed_length: Option<usize>,
}

impl<'a> XmlProtocol<'a> {
    /// 绑定到一条已打开的传输端口
    pub fn new(port: &'a mut dyn BromTransport) -> Self {
        XmlProtocol {
            port,
            write_packed_length: None,
        }
    }

    // --- 传输原语 ---

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

    /// 读取下一帧 Flow 包头（跳过并消费途中的 Message 包）
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

    /// 消费一个 Message 包的负载（DA 异步日志）
    fn drain_message(&mut self, length: u32) -> Result<(), XmlProtoError> {
        if length == 0 {
            return Ok(());
        }
        let mut payload = vec![0u8; length as usize];
        self.port.read_exact(&mut payload)?;
        // 前 4 字节为长度头，其余为日志文本
        if payload.len() > 4 {
            trace!(
                "[XML DA Message] {}",
                String::from_utf8_lossy(&payload[4..])
            );
        }
        Ok(())
    }

    /// 读取一个完整 Flow 包的负载
    pub fn read_data(&mut self) -> Result<Vec<u8>, XmlProtoError> {
        let hdr = self.read_next_flow_header()?;
        let mut data = vec![0u8; hdr.length as usize];
        self.port.read_exact(&mut data)?;
        Ok(data)
    }

    /// 发送一段数据（自动加 Flow 包头并按协商包长分块）
    pub fn send(&mut self, data: &[u8]) -> Result<(), XmlProtoError> {
        self.send_data(&[data])
    }

    /// 发送多段数据，每段各带一个 Flow 包头
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

    /// 探测并丢弃 USB 日志通道的残留 Message 包，返回途中遇到的第一个 Flow 包头
    fn flush_usb_logs(&mut self) -> Result<Option<XmlPacketHeader>, XmlProtoError> {
        let prev = self.port.get_timeout();
        self.port.set_timeout(USB_LOG_PROBE_TIMEOUT);

        let mut result = Ok(None);
        loop {
            let mut buf = [0u8; XmlPacketHeader::SIZE];
            if self.port.read_exact(&mut buf).is_err() {
                break;
            }
            match XmlPacketHeader::parse(&buf) {
                None => break,
                Some(hdr) if hdr.is_message() => {
                    if let Err(e) = self.drain_message(hdr.length) {
                        result = Err(e);
                        break;
                    }
                }
                Some(hdr) => {
                    result = Ok(Some(hdr));
                    break;
                }
            }
        }

        self.port.set_timeout(prev);
        result
    }

    // --- ACK / 生命周期 ---

    /// 发送 ACK：无值发 `OK\0`，有值发 `OK@<v>\0`
    pub fn ack(&mut self, value: Option<usize>) -> Result<(), XmlProtoError> {
        match value {
            Some(v) => self.send(format!("OK@{}\0", v).as_bytes()),
            None => self.send(b"OK\0"),
        }
    }

    /// 读取设备 ACK（`OK\0` / `OK@0x0\0` 视为成功）
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

    /// 校验命令生命周期标记（`CMD:START` / `CMD:END`）
    pub fn check_lifetime(&mut self, lifetime: XmlCmdLifetime) -> Result<(), XmlProtoError> {
        let data = match self.read_data() {
            Ok(d) => d,
            // 复用既有会话时设备可能已发过 CMD:END，超时视为可接受
            Err(XmlProtoError::Transport(_)) => return Ok(()),
            Err(e) => return Err(e),
        };

        let text = String::from_utf8_lossy(&data);
        if text.contains("<result>ERR</result>") {
            let msg = get_tag(&text, "arg/message").unwrap_or_default();
            return Err(XmlError::from_message(msg.as_bytes()).into());
        }
        if !contains_bytes(&data, lifetime.pattern()) {
            return Err(XmlProtoError::proto("生命周期标记不匹配"));
        }
        Ok(())
    }

    /// 校验生命周期标记并回 ACK
    pub fn lifetime_ack(&mut self, lifetime: XmlCmdLifetime) -> Result<(), XmlProtoError> {
        let resp = self.check_lifetime(lifetime);
        self.ack(None)?;
        resp
    }

    /// 发送一条 XML 命令并读取设备 ACK
    ///
    /// 返回 `Ok(false)` 表示设备返回"不支持"（此时已消费 `CMD:END`）。
    pub fn send_cmd(&mut self, xml_cmd: &str) -> Result<bool, XmlProtoError> {
        self.lifetime_ack(XmlCmdLifetime::CmdStart)?;
        self.send(xml_cmd.as_bytes())?;
        trace!("[XML-TX] {}", xml_cmd);

        match self.read_ack() {
            Ok(()) => Ok(true),
            Err(e) if e.is_unsupported() => {
                // 不支持的命令：消费 CMD:END 保持状态机同步
                self.lifetime_ack(XmlCmdLifetime::CmdEnd)?;
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    // --- 数据收发 ---

    /// 接收设备上传的数据（先读取 `CMD:UPLOAD-FILE` 命令包）
    pub fn upload_data<W: Write, F: FnMut(usize, usize)>(
        &mut self,
        writer: &mut W,
        progress: &mut F,
    ) -> Result<usize, XmlProtoError> {
        let resp = self.read_data()?;
        let resp = String::from_utf8_lossy(&resp).into_owned();
        self.process_upload_data(&resp, writer, progress)
    }

    /// 读取 `CMD:UPLOAD-FILE` 的完整文本响应（用于 GET-HW-INFO 等）
    pub fn get_upload_file_resp(&mut self) -> Result<String, XmlProtoError> {
        let mut buf: Vec<u8> = Vec::new();
        self.upload_data(&mut buf, &mut |_, _| {})?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// 处理 `CMD:UPLOAD-FILE` 数据流
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

        // 1) 确认收到命令
        self.ack(None)?;
        // 2) 设备回报总大小 `OK@0x<size>\0`
        let size_resp = self.read_data()?;
        let size_text = String::from_utf8_lossy(&size_resp).into_owned();
        let size = parse_ok_hex(&size_text)?;
        // 3) 再次确认
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

    /// 发送数据给设备（先读取 `CMD:DOWNLOAD-FILE` 命令包）
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

    /// 处理 `CMD:DOWNLOAD-FILE` 数据流
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

        // 1) 确认收到命令
        self.ack(None)?;
        // 2) 告知要发送的大小
        self.ack(Some(size))?;
        // 3) 读取设备就绪 ACK
        self.read_ack()?;

        let packet_length =
            get_tag_usize(resp, "arg/packet_length").map_err(XmlProtoError::proto)?;
        // 协商包长后，send 不再拆成更小块
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
                // 分块 status 握手：ack(0) → read_ack → 数据 → read_ack
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

    /// 处理 `CMD:PROGRESS-REPORT`（擦除/写入进度，直到设备回 `OK!EOT`）
    pub fn progress_report<F: FnMut(usize, usize)>(
        &mut self,
        progress: &mut F,
    ) -> Result<(), XmlProtoError> {
        let resp = self.read_data()?;
        let resp = String::from_utf8_lossy(&resp).into_owned();
        self.process_progress_report(&resp, progress)
    }

    /// 处理 `CMD:PROGRESS-REPORT` 数据流
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

    /// 处理 `CMD:FILE-SYS-OPERATION`（读取设备请求并回 `OK@<op>\0`）
    pub fn file_system_op(&mut self, op: FileSystemOp) -> Result<(), XmlProtoError> {
        let resp = self.read_data()?;
        let resp = String::from_utf8_lossy(&resp).into_owned();
        self.process_file_sys_op(&resp, op)
    }

    /// 处理 `CMD:FILE-SYS-OPERATION` 响应
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

    // --- 内部工具 ---

    /// 校验响应命令名；不符时按 `arg/message` 给出更具体的错误
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
    /// 便捷构造（供 `XmlProtoError` 的 `into()` 使用）
    fn from_kind_fallback(kind: XmlErrorKind) -> XmlError {
        XmlError::from_kind(kind)
    }
}

/// 解析 `OK@0x<hex>\0` 形式的大小回报
fn parse_ok_hex(text: &str) -> Result<usize, XmlProtoError> {
    let trimmed = text.trim_end_matches('\0').trim();
    let hex = trimmed
        .strip_prefix("OK@0x")
        .ok_or_else(|| XmlProtoError::proto(format!("非法大小回报: {:?}", trimmed)))?;
    usize::from_str_radix(hex, 16)
        .map_err(|_| XmlProtoError::proto(format!("大小解析失败: {:?}", hex)))
}

/// 子串查找（避免额外依赖）
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    /// 可脚本化的模拟端口
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
                timeout: MIN_TIMEOUT,
            }
        }

        /// 共享写缓冲句柄；在借用端口前取得，便于事后断言
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

    /// 构造一个 Framed Flow 包（头 + 负载）
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
        // 写入内容 = 帧头(12) + "OK@4096\0"
        let written = handle.borrow().clone();
        assert!(written.ends_with(b"OK@4096\0"));
        // 帧头长度正确
        let hdr = XmlPacketHeader::parse(&written[..12]).unwrap();
        assert_eq!(hdr.length as usize, b"OK@4096\0".len());
    }

    #[test]
    fn send_cmd_performs_lifetime_handshake() {
        // 脚本：CMD:START 包 → (主机回 OK) → 命令 ACK "OK\0"
        let mut scripted = flow_packet(b"<command>CMD:START</command>");
        scripted.extend_from_slice(&flow_packet(b"OK\0"));
        let mut port = MockPort::new(scripted);
        let handle = port.written_handle();
        let mut xml = XmlProtocol::new(&mut port);

        let ok = xml.send_cmd(&cmd::reboot(false)).unwrap();
        assert!(ok, "设备应接受命令");
        let written = handle.borrow().clone();
        // 先回 CMD:START 的 OK（带 12 字节帧头），再发送 XML 命令本体
        assert!(written.windows(3).any(|w| w == b"OK\0"));
        assert!(written.windows(9).any(|w| w == b"CMD:REBOO"));
    }

    #[test]
    fn read_next_flow_header_skips_message_packets() {
        // Message 包（前 4 字节长度头 + 文本）后跟一个 Flow 包
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
        assert!(e2.into_string().contains("XML 协议错误"));
    }
}

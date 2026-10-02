//! 基于 XML 协议的分区读 / 写 / 擦 / 格式化
//!
//! 对齐 penumbra `core/src/da/xml/flash.rs`：把 [`XmlProtocol`] 的收发原语编排
//! 成完整的闪存操作。与 penumbra 的差异是会话对象 [`XmlProtocol`] 内部持有
//! 传输端口，因此这里不再额外传 `port`。
//!
//! 每个操作都遵循 XML 命令生命周期：`CMD:START` → 命令 → 数据交互 →
//! `CMD:END`（由 [`XmlProtocol::send_cmd`] 与 `lifetime_ack` 内部处理）。

#![allow(dead_code)] // V6 闪存操作：CLI 目前仅接入 reboot 子集，其余待 v6 设备验证后启用

use std::io::{Read, Write};

use log::{debug, warn};

use crate::da::xml::cmd::{self, FileSystemOp, XmlCmdLifetime};
use crate::da::xml::protocol::{XmlProtoError, XmlProtocol, get_tag};

/// scatter 流程中记录保护分区信息的特殊文件名
pub const RECORD_FILE: &str = "record-file";

/// `READ-FLASH`：按地址/长度读取，数据回写到 `writer`
///
/// 返回实际接收的字节数。
pub fn read_flash<W, F>(
    xml: &mut XmlProtocol<'_>,
    section: &str,
    offset: u64,
    size: usize,
    writer: &mut W,
    progress: &mut F,
) -> Result<usize, XmlProtoError>
where
    W: Write,
    F: FnMut(usize, usize),
{
    debug!(
        "[XML] READ-FLASH 分区={} 地址=0x{:X} 长度=0x{:X}",
        section, offset, size
    );

    xml.send_cmd(&cmd::read_flash(section, size, offset))?;
    let read = xml.upload_data(writer, progress)?;
    xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;

    debug!("[XML] READ-FLASH 完成，共 0x{:X} 字节", read);
    Ok(read)
}

/// `WRITE-FLASH`：按地址/长度写入，数据来自 `reader`
pub fn write_flash<R, F>(
    xml: &mut XmlProtocol<'_>,
    section: &str,
    offset: u64,
    size: usize,
    reader: &mut R,
    progress: &mut F,
) -> Result<usize, XmlProtoError>
where
    R: Read,
    F: FnMut(usize, usize),
{
    debug!(
        "[XML] WRITE-FLASH 分区={} 地址=0x{:X} 长度=0x{:X}",
        section, offset, size
    );

    xml.send_cmd(&cmd::write_flash(section, size, offset))?;
    // DA 先询问目标文件大小
    xml.file_system_op(FileSystemOp::FileSize(size))?;
    // 写入前的预擦除进度上报
    let mut noop = |_: usize, _: usize| {};
    xml.progress_report(&mut noop)?;
    let sent = xml.download_data(size, reader, progress)?;
    xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;

    debug!("[XML] WRITE-FLASH 完成，共 0x{:X} 字节", sent);
    Ok(sent)
}

/// `ERASE-FLASH`：按地址/长度擦除
pub fn erase_flash<F>(
    xml: &mut XmlProtocol<'_>,
    section: &str,
    offset: u64,
    size: usize,
    progress: &mut F,
) -> Result<(), XmlProtoError>
where
    F: FnMut(usize, usize),
{
    debug!(
        "[XML] ERASE-FLASH 分区={} 地址=0x{:X} 长度=0x{:X}",
        section, offset, size
    );

    xml.send_cmd(&cmd::erase_flash(section, size, offset))?;
    xml.progress_report(progress)?;
    xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;

    debug!("[XML] ERASE-FLASH 完成");
    Ok(())
}

/// `READ-PARTITION`：整分区回读
pub fn read_partition<W, F>(
    xml: &mut XmlProtocol<'_>,
    part_name: &str,
    writer: &mut W,
    progress: &mut F,
) -> Result<usize, XmlProtoError>
where
    W: Write,
    F: FnMut(usize, usize),
{
    debug!("[XML] READ-PARTITION 分区={}", part_name);

    xml.send_cmd(&cmd::read_partition(part_name))?;
    let read = xml.upload_data(writer, progress)?;
    xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;

    debug!("[XML] READ-PARTITION 完成，共 0x{:X} 字节", read);
    Ok(read)
}

/// `FORMAT-PARTITION`（ERASE-PARTITION）：格式化整个分区
pub fn format_partition<F>(
    xml: &mut XmlProtocol<'_>,
    part_name: &str,
    progress: &mut F,
) -> Result<(), XmlProtoError>
where
    F: FnMut(usize, usize),
{
    debug!("[XML] 格式化分区 {}", part_name);

    xml.send_cmd(&cmd::erase_partition(part_name))?;
    xml.progress_report(progress)?;
    xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;

    debug!("[XML] 分区 {} 格式化完成", part_name);
    Ok(())
}

/// `WRITE-PARTITION`：整分区写入
///
/// `WRITE-PARTITION` 是一段持续交互：设备会依次下发 `PROGRESS-REPORT`（预擦除）、
/// `FILE-SYS-OPERATION`（询问文件大小）、`DOWNLOAD-FILE`（索要数据），最后以
/// `CMD:END` 结束。本函数按此状态机循环，直到收到 `CMD:END`。
pub fn write_partition<R, F>(
    xml: &mut XmlProtocol<'_>,
    part_name: &str,
    size: usize,
    reader: &mut R,
    progress: &mut F,
) -> Result<(), XmlProtoError>
where
    R: Read,
    F: FnMut(usize, usize),
{
    debug!("[XML] WRITE-PARTITION 分区={} 长度=0x{:X}", part_name, size);

    xml.send_cmd(&cmd::write_partition(part_name))?;

    let mut noop = |_: usize, _: usize| {};
    loop {
        let resp = xml.read_data()?;

        // 收到 CMD:END：回 ACK 并校验最终结果
        if contains(&resp, cmd::CMD_END) {
            xml.ack(None)?;
            check_end_result(&resp)?;
            break;
        }

        let text = String::from_utf8_lossy(&resp).into_owned();
        let cmd_name = get_tag(&text, "command").map_err(XmlProtoError::proto)?;

        match cmd_name.as_str() {
            cmd::CMD_PROGRESS_REPORT => {
                xml.process_progress_report(&text, &mut noop)?;
            }
            cmd::CMD_FILE_SYSTEM_OP => {
                let op = get_tag(&text, "arg/key")
                    .map(|s| FileSystemOp::from(s.as_str()))
                    .unwrap_or(FileSystemOp::Exists);
                xml.process_file_sys_op(&text, op)?;
            }
            cmd::CMD_DOWNLOAD_FILE => {
                xml.process_download_data(
                    &text,
                    size,
                    crate::da::xml::protocol::MAX_TIMEOUT,
                    reader,
                    progress,
                )?;
            }
            other => {
                warn!("[XML] WRITE-PARTITION 收到未知命令: {}", other);
                return Err(XmlProtoError::Xml(crate::error::XmlError::from_kind(
                    crate::error::XmlErrorKind::UnsupportedCmd,
                )));
            }
        }
    }

    debug!("[XML] WRITE-PARTITION 完成");
    Ok(())
}

/// 校验 `CMD:END` 报文中的 `arg/result`；非 `OK` 时抛出设备错误
fn check_end_result(resp: &[u8]) -> Result<(), XmlProtoError> {
    let text = String::from_utf8_lossy(resp);
    let result = get_tag(&text, "arg/result").unwrap_or_default();

    if result == "OK" {
        return Ok(());
    }

    let message = get_tag(&text, "arg/message").unwrap_or_default();
    Err(crate::error::XmlError::from_message(message.as_bytes()).into())
}

/// 子串查找（避免额外依赖）
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
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
    use crate::da::xflash::protocol::{CMD_MAGIC, DATA_TYPE_FLOW, DATA_TYPE_MESSAGE};
    use crate::da::xml::protocol::XmlPacketHeader;
    use crate::preloader::transport::BromTransport;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    /// 可脚本化的模拟端口（读脚本 + 记录写入）
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
            if self.pos >= self.read_buf.len() {
                return Ok(0);
            }
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

    fn flow(payload: &[u8]) -> Vec<u8> {
        let mut v = XmlPacketHeader::flow(payload.len() as u32)
            .to_bytes()
            .to_vec();
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn read_flash_drives_upload_and_end() {
        // 脚本：CMD:START → 命令 ACK → UPLOAD-FILE 命令 → OK@size → 数据 → CMD:END
        let mut scripted = Vec::new();
        scripted.extend_from_slice(&flow(b"<command>CMD:START</command>"));
        scripted.extend_from_slice(&flow(b"OK\0"));
        scripted.extend_from_slice(&flow(
            b"<da><command>CMD:UPLOAD-FILE</command>\
              <arg><packet_length>0x4</packet_length></arg></da>",
        ));
        scripted.extend_from_slice(&flow(b"OK@0x4\0"));
        scripted.extend_from_slice(&flow(b"OK\0")); // 分块前 ACK
        scripted.extend_from_slice(&flow(b"DATA"));
        scripted.extend_from_slice(&flow(b"<command>CMD:END</command>"));

        let mut port = MockPort::new(scripted);
        let handle = port.written_handle();
        let mut xml = XmlProtocol::new(&mut port);
        let mut out = Vec::new();
        let mut progress = |_: usize, _: usize| {};

        let n = read_flash(&mut xml, "boot", 0x0, 4, &mut out, &mut progress).unwrap();
        assert_eq!(n, 4);
        assert_eq!(out, b"DATA");
        // 命令体应为 READ-FLASH
        assert!(contains(&handle.borrow(), b"CMD:READ-FLASH"));
    }

    #[test]
    fn read_next_flow_header_skips_message_packet() {
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
        scripted.extend_from_slice(&flow(b"<command>CMD:END</command>"));

        let mut port = MockPort::new(scripted);
        let mut xml = XmlProtocol::new(&mut port);
        let resp = xml.read_data().unwrap();
        assert!(contains(&resp, cmd::CMD_END));
        assert_eq!(DATA_TYPE_FLOW, 1);
    }

    #[test]
    fn check_end_result_maps_message() {
        assert!(check_end_result(b"<arg><result>OK</result></arg>").is_ok());
        let err = check_end_result(b"<arg><result>ERR</result><message>ERR!CANCEL</message></arg>")
            .unwrap_err();
        assert!(err.to_string().contains("取消"), "{}", err);
    }

    #[test]
    fn write_flash_runs_prescribed_sequence() {
        // WRITE-FLASH：CMD:START/ACK → FILE-SYS-OP → PROGRESS-REPORT → DOWNLOAD-FILE → CMD:END
        let mut scripted = Vec::new();
        scripted.extend_from_slice(&flow(b"<command>CMD:START</command>"));
        scripted.extend_from_slice(&flow(b"OK\0"));
        scripted.extend_from_slice(&flow(
            b"<da><command>CMD:FILE-SYS-OPERATION</command><arg><key>1F40</key></arg></da>",
        ));
        scripted.extend_from_slice(&flow(
            b"<da><command>CMD:PROGRESS-REPORT</command><arg><message>erase</message></arg></da>",
        ));
        scripted.extend_from_slice(&flow(b"OK!EOT\0"));
        scripted.extend_from_slice(&flow(
            b"<da><command>CMD:DOWNLOAD-FILE</command>\
              <arg><packet_length>0x4</packet_length></arg></da>",
        ));
        // 分块握手：主机 ack(size) → 设备 OK → 主机 ack(0) → 设备 OK → 数据 → 设备 OK
        scripted.extend_from_slice(&flow(b"OK\0"));
        scripted.extend_from_slice(&flow(b"OK\0"));
        scripted.extend_from_slice(&flow(b"OK\0"));
        scripted.extend_from_slice(&flow(b"<command>CMD:END</command>"));

        let mut port = MockPort::new(scripted);
        let handle = port.written_handle();
        let mut xml = XmlProtocol::new(&mut port);
        let data = [0xAAu8; 4];
        let mut reader = &data[..];
        let mut progress = |_: usize, _: usize| {};

        let sent = write_flash(&mut xml, "boot", 0, 4, &mut reader, &mut progress).unwrap();
        assert_eq!(sent, 4);
        assert!(contains(&handle.borrow(), b"CMD:WRITE-FLASH"));
    }

    #[test]
    fn write_partition_loop_handles_download_then_end() {
        // 先 DOWNLOAD-FILE 再 CMD:END
        let mut scripted = Vec::new();
        scripted.extend_from_slice(&flow(b"<command>CMD:START</command>"));
        scripted.extend_from_slice(&flow(b"OK\0"));
        scripted.extend_from_slice(&flow(
            b"<da><command>CMD:DOWNLOAD-FILE</command>\
              <arg><packet_length>0x2</packet_length></arg></da>",
        ));
        scripted.extend_from_slice(&flow(b"OK\0")); // 回 size
        scripted.extend_from_slice(&flow(b"OK\0")); // 回 ack(0)
        scripted.extend_from_slice(&flow(b"OK\0")); // 数据后确认
        scripted.extend_from_slice(&flow(
            b"<arg><result>OK</result></arg><command>CMD:END</command>",
        ));

        let mut port = MockPort::new(scripted);
        let handle = port.written_handle();
        let mut xml = XmlProtocol::new(&mut port);
        let data = [0x11u8, 0x22];
        let mut reader = &data[..];
        let mut progress = |_: usize, _: usize| {};

        write_partition(&mut xml, "boot", 2, &mut reader, &mut progress).unwrap();
        assert!(contains(&handle.borrow(), b"CMD:WRITE-PARTITION"));
    }

    #[test]
    fn format_partition_sends_erase_and_progress() {
        let mut scripted = Vec::new();
        scripted.extend_from_slice(&flow(b"<command>CMD:START</command>"));
        scripted.extend_from_slice(&flow(b"OK\0"));
        scripted.extend_from_slice(&flow(
            b"<da><command>CMD:PROGRESS-REPORT</command><arg><message>format</message></arg></da>",
        ));
        scripted.extend_from_slice(&flow(b"OK!EOT\0"));
        scripted.extend_from_slice(&flow(b"<command>CMD:END</command>"));

        let mut port = MockPort::new(scripted);
        let handle = port.written_handle();
        let mut xml = XmlProtocol::new(&mut port);
        let mut progress = |_: usize, _: usize| {};

        format_partition(&mut xml, "userdata", &mut progress).unwrap();
        assert!(contains(&handle.borrow(), b"CMD:ERASE-PARTITION"));
        assert!(contains(&handle.borrow(), b"userdata"));
    }
}

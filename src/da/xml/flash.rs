//! XML-protocol based partition read / write / erase / format
//!
//! Mirrors penumbra `core/src/da/xml/flash.rs`: composes the [`XmlProtocol`] send/receive
//! primitives into complete flash operations. Unlike penumbra, the session object
//! [`XmlProtocol`] owns the transport, so no `port` is passed in here.
//!
//! Every operation follows the XML command lifecycle: `CMD:START` → command → data exchange →
//! `CMD:END` (handled internally by [`XmlProtocol::send_cmd`] and `lifetime_ack`).

use std::io::{Read, Write};

use log::{debug, warn};

use crate::da::xml::cmd::{self, FileSystemOp, XmlCmdLifetime};
use crate::da::xml::protocol::{XmlProtoError, XmlProtocol, get_tag};

/// `READ-FLASH`: read by address/length and write the data into `writer`
///
/// Returns the number of bytes actually received.
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

/// `WRITE-FLASH`: write by address/length with data from `reader`
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
    // The DA first asks for the target file size
    xml.file_system_op(FileSystemOp::FileSize(size))?;
    // Pre-erase progress report before writing
    let mut noop = |_: usize, _: usize| {};
    xml.progress_report(&mut noop)?;
    let sent = xml.download_data(size, reader, progress)?;
    xml.lifetime_ack(XmlCmdLifetime::CmdEnd)?;

    debug!("[XML] WRITE-FLASH 完成，共 0x{:X} 字节", sent);
    Ok(sent)
}

/// `ERASE-FLASH`: erase by address/length
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

/// `READ-PARTITION`: read back a whole partition
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

/// `FORMAT-PARTITION` (ERASE-PARTITION): format a whole partition
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

/// `WRITE-PARTITION`: write a whole partition
///
/// `WRITE-PARTITION` is a continuous exchange: the device sends `PROGRESS-REPORT` (pre-erase),
/// `FILE-SYS-OPERATION` (file size query) and `DOWNLOAD-FILE` (data request), ending with
/// `CMD:END`. This function loops over that state machine until `CMD:END` arrives.
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

        // On CMD:END: reply with an ACK and check the final result
        if contains(&resp, &cmd::cmd_end()) {
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

/// Check `arg/result` in the `CMD:END` packet; raise a device error when it is not `OK`
fn check_end_result(resp: &[u8]) -> Result<(), XmlProtoError> {
    let text = String::from_utf8_lossy(resp);
    let result = get_tag(&text, "arg/result").unwrap_or_default();

    if result == "OK" {
        return Ok(());
    }

    let message = get_tag(&text, "arg/message").unwrap_or_default();
    Err(crate::error::XmlError::from_message(message.as_bytes()).into())
}

/// Substring search (avoids extra dependencies)
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
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
    use crate::da::xflash::protocol::{CMD_MAGIC, DATA_TYPE_FLOW, DATA_TYPE_MESSAGE};
    use crate::da::xml::protocol::XmlPacketHeader;
    use crate::preloader::transport::BromTransport;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    /// Scriptable mock port (scripted reads + recorded writes)
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

        /// Shared write-buffer handle; take it before borrowing the port for later assertions
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
        // Script: CMD:START → command ACK → UPLOAD-FILE → OK@size → data → CMD:END
        let mut scripted = Vec::new();
        scripted.extend_from_slice(&flow(b"<command>CMD:START</command>"));
        scripted.extend_from_slice(&flow(b"OK\0"));
        scripted.extend_from_slice(&flow(
            b"<da><command>CMD:UPLOAD-FILE</command>\
              <arg><packet_length>0x4</packet_length></arg></da>",
        ));
        scripted.extend_from_slice(&flow(b"OK@0x4\0"));
        scripted.extend_from_slice(&flow(b"OK\0")); // ACK before the chunk
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
        // The command body should be READ-FLASH
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
        assert!(contains(&resp, &cmd::cmd_end()));
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
        // WRITE-FLASH: CMD:START/ACK → FILE-SYS-OP → PROGRESS-REPORT → DOWNLOAD-FILE → CMD:END
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
        // Chunk handshake: host ack(size) → device OK → host ack(0) → device OK → data → device OK
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
        // DOWNLOAD-FILE first, then CMD:END
        let mut scripted = Vec::new();
        scripted.extend_from_slice(&flow(b"<command>CMD:START</command>"));
        scripted.extend_from_slice(&flow(b"OK\0"));
        scripted.extend_from_slice(&flow(
            b"<da><command>CMD:DOWNLOAD-FILE</command>\
              <arg><packet_length>0x2</packet_length></arg></da>",
        ));
        scripted.extend_from_slice(&flow(b"OK\0")); // reply size
        scripted.extend_from_slice(&flow(b"OK\0")); // reply ack(0)
        scripted.extend_from_slice(&flow(b"OK\0")); // confirm after data
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

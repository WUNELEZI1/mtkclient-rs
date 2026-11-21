use super::*;
use crate::da::xflash::protocol::{CMD_MAGIC, DATA_TYPE_MESSAGE};
use crate::da::xml::cmd;
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

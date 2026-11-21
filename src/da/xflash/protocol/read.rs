//! Protocol primitives — block 1: status / data reads

use super::{CMD_MAGIC, DA_CTRL_TIMEOUT_MS, DATA_TYPE_MESSAGE};
use log::{debug, trace};
use std::time::Duration;

use crate::da::xflash::DAXFlash;

/// XFlash header length: magic(4) + data_type(4) + length(4)
const XFLASH_HEADER_LEN: usize = 12;
/// Maximum number of Message packets skipped in a single response read.
/// Defensive limit: a healthy DA never emits this many log packets in a row, so exceeding it
/// means the protocol is out of sync; this prevents an unbounded loop on a faulty device.
const MAX_MESSAGE_PACKETS_PER_READ: u32 = 64;

/// Reasonable upper bound for a single packet length (64 MiB). A defensive sanity check only:
/// normal DA packets are far smaller (2~4 MiB after negotiation); a misparsed header could
/// yield a length such as 0xFFFFFFFF, and allocating it would OOM/panic, so reject it up front.
const MAX_XFLASH_PACKET_LEN: u32 = 64 * 1024 * 1024;

/// Whether the header-declared length is within a sane range (pure function, easy to unit test)
pub(crate) fn packet_length_is_valid(length: u32) -> bool {
    length <= MAX_XFLASH_PACKET_LEN
}

/// What to do after receiving one packet
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PacketAction {
    /// Device asynchronous message packet: consume and log the payload, then keep reading
    SkipMessage,
    /// Flow packet (or unknown type): return it as the response (stay lenient with unknown types,
    /// as the old implementation did, to avoid breaking extension types from newer firmware)
    Deliver,
}

/// Decide the action from the header `data_type` (pure function, easy to unit test)
pub(crate) fn classify_packet(data_type: u32) -> PacketAction {
    if data_type == DATA_TYPE_MESSAGE {
        PacketAction::SkipMessage
    } else {
        PacketAction::Deliver
    }
}

/// Render a device asynchronous message payload as readable text (pure function, easy to unit test)
pub(crate) fn format_device_message(payload: &[u8]) -> String {
    String::from_utf8_lossy(payload)
        .trim_end_matches(['\0', '\r', '\n'])
        .to_string()
}

/// Handle one already-read packet:
/// - Message packet: log it, increment `skipped`, return `Ok(None)` (keep reading)
/// - Flow packet: return `Ok(Some(payload))` (the response)
/// - Too many consecutive Message packets: return `Err` (protocol out of sync)
///
/// Pure function (no I/O), so packet skipping and the limit are easy to unit test.
fn handle_packet(
    data_type: u32,
    payload: Vec<u8>,
    skipped: &mut u32,
) -> Result<Option<Vec<u8>>, String> {
    match classify_packet(data_type) {
        PacketAction::SkipMessage => {
            *skipped += 1;
            debug!(
                "[xread] 设备异步消息(len={}): {}",
                payload.len(),
                format_device_message(&payload)
            );
            if *skipped >= MAX_MESSAGE_PACKETS_PER_READ {
                return Err(
                    crate::error::ProtocolError::MessageFlood { count: *skipped }.to_string(),
                );
            }
            Ok(None)
        }
        PacketAction::Deliver => Ok(Some(payload)),
    }
}

impl<'a> DAXFlash<'a> {
    /// Read XFlash protocol data.
    /// Flow: read the 12-byte header (magic + type + length) → validate magic → read the data
    /// Returns: length of the data read (a u32 value when the payload is 4 bytes)
    pub(crate) fn xread(&mut self) -> Result<u32, String> {
        // The device may need hundreds of milliseconds after setup_hw_init before responding.
        // Wait for the first byte using max(current timeout, DA_CTRL_TIMEOUT_MS) to avoid status
        // timeouts on slow serial links; keep a shorter caller timeout (with_short_timeout fast fail).
        let orig_timeout = self.preloader.device.get_timeout();
        let eff = orig_timeout.max(Duration::from_millis(DA_CTRL_TIMEOUT_MS));
        self.preloader.device.set_timeout(eff);

        let result = self.xread_inner();
        self.preloader.device.set_timeout(orig_timeout);
        result
    }

    fn xread_inner(&mut self) -> Result<u32, String> {
        let payload = self.read_flow_packet()?;
        // If the payload is 4 bytes, return it as a u32 value
        if payload.len() == 4 {
            let val = u32::from_le_bytes(payload[0..4].try_into().unwrap());
            trace!("[xread] val=0x{:08X}", val);
            return Ok(val);
        }
        Ok(0)
    }

    /// Read a single XFlash packet and return (data_type, payload)
    fn read_packet(&mut self) -> Result<(u32, Vec<u8>), String> {
        let mut header = [0u8; XFLASH_HEADER_LEN];
        self.preloader.device.read_exact(&mut header)?;

        let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let data_type = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let length = u32::from_le_bytes(header[8..12].try_into().unwrap());

        if magic != CMD_MAGIC {
            trace!("[xread] bad magic: 0x{:08X}", magic);
            return Err(crate::error::ProtocolError::BadMagic {
                got: magic,
                expected: CMD_MAGIC,
            }
            .to_string());
        }

        if !packet_length_is_valid(length) {
            trace!("[xread] invalid length: {}", length);
            return Err(crate::error::ProtocolError::InvalidPacketLength(length).to_string());
        }

        trace!("[xread] dt={:08X} len={}", data_type, length);

        let mut payload = vec![0u8; length as usize];
        if length > 0 {
            self.preloader.device.read_exact(&mut payload)?;
        }
        Ok((data_type, payload))
    }

    /// Read the next flow packet, skipping and logging device asynchronous Message packets (data_type=2).
    ///
    /// The DA may inject log/status message packets at any time; the old implementation consumed
    /// their payload as the command response, misaligning later headers (protocol desync). Here
    /// `data_type` is checked after header parsing: Message packets are consumed and logged until a flow packet arrives.
    /// Mirrors penumbra `read_next_flow_header()` + `drain_message()`.
    fn read_flow_packet(&mut self) -> Result<Vec<u8>, String> {
        let mut skipped = 0u32;
        loop {
            let (data_type, payload) = self.read_packet()?;
            if let Some(payload) = handle_packet(data_type, payload, &mut skipped)? {
                return Ok(payload);
            }
        }
    }

    /// Read the status (4-byte little-endian)
    pub(crate) fn status(&mut self) -> Result<u32, String> {
        // DA control read: on slow serial links the DA may take longer than the default 1s to return status;
        // wait using max(current, DA_CTRL_TIMEOUT_MS) to avoid "Operation timed out". A shorter caller timeout is kept.
        let orig_timeout = self.preloader.device.get_timeout();
        let eff = orig_timeout.max(Duration::from_millis(DA_CTRL_TIMEOUT_MS));
        self.preloader.device.set_timeout(eff);

        let result = (|| {
            // Skip DA asynchronous message packets before taking the flow packet (so their payload is not parsed as status)
            let payload = self.read_flow_packet()?;
            match payload.len() {
                4 => {
                    let val = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                    // Python special case: return 0 if status == 0xFEEEEEEF
                    if val == 0xFEEEEEEF {
                        trace!("[status] 0xFEEEEEEF → 0");
                        return Ok(0);
                    }
                    trace!("[status] val=0x{:08X}", val);
                    Ok(val)
                }
                2 => {
                    let val = u16::from_le_bytes(payload[0..2].try_into().unwrap()) as u32;
                    trace!("[status] val=0x{:04X}", val);
                    Ok(val)
                }
                other => {
                    trace!("[status] len={} (no u32/u16 payload)", other);
                    Ok(0)
                }
            }
        })();

        self.preloader.device.set_timeout(orig_timeout);
        result
    }

    /// Read XFlash data and return a Vec
    pub(crate) fn xread_data(&mut self) -> Result<Vec<u8>, String> {
        // DA control read: same as status(); use max(current, DA_CTRL_TIMEOUT_MS) to avoid slow-serial timeouts
        let orig_timeout = self.preloader.device.get_timeout();
        let eff = orig_timeout.max(Duration::from_millis(DA_CTRL_TIMEOUT_MS));
        self.preloader.device.set_timeout(eff);

        let result = (|| {
            // Skip DA asynchronous message packets before taking the flow packet
            let payload = self.read_flow_packet()?;
            trace!("[xread_data] len={}", payload.len());
            Ok(payload)
        })();

        self.preloader.device.set_timeout(orig_timeout);
        result
    }
}

// =============================================================================
// Unit tests — Message packet handling (P0-1)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::da::xflash::protocol::{DATA_TYPE_FLOW, DATA_TYPE_MESSAGE};

    #[test]
    fn classify_packet_distinguishes_message_and_flow() {
        assert_eq!(
            classify_packet(DATA_TYPE_MESSAGE),
            PacketAction::SkipMessage
        );
        assert_eq!(classify_packet(DATA_TYPE_FLOW), PacketAction::Deliver);
        // Unknown types stay lenient and are treated as flow packets (same as the old behavior)
        assert_eq!(classify_packet(0x55), PacketAction::Deliver);
        assert_eq!(classify_packet(0), PacketAction::Deliver);
    }

    #[test]
    fn format_device_message_trims_trailing_control_bytes() {
        assert_eq!(format_device_message(b"hello\n"), "hello");
        assert_eq!(format_device_message(b"hello\0"), "hello");
        assert_eq!(format_device_message(b"hello\r\n"), "hello");
        assert_eq!(format_device_message(b""), "");
        // Non-UTF-8 input degrades safely (lossy)
        assert_eq!(format_device_message(&[0xFF, b'o', b'k']), "\u{FFFD}ok");
    }

    #[test]
    fn handle_packet_skips_message_then_delivers_flow() {
        let mut skipped = 0u32;
        // Two message packets → returns None (keep reading)
        assert!(
            handle_packet(DATA_TYPE_MESSAGE, b"log1".to_vec(), &mut skipped)
                .unwrap()
                .is_none()
        );
        assert!(
            handle_packet(DATA_TYPE_MESSAGE, b"log2".to_vec(), &mut skipped)
                .unwrap()
                .is_none()
        );
        assert_eq!(skipped, 2);
        // Flow packet → returns the payload, and skipped stops growing
        let delivered = handle_packet(DATA_TYPE_FLOW, vec![1, 2, 3, 4], &mut skipped)
            .unwrap()
            .unwrap();
        assert_eq!(delivered, vec![1, 2, 3, 4]);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn handle_packet_errors_on_message_flood() {
        let mut skipped = 0u32;
        // MAX-1 consecutive message packets should still be skipped normally
        for _ in 0..(MAX_MESSAGE_PACKETS_PER_READ - 1) {
            assert!(
                handle_packet(DATA_TYPE_MESSAGE, vec![], &mut skipped)
                    .unwrap()
                    .is_none()
            );
        }
        // The MAX-th packet triggers the protocol desync error
        let err = handle_packet(DATA_TYPE_MESSAGE, vec![], &mut skipped).unwrap_err();
        assert!(err.contains("Message"), "unexpected error: {}", err);
    }

    #[test]
    fn packet_length_guard_rejects_absurd_lengths() {
        assert!(packet_length_is_valid(0));
        assert!(packet_length_is_valid(0x200000)); // 2MiB is a normal value
        assert!(packet_length_is_valid(MAX_XFLASH_PACKET_LEN));
        // Absurd lengths from misparsed headers are rejected, avoiding a vec![0u8; 4GiB] OOM
        assert!(!packet_length_is_valid(MAX_XFLASH_PACKET_LEN + 1));
        assert!(!packet_length_is_valid(0xFFFFFFFF));
    }

    #[test]
    fn read_packet_header_layout_matches_pack3() {
        // Headers produced by pack3 must match the field offsets used when reading (regression guard)
        let pkt = crate::da::xflash::protocol::pack3(CMD_MAGIC, DATA_TYPE_MESSAGE, 5);
        let magic = u32::from_le_bytes(pkt[0..4].try_into().unwrap());
        let data_type = u32::from_le_bytes(pkt[4..8].try_into().unwrap());
        let length = u32::from_le_bytes(pkt[8..12].try_into().unwrap());
        assert_eq!(magic, CMD_MAGIC);
        assert_eq!(data_type, DATA_TYPE_MESSAGE);
        assert_eq!(length, 5);
        assert_eq!(XFLASH_HEADER_LEN, pkt.len());
    }
}

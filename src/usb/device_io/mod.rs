//! USB 设备 IO 操作：读取 / 写入 / 控制传输 / 清除停顿
//!
//! 使用 nusb Interface 的 control_transfer_in/out 和 bulk transfer_blocking。
//! 后续阶段 2 会将 bulk 读取改为异步流水线（submit/complete）。

pub(crate) mod control;
pub(crate) mod read;
pub(crate) mod write;

use crate::usb::device::UsbDevice;
use nusb::transfer::Recipient;
use std::time::Duration;

const CANCEL_TRANSFER_DRAIN_TIMEOUT: Duration = Duration::from_millis(20);
const CANCEL_TRANSFER_MAX_DRAINS: usize = 8;

pub(crate) fn bulk_in_submit_len(requested_len: usize, max_packet_size: u16) -> usize {
    let packet_size = usize::from(max_packet_size).max(1);
    if requested_len == 0 {
        0
    } else {
        requested_len.div_ceil(packet_size) * packet_size
    }
}

pub(crate) fn control_index_for_recipient(
    bm_request_type: u8,
    requested_index: u16,
    interface_number: u8,
) -> u16 {
    match UsbDevice::convert_receiver(bm_request_type) {
        Recipient::Interface => u16::from(interface_number),
        _ => requested_index,
    }
}

pub(crate) fn copy_from_bulk_packet(packet: &[u8], out: &mut [u8], pending: &mut Vec<u8>) -> usize {
    let copy_len = packet.len().min(out.len());
    out[..copy_len].copy_from_slice(&packet[..copy_len]);
    if packet.len() > copy_len {
        pending.extend_from_slice(&packet[copy_len..]);
    }
    copy_len
}

pub(crate) fn drain_pending_into(pending: &mut Vec<u8>, out: &mut [u8]) -> usize {
    let copy_len = pending.len().min(out.len());
    if copy_len == 0 {
        return 0;
    }
    out[..copy_len].copy_from_slice(&pending[..copy_len]);
    pending.drain(..copy_len);
    copy_len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_in_submit_len_uses_one_packet_for_single_byte_read() {
        assert_eq!(bulk_in_submit_len(1, 512), 512);
    }

    #[test]
    fn bulk_in_submit_len_rounds_up_to_packet_multiple() {
        assert_eq!(bulk_in_submit_len(513, 512), 1024);
    }

    #[test]
    fn bulk_in_submit_len_keeps_zero_length_zero() {
        assert_eq!(bulk_in_submit_len(0, 512), 0);
    }

    #[test]
    fn interface_control_transfer_uses_control_interface_number_as_index() {
        assert_eq!(control_index_for_recipient(0xA1, 0, 0), 0);
        assert_eq!(control_index_for_recipient(0x21, 0, 0), 0);
    }

    #[test]
    fn device_control_transfer_keeps_requested_index() {
        assert_eq!(control_index_for_recipient(0x80, 7, 1), 7);
    }

    #[test]
    fn bulk_packet_overread_is_kept_as_pending_bytes() {
        let packet = [0x12, 0x34, 0x56, 0x78];
        let mut out = [0u8; 2];
        let mut pending = Vec::new();

        let copied = copy_from_bulk_packet(&packet, &mut out, &mut pending);

        assert_eq!(copied, 2);
        assert_eq!(out, [0x12, 0x34]);
        assert_eq!(pending, vec![0x56, 0x78]);
    }

    #[test]
    fn pending_bytes_are_drained_before_new_usb_reads() {
        let mut pending = vec![0x56, 0x78, 0x9A];
        let mut out = [0u8; 2];

        let copied = drain_pending_into(&mut pending, &mut out);

        assert_eq!(copied, 2);
        assert_eq!(out, [0x56, 0x78]);
        assert_eq!(pending, vec![0x9A]);
    }
}

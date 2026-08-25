//! USB 设备 IO 操作：读取 / 写入 / 控制传输 / 清除停顿
//!
//! 使用 nusb Interface 的 control_transfer_in/out 和 bulk transfer_blocking。
//! 后续阶段 2 会将 bulk 读取改为异步流水线（submit/complete）。

use super::device::UsbDevice;
use super::log::{QUIET_USB_READ, usb_trace};
use log::trace;
use std::sync::atomic::Ordering;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, In, Recipient};

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

fn cancel_and_drain_in_endpoint(ep_in: &mut nusb::Endpoint<Bulk, In>) {
    ep_in.cancel_all();
    for _ in 0..CANCEL_TRANSFER_MAX_DRAINS {
        if ep_in.pending() == 0 {
            break;
        }
        let _ = ep_in.wait_next_complete(CANCEL_TRANSFER_DRAIN_TIMEOUT);
    }
}

/// 获取 IN 端点地址（解决借用检查问题）
fn in_ep_addr(device: &UsbDevice) -> u8 {
    device.in_ep
}

/// 获取 OUT 端点地址
fn out_ep_addr(device: &UsbDevice) -> u8 {
    device.out_ep
}

impl UsbDevice {
    pub fn submit_read(&mut self, len: usize) -> Result<bool, String> {
        if len == 0 || !self.in_buf.is_empty() {
            return Ok(false);
        }

        let max_packet_size = self.in_ep_max_packet_size;
        let ep_in = self.in_endpoint_mut().ok_or("输入端点未初始化")?;
        if ep_in.pending() != 0 {
            return Ok(false);
        }

        let submit_len = bulk_in_submit_len(len, max_packet_size);
        let buffer = nusb::transfer::Buffer::new(submit_len);
        ep_in.submit(buffer);
        Ok(true)
    }

    pub fn complete_read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if buf.is_empty() {
            return Ok(0);
        }

        let timeout = self.timeout;
        let ep_in = self.in_endpoint_mut().ok_or("输入端点未初始化")?;
        if ep_in.pending() == 0 {
            return Err("没有待完成的预提交读取".to_string());
        }

        let result = match ep_in.wait_next_complete(timeout) {
            Some(r) => r,
            None => {
                cancel_and_drain_in_endpoint(ep_in);
                return Err("queued read timeout".to_string());
            }
        };

        result
            .status
            .map_err(|e| format!("queued read err: {:?}", e))?;
        let actual_len = result.actual_len;
        Ok(copy_from_bulk_packet(
            &result.buffer[..actual_len],
            buf,
            &mut self.in_buf,
        ))
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        let timeout = self.timeout;
        let ep_out = self.out_endpoint_mut().ok_or("输出端点未初始化")?;

        if data.is_empty() {
            // ZLP (Zero Length Packet)
            usb_trace("TX", "USB设备::写入 ZLP", &[]);
            let buf: nusb::transfer::Buffer = Vec::<u8>::new().into();
            ep_out.submit(buf);
            let result = ep_out.wait_next_complete(timeout).ok_or("ZLP 写入超时")?;
            result
                .status
                .map_err(|e| format!("write ZLP err: {:?}", e))?;
            return Ok(0);
        }

        usb_trace("TX", "USB设备::写入", data);

        let buf: nusb::transfer::Buffer = data.to_vec().into();
        ep_out.submit(buf);
        let result = match ep_out.wait_next_complete(timeout) {
            Some(r) => r,
            None => {
                trace!(
                    "[USB WRITE] timeout after {:?}ms, len={}",
                    timeout.as_millis(),
                    data.len()
                );
                // 超时后必须排空 OUT endpoint 的 pending transfer，否则后续 submit
                // 会被追加到队列末尾，WinUSB 仍在等待第一个（已失败的）transfer
                Self::drain_out_pending(ep_out);
                return Err("写入超时".to_string());
            }
        };
        if let Err(ref e) = result.status {
            trace!("[USB WRITE] err: {:?}, len={}", e, data.len());
        }
        result.status.map_err(|e| format!("write err: {:?}", e))?;
        Ok(result.actual_len)
    }

    /// 排空 OUT endpoint 上所有挂起的 transfer（超时后必须调用）
    fn drain_out_pending(ep_out: &mut nusb::Endpoint<nusb::transfer::Bulk, nusb::transfer::Out>) {
        let pending = ep_out.pending();
        if pending > 0 {
            trace!("[USB] drain_out_pending: 取消 {} 个挂起 OUT 传输", pending);
            ep_out.cancel_all();
            for _ in 0..pending {
                let _ = ep_out.wait_next_complete(Duration::from_millis(100));
            }
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let quiet = QUIET_USB_READ.load(Ordering::Relaxed);

        if !quiet {
            trace!(
                "[USB READ] starting, buf_len={}, timeout={:?}ms",
                buf.len(),
                self.timeout.as_millis()
            );
        }

        let pending_copied = drain_pending_into(&mut self.in_buf, buf);
        if pending_copied > 0 {
            if !quiet {
                trace!("[USB READ] got {} bytes from pending", pending_copied);
            }
            return Ok(pending_copied);
        }

        let submit_len = bulk_in_submit_len(buf.len(), self.in_ep_max_packet_size);

        let timeout = self.timeout;
        let ep_in = self.in_endpoint_mut().ok_or("输入端点未初始化")?;

        let buffer = nusb::transfer::Buffer::new(submit_len);
        ep_in.submit(buffer);

        let result = match ep_in.wait_next_complete(timeout) {
            Some(r) => r,
            None => {
                if !quiet {
                    trace!("[USB READ] timeout");
                }
                cancel_and_drain_in_endpoint(ep_in);
                return Ok(0);
            }
        };

        match result.status {
            Ok(()) => {
                let actual_len = result.actual_len;
                if !quiet {
                    trace!("[USB READ] got {} bytes", actual_len);
                }
                let copy_len =
                    copy_from_bulk_packet(&result.buffer[..actual_len], buf, &mut self.in_buf);
                if copy_len > 0 {
                    usb_trace("RX", "USB设备::读取", &buf[..copy_len]);
                }
                Ok(copy_len)
            }
            Err(e) => {
                let err_str = format!("{:?}", e);
                if err_str.contains("timeout") || err_str.contains("Timeout") {
                    if self.timeout.as_millis() < 50 {
                        return Ok(0);
                    }
                    if !quiet {
                        trace!("[USB READ] timeout error");
                    }
                    Ok(0)
                } else {
                    Err(format!("read err: {}", err_str))
                }
            }
        }
    }

    /// 精确读取：循环 bulk transfer 直到读满 buf.len()
    /// 使用双缓冲流水线：在等待当前 transfer 时提前提交下一个，
    /// 消除 USB 总线在两次 submit 之间的空闲间隙。
    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if buf.is_empty() {
            return Ok(0);
        }
        let quiet = QUIET_USB_READ.load(Ordering::Relaxed);
        if !quiet {
            trace!(
                "[USB READ EXACT] starting, buf_len={}, timeout={:?}ms",
                buf.len(),
                self.timeout.as_millis()
            );
        }

        let max_packet_size = self.in_ep_max_packet_size;
        let pending_copied = drain_pending_into(&mut self.in_buf, buf);
        if pending_copied > 0 {
            if !quiet {
                trace!(
                    "[USB READ EXACT] got {} bytes from pending (total: {}/{})",
                    pending_copied,
                    pending_copied,
                    buf.len()
                );
            }
        }
        let mut total = pending_copied;
        let mut overread = Vec::new();
        {
            let timeout = self.timeout;
            let ep_in = self.in_endpoint_mut().ok_or("输入端点未初始化")?;

            // 双缓冲流水线：提前提交第一个 transfer
            if total < buf.len() {
                let remaining = buf.len() - total;
                let submit_len = bulk_in_submit_len(remaining, max_packet_size);
                ep_in.submit(nusb::transfer::Buffer::new(submit_len));
            }

            while total < buf.len() {
                let result = match ep_in.wait_next_complete(timeout) {
                    Some(r) => r,
                    None => {
                        cancel_and_drain_in_endpoint(ep_in);
                        if total > 0 {
                            if !quiet {
                                trace!(
                                    "[USB READ EXACT] partial read: {}/{} bytes before timeout",
                                    total,
                                    buf.len()
                                );
                            }
                            break;
                        }
                        return Err("read_exact timeout".into());
                    }
                };

                // 立即提交下一个 transfer（流水线关键：不等数据处理完就提交）
                if total + result.actual_len < buf.len() {
                    let remaining = buf.len() - (total + result.actual_len);
                    let submit_len = bulk_in_submit_len(remaining, max_packet_size);
                    ep_in.submit(nusb::transfer::Buffer::new(submit_len));
                }

                match result.status {
                    Ok(()) => {
                        let actual_len = result.actual_len;
                        if actual_len == 0 {
                            std::hint::spin_loop();
                            continue;
                        }
                        let copy_len = actual_len.min(buf.len() - total);
                        buf[total..total + copy_len].copy_from_slice(&result.buffer[..copy_len]);
                        if actual_len > copy_len {
                            overread.extend_from_slice(&result.buffer[copy_len..actual_len]);
                        }
                        total += copy_len;
                    }
                    Err(e) => {
                        let err_str = format!("{:?}", e);
                        if err_str.contains("timeout") || err_str.contains("Timeout") {
                            if total > 0 {
                                break;
                            }
                            return Err("read_exact timeout".into());
                        }
                        return Err(format!("read_exact err: {}", err_str));
                    }
                }
            }
        }
        self.in_buf.extend(overread);

        if !quiet {
            trace!("[USB READ EXACT] total read: {}/{} bytes", total, buf.len());
        }
        if total > 0 {
            usb_trace("RX", "USB设备::精确读取", &buf[..total]);
        }
        Ok(total)
    }

    pub fn read_exact_vec(&mut self, len: usize) -> Result<Vec<u8>, String> {
        if len == 0 {
            return Ok(Vec::new());
        }

        let max_packet_size = self.in_ep_max_packet_size;
        let mut out = Vec::with_capacity(len);
        if !self.in_buf.is_empty() {
            let take = self.in_buf.len().min(len);
            out.extend_from_slice(&self.in_buf[..take]);
            self.in_buf.drain(..take);
        }

        // 双缓冲流水线：提前提交第一个 transfer
        {
            let ep_in = self.in_endpoint_mut().ok_or("输入端点未初始化")?;
            if out.len() < len {
                let remaining = len - out.len();
                let submit_len = bulk_in_submit_len(remaining, max_packet_size);
                ep_in.submit(nusb::transfer::Buffer::new(submit_len));
            }
        }

        while out.len() < len {
            let remaining = len - out.len();
            let timeout = self.timeout;
            let (result, actual_len) = {
                let ep_in = self.in_endpoint_mut().ok_or("输入端点未初始化")?;
                let result = match ep_in.wait_next_complete(timeout) {
                    Some(r) => r,
                    None => {
                        cancel_and_drain_in_endpoint(ep_in);
                        return Err("read_exact_vec timeout".to_string());
                    }
                };

                // 立即提交下一个 transfer（流水线）
                let actual_len = result.actual_len;
                if out.len() + actual_len < len {
                    let next_remaining = len - (out.len() + actual_len);
                    let submit_len = bulk_in_submit_len(next_remaining, max_packet_size);
                    ep_in.submit(nusb::transfer::Buffer::new(submit_len));
                }

                result
                    .status
                    .map_err(|e| format!("read_exact_vec err: {:?}", e))?;
                (result, actual_len)
            };
            let mut chunk = result.buffer.into_vec();
            if actual_len == 0 {
                std::hint::spin_loop();
                continue;
            }
            if chunk.len() > actual_len {
                chunk.truncate(actual_len);
            }
            let copy_len = chunk.len().min(remaining);
            if copy_len == chunk.len() && out.is_empty() && copy_len == len {
                return Ok(chunk);
            }
            out.extend_from_slice(&chunk[..copy_len]);
            if chunk.len() > copy_len {
                self.in_buf.extend_from_slice(&chunk[copy_len..]);
            }
        }

        Ok(out)
    }

    pub fn ctrl_in(
        &mut self,
        rt: u8,
        r: u8,
        v: u16,
        i: u16,
        len: u16,
    ) -> Result<Vec<u8>, String> {
        trace!(
            "[CTRL] IN rt=0x{:02X} r=0x{:02X} v=0x{:04X} i=0x{:04X} len={}",
            rt, r, v, i, len
        );

        let timeout = self.timeout;
        let index = control_index_for_recipient(rt, i, self.control_iface_num);
        let interface = self.control_iface().ok_or("控制接口未初始化")?;

        let control = ControlIn {
            control_type: Self::convert_ctrl_type(rt),
            recipient: Self::convert_receiver(rt),
            request: r,
            value: v,
            index,
            length: len,
        };

        let result = interface
            .control_in(control, timeout)
            .wait()
            .map_err(|e| format!("ctrl_transfer_in err: {:?}", e))?;

        usb_trace("RX", "USB设备::控制传输输入", &result);
        Ok(result)
    }

    pub fn ctrl_out(
        &mut self,
        rt: u8,
        r: u8,
        v: u16,
        i: u16,
        data: &[u8],
    ) -> Result<usize, String> {
        trace!(
            "[CTRL] OUT rt=0x{:02X} r=0x{:02X} v=0x{:04X} i=0x{:04X} len={}",
            rt,
            r,
            v,
            i,
            data.len()
        );
        usb_trace("TX", "USB设备::控制传输输出", data);

        let timeout = self.timeout;
        let index = control_index_for_recipient(rt, i, self.control_iface_num);
        let interface = self.control_iface().ok_or("控制接口未初始化")?;

        let control = ControlOut {
            control_type: Self::convert_ctrl_type(rt),
            recipient: Self::convert_receiver(rt),
            request: r,
            value: v,
            index,
            data,
        };

        interface
            .control_out(control, timeout)
            .wait()
            .map_err(|e| format!("ctrl_transfer_out err: {:?}", e))?;

        Ok(data.len())
    }

    pub fn clear_halt_in(&mut self) -> Result<(), String> {
        // nusb: 通过 control transfer 实现 clear_halt
        // SET_FEATURE ENDPOINT_HALT (0x00) 清除 halt
        let ep_addr = in_ep_addr(self);
        let interface = self.interface_mut().ok_or("设备未初始化")?;
        let control = nusb::transfer::ControlOut {
            control_type: ControlType::Standard,
            recipient: Recipient::Endpoint,
            request: 0x01, // SET_FEATURE
            value: 0x00,   // ENDPOINT_HALT
            index: ep_addr as u16,
            data: &[],
        };
        interface
            .control_out(control, Duration::from_millis(100))
            .wait()
            .map_err(|e| format!("clear_halt ep_in err: {:?}", e))?;
        Ok(())
    }

    pub fn clear_halt_out(&mut self) -> Result<(), String> {
        let ep_addr = out_ep_addr(self);
        trace!("[USB] clear_halt_out ep=0x{:02X}", ep_addr);
        let interface = self.interface_mut().ok_or("设备未初始化")?;
        let control = nusb::transfer::ControlOut {
            control_type: ControlType::Standard,
            recipient: Recipient::Endpoint,
            request: 0x01,
            value: 0x00,
            index: ep_addr as u16,
            data: &[],
        };
        interface
            .control_out(control, Duration::from_millis(100))
            .wait()
            .map_err(|e| {
                trace!("[USB] clear_halt_out ep=0x{:02X} err: {:?}", ep_addr, e);
                format!("clear_halt ep_out err: {:?}", e)
            })?;
        trace!("[USB] clear_halt_out ep=0x{:02X} ok", ep_addr);
        Ok(())
    }

    /// 将 libusb 的 bmRequestType 转换为 nusb ControlType
    fn convert_ctrl_type(bm_request_type: u8) -> ControlType {
        let typ = (bm_request_type >> 5) & 0x03;
        match typ {
            0 => ControlType::Standard,
            1 => ControlType::Class,
            2 => ControlType::Vendor,
            _ => ControlType::Standard,
        }
    }

    /// 将 libusb 的 bmRequestType 转换为 nusb Recipient
    fn convert_receiver(bm_request_type: u8) -> Recipient {
        let rec = bm_request_type & 0x1F;
        match rec {
            0 => Recipient::Device,
            1 => Recipient::Interface,
            2 => Recipient::Endpoint,
            3 => Recipient::Other,
            _ => Recipient::Device,
        }
    }
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

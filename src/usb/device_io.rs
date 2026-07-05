//! USB 设备 IO 操作：读取 / 写入 / 控制传输 / 清除停顿
//!
//! 使用 nusb Interface 的 control_transfer_in/out 和 bulk transfer_blocking。
//! 后续阶段 2 会将 bulk 读取改为异步流水线（submit/complete）。

use super::device::USB设备;
use super::log::{QUIET_USB_READ, usb_trace};
use log::trace;
use std::sync::atomic::Ordering;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, In, Recipient};

const 取消传输排空超时: Duration = Duration::from_millis(20);
const 取消传输最大排空次数: usize = 8;

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
    match USB设备::转换接收者(bm_request_type) {
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
    for _ in 0..取消传输最大排空次数 {
        if ep_in.pending() == 0 {
            break;
        }
        let _ = ep_in.wait_next_complete(取消传输排空超时);
    }
}

/// 获取 IN 端点地址（解决借用检查问题）
fn 输入端点地址(设备: &USB设备) -> u8 {
    设备.输入端点
}

/// 获取 OUT 端点地址
fn 输出端点地址(设备: &USB设备) -> u8 {
    设备.输出端点
}

impl USB设备 {
    pub fn 写入(&mut self, data: &[u8]) -> Result<usize, String> {
        let timeout = self.超时;
        let ep_out = self.获取输出端点_mut().ok_or("输出端点未初始化")?;

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
        let result = ep_out.wait_next_complete(timeout).ok_or("写入超时")?;
        result.status.map_err(|e| format!("write err: {:?}", e))?;
        Ok(result.actual_len)
    }

    pub fn 读取(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let 静默 = QUIET_USB_READ.load(Ordering::Relaxed);

        if !静默 {
            trace!(
                "[USB READ] starting, buf_len={}, timeout={:?}ms",
                buf.len(),
                self.超时.as_millis()
            );
        }

        let pending_copied = drain_pending_into(&mut self.输入暂存, buf);
        if pending_copied > 0 {
            if !静默 {
                trace!("[USB READ] got {} bytes from pending", pending_copied);
            }
            return Ok(pending_copied);
        }

        let submit_len = bulk_in_submit_len(buf.len(), self.输入端点最大包大小);

        let timeout = self.超时;
        let ep_in = self.获取输入端点_mut().ok_or("输入端点未初始化")?;

        let buffer = nusb::transfer::Buffer::new(submit_len);
        ep_in.submit(buffer);

        let result = match ep_in.wait_next_complete(timeout) {
            Some(r) => r,
            None => {
                if !静默 {
                    trace!("[USB READ] timeout");
                }
                cancel_and_drain_in_endpoint(ep_in);
                return Ok(0);
            }
        };

        match result.status {
            Ok(()) => {
                let 实际长度 = result.actual_len;
                if !静默 {
                    trace!("[USB READ] got {} bytes", 实际长度);
                }
                let copy_len =
                    copy_from_bulk_packet(&result.buffer[..实际长度], buf, &mut self.输入暂存);
                if copy_len > 0 {
                    usb_trace("RX", "USB设备::读取", &buf[..copy_len]);
                }
                Ok(copy_len)
            }
            Err(e) => {
                let err_str = format!("{:?}", e);
                if err_str.contains("timeout") || err_str.contains("Timeout") {
                    if self.超时.as_millis() < 50 {
                        return Ok(0);
                    }
                    if !静默 {
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
    pub fn 精确读取(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if buf.is_empty() {
            return Ok(0);
        }
        let 静默 = QUIET_USB_READ.load(Ordering::Relaxed);
        if !静默 {
            trace!(
                "[USB READ EXACT] starting, buf_len={}, timeout={:?}ms",
                buf.len(),
                self.超时.as_millis()
            );
        }

        let max_packet_size = self.输入端点最大包大小;
        let pending_copied = drain_pending_into(&mut self.输入暂存, buf);
        if pending_copied > 0 {
            if !静默 {
                trace!(
                    "[USB READ EXACT] got {} bytes from pending (total: {}/{})",
                    pending_copied,
                    pending_copied,
                    buf.len()
                );
            }
        }
        let mut 总计 = pending_copied;
        let mut overread = Vec::new();
        {
            let timeout = self.超时;
            let ep_in = self.获取输入端点_mut().ok_or("输入端点未初始化")?;

            while 总计 < buf.len() {
                let 剩余 = buf.len() - 总计;
                let submit_len = bulk_in_submit_len(剩余, max_packet_size);
                let buffer = nusb::transfer::Buffer::new(submit_len);
                ep_in.submit(buffer);

                let result = match ep_in.wait_next_complete(timeout) {
                    Some(r) => r,
                    None => {
                        cancel_and_drain_in_endpoint(ep_in);
                        if 总计 > 0 {
                            if !静默 {
                                trace!(
                                    "[USB READ EXACT] partial read: {}/{} bytes before timeout",
                                    总计,
                                    buf.len()
                                );
                            }
                            break;
                        }
                        return Err("read_exact timeout".into());
                    }
                };

                match result.status {
                    Ok(()) => {
                        let 实际长度 = result.actual_len;
                        if !静默 {
                            trace!(
                                "[USB READ EXACT] got {} bytes (total: {}/{})",
                                实际长度,
                                总计 + 实际长度,
                                buf.len()
                            );
                        }
                        if 实际长度 == 0 {
                            std::hint::spin_loop();
                            continue;
                        }
                        let copy_len = 实际长度.min(剩余);
                        buf[总计..总计 + copy_len].copy_from_slice(&result.buffer[..copy_len]);
                        if 实际长度 > copy_len {
                            overread.extend_from_slice(&result.buffer[copy_len..实际长度]);
                        }
                        总计 += copy_len;
                    }
                    Err(e) => {
                        let err_str = format!("{:?}", e);
                        if err_str.contains("timeout") || err_str.contains("Timeout") {
                            if 总计 > 0 {
                                break;
                            }
                            return Err("read_exact timeout".into());
                        }
                        return Err(format!("read_exact err: {}", err_str));
                    }
                }
            }
        }
        self.输入暂存.extend(overread);

        if !静默 {
            trace!("[USB READ EXACT] total read: {}/{} bytes", 总计, buf.len());
        }
        if 总计 > 0 {
            usb_trace("RX", "USB设备::精确读取", &buf[..总计]);
        }
        Ok(总计)
    }

    pub fn 控制传输输入(
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

        let 超时 = self.超时;
        let index = control_index_for_recipient(rt, i, self.控制接口编号);
        let interface = self.获取控制interface().ok_or("控制接口未初始化")?;

        let control = ControlIn {
            control_type: Self::转换控制类型(rt),
            recipient: Self::转换接收者(rt),
            request: r,
            value: v,
            index,
            length: len,
        };

        let result = interface
            .control_in(control, 超时)
            .wait()
            .map_err(|e| format!("ctrl_transfer_in err: {:?}", e))?;

        usb_trace("RX", "USB设备::控制传输输入", &result);
        Ok(result)
    }

    pub fn 控制传输输出(
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

        let 超时 = self.超时;
        let index = control_index_for_recipient(rt, i, self.控制接口编号);
        let interface = self.获取控制interface().ok_or("控制接口未初始化")?;

        let control = ControlOut {
            control_type: Self::转换控制类型(rt),
            recipient: Self::转换接收者(rt),
            request: r,
            value: v,
            index,
            data,
        };

        interface
            .control_out(control, 超时)
            .wait()
            .map_err(|e| format!("ctrl_transfer_out err: {:?}", e))?;

        Ok(data.len())
    }

    pub fn 清除输入端点停顿(&mut self) -> Result<(), String> {
        // nusb: 通过 control transfer 实现 clear_halt
        // SET_FEATURE ENDPOINT_HALT (0x00) 清除 halt
        let ep_addr = 输入端点地址(self);
        let interface = self.获取interface_mut().ok_or("设备未初始化")?;
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

    pub fn 清除输出端点停顿(&mut self) -> Result<(), String> {
        let ep_addr = 输出端点地址(self);
        let interface = self.获取interface_mut().ok_or("设备未初始化")?;
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
            .map_err(|e| format!("clear_halt ep_out err: {:?}", e))?;
        Ok(())
    }

    /// 将 libusb 的 bmRequestType 转换为 nusb ControlType
    fn 转换控制类型(bm_request_type: u8) -> ControlType {
        let typ = (bm_request_type >> 5) & 0x03;
        match typ {
            0 => ControlType::Standard,
            1 => ControlType::Class,
            2 => ControlType::Vendor,
            _ => ControlType::Standard,
        }
    }

    /// 将 libusb 的 bmRequestType 转换为 nusb Recipient
    fn 转换接收者(bm_request_type: u8) -> Recipient {
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

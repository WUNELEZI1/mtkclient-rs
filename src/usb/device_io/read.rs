use crate::usb::device::UsbDevice;
use crate::usb::log::{QUIET_USB_READ, usb_trace};
use log::trace;
use std::sync::atomic::Ordering;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{Bulk, ControlType, In, Recipient};

use super::{bulk_in_submit_len, copy_from_bulk_packet, drain_pending_into};

fn cancel_and_drain_in_endpoint(ep_in: &mut nusb::Endpoint<Bulk, In>) {
    ep_in.cancel_all();
    for _ in 0..super::CANCEL_TRANSFER_MAX_DRAINS {
        if ep_in.pending() == 0 {
            break;
        }
        let _ = ep_in.wait_next_complete(super::CANCEL_TRANSFER_DRAIN_TIMEOUT);
    }
}

/// Get the IN endpoint address (works around borrow-checker issues)
fn in_ep_addr(device: &UsbDevice) -> u8 {
    device.in_ep
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
            Err(e) => match e {
                // bulk-IN endpoint STALL: clear the halt and retry once, otherwise the endpoint keeps failing
                nusb::transfer::TransferError::Stall => {
                    if !quiet {
                        trace!("[USB READ] bulk-IN stalled, clear_halt_in + retry once");
                    }
                    // The ep_in borrow from the previous wait_next_complete has ended (NLL), so self can be borrowed again
                    let _ = self.clear_halt_in();
                    let ep_in = self.in_endpoint_mut().ok_or("输入端点未初始化")?;
                    let buffer = nusb::transfer::Buffer::new(submit_len);
                    ep_in.submit(buffer);
                    let result = match ep_in.wait_next_complete(timeout) {
                        Some(r) => r,
                        None => {
                            cancel_and_drain_in_endpoint(ep_in);
                            return Ok(0);
                        }
                    };
                    match result.status {
                        Ok(()) => {
                            let actual_len = result.actual_len;
                            let copy_len = copy_from_bulk_packet(
                                &result.buffer[..actual_len],
                                buf,
                                &mut self.in_buf,
                            );
                            if copy_len > 0 {
                                usb_trace("RX", "USB设备::读取", &buf[..copy_len]);
                            }
                            Ok(copy_len)
                        }
                        Err(retry_err) => {
                            if !quiet {
                                trace!(
                                    "[USB READ] still failing after clear_halt_in: {:?}",
                                    retry_err
                                );
                            }
                            Err(format!("read err: {:?}", retry_err))
                        }
                    }
                }
                // Cancel/timeout: on WinUSB ERROR_TIMEOUT maps to Cancelled; return Ok(0) as the old string check did
                nusb::transfer::TransferError::Cancelled => Ok(0),
                other => Err(format!("read err: {:?}", other)),
            },
        }
    }

    /// Exact read: loop bulk transfers until buf.len() bytes have been read.
    /// Uses a double-buffered pipeline: submit the next transfer while waiting for the current one,
    /// eliminating the USB bus idle gap between two submits.
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

            // Double-buffered pipeline: submit the first transfer up front
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
                            return Err(format!(
                                "USB 读取超时，仅收到 {}/{} 字节",
                                total,
                                buf.len()
                            ));
                        }
                        return Err(crate::error::UsbError::Timeout.to_string());
                    }
                };

                // Submit the next transfer immediately (the key to pipelining: do not wait for data processing)
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
                                return Err(format!(
                                    "USB 读取超时，仅收到 {}/{} 字节",
                                    total,
                                    buf.len()
                                ));
                            }
                            return Err(crate::error::UsbError::Timeout.to_string());
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

        // Double-buffered pipeline: submit the first transfer up front
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
                        return Err(crate::error::UsbError::Timeout.to_string());
                    }
                };

                // Submit the next transfer immediately (pipelining)
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

    pub fn clear_halt_in(&mut self) -> Result<(), String> {
        // nusb: implement clear_halt via a control transfer
        // SET_FEATURE with ENDPOINT_HALT (0x00) clears the halt
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
}

//! UsbDevice IO 操作：read / write / ctrl_transfer / clear_halt
//!
//! - `read`         — bulk IN，循环到 deadline 或首次读到数据
//! - `read_exact`   — bulk IN，循环到读满 buf.len() 为止
//! - `write`        — bulk OUT（含 ZLP 处理）
//! - `ctrl_transfer_in/out` — control transfer
//! - `clear_halt_*` — 复位 bulk 端点（stall）

use super::context::LIBUSB_ERROR_TIMEOUT;
use super::device::UsbDevice;
use super::log::{QUIET_USB_READ, usb_trace};
use log::trace;
use std::sync::atomic::Ordering;
use std::time::Duration;

const READ_RETRY_DELAY_MS: u64 = 10;

impl UsbDevice {
    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        if data.is_empty() {
            // ZLP (Zero Length Packet)
            usb_trace("TX", "UsbDevice::write ZLP", &[]);
            unsafe {
                let mut transferred: i32 = 0;
                let ret = libusb1_sys::libusb_bulk_transfer(
                    self.handle,
                    self.ep_out,
                    std::ptr::null_mut(),
                    0,
                    &mut transferred,
                    self.timeout.as_millis() as u32,
                );
                if ret != 0 {
                    return Err(format!("write ZLP err {}", ret));
                }
            }
            return Ok(0);
        }

        // TX trace: 记录发送的数据
        usb_trace("TX", "UsbDevice::write", data);

        // Python usbwrite sends all data in one libusb_bulk_transfer call
        // No chunking - let libusb handle USB packetization internally
        unsafe {
            let mut transferred: i32 = 0;
            let ret = libusb1_sys::libusb_bulk_transfer(
                self.handle,
                self.ep_out,
                data.as_ptr() as *mut u8,
                data.len() as i32,
                &mut transferred,
                self.timeout.as_millis() as u32,
            );
            if ret != 0 {
                return Err(format!(
                    "write err {} (transferred={}/{})",
                    ret,
                    transferred,
                    data.len()
                ));
            }
            Ok(transferred as usize)
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let quiet = QUIET_USB_READ.load(Ordering::Relaxed);
        let mut total = 0usize;
        let deadline = std::time::Instant::now() + self.timeout;
        if !quiet {
            trace!(
                "[USB READ] starting, buf_len={}, timeout={:?}ms",
                buf.len(),
                self.timeout.as_millis()
            );
        }
        while total < buf.len() {
            let remaining = buf.len() - total;
            unsafe {
                let mut transferred: i32 = 0;
                let now = std::time::Instant::now();
                if now >= deadline {
                    if !quiet {
                        trace!("[USB READ] deadline reached, breaking, total={}", total);
                    }
                    break;
                }
                let ms_left = match (deadline - now).checked_sub(Duration::ZERO) {
                    Some(d) => {
                        let ms = d.as_millis() as u32;
                        if ms == 0 { 1 } else { ms }
                    }
                    None => break,
                };
                if !quiet {
                    trace!(
                        "[USB READ] calling bulk_transfer, remaining={}, timeout={}ms",
                        remaining, ms_left
                    );
                }
                let ret = libusb1_sys::libusb_bulk_transfer(
                    self.handle,
                    self.ep_in,
                    buf[total..].as_mut_ptr(),
                    remaining as i32,
                    &mut transferred,
                    ms_left,
                );
                if !quiet {
                    trace!(
                        "[USB READ] bulk_transfer returned: ret={}, transferred={}",
                        ret, transferred
                    );
                }

                if ret == LIBUSB_ERROR_TIMEOUT && self.timeout.as_millis() < 50 {
                    // 优化：对于极短超时（如 flush/drain），超时即视为无更多数据，直接返回
                    return Ok(total);
                }

                if ret != 0 && ret != LIBUSB_ERROR_TIMEOUT {
                    if !quiet {
                        trace!("[USB READ] error, returning");
                    }
                    return Err(format!("read err {}", ret));
                }
                total += transferred as usize;

                // 核心修复：一旦读到任何数据，立即返回，不要等待读满整个 buf.len()
                // 这符合标准 read 语义，也避免了 handshake 等场景下的挂起
                if total > 0 {
                    if !quiet {
                        trace!(
                            "[USB READ] data received ({} bytes), returning early",
                            total
                        );
                    }
                    break;
                }

                if transferred == 0 {
                    // ZLP 或设备忙，对齐 PyUSB 行为：自动忽略 ZLP 继续等待数据
                    if ret == 0 {
                        // ZLP (zero-length packet): ret=0, transferred=0
                        if !quiet {
                            trace!("[USB READ] ZLP received, retrying");
                        }
                    }
                    if now < deadline {
                        if !quiet {
                            trace!("[USB READ] transferred=0, retrying in 10ms");
                        }
                        std::thread::sleep(Duration::from_millis(READ_RETRY_DELAY_MS));
                        continue;
                    } else {
                        if !quiet {
                            trace!("[USB READ] transferred=0 at deadline, breaking");
                        }
                        break;
                    }
                }
            }
        }
        if !quiet {
            trace!("[USB READ] returning total={}", total);
        }
        // RX trace: 记录实际读取的数据
        if total > 0 {
            usb_trace("RX", "UsbDevice::read", &buf[..total]);
        }
        Ok(total)
    }

    /// 精确读取：循环 bulk transfer 直到读满 buf.len()
    /// 对齐 Python usbread(length) — 精确读 length 字节
    /// 修复：之前只调用一次 bulk_transfer，如果设备分多次返回数据（如先返回 2 字节再返回 2 字节），
    ///       会导致读不完整，残留字节污染后续 echo 通信。
    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if buf.is_empty() {
            return Ok(0);
        }
        trace!(
            "[USB READ EXACT] starting, buf_len={}, timeout={:?}ms",
            buf.len(),
            self.timeout.as_millis()
        );
        let mut total = 0usize;
        while total < buf.len() {
            let remaining = buf.len() - total;
            unsafe {
                let mut transferred: i32 = 0;
                let ret = libusb1_sys::libusb_bulk_transfer(
                    self.handle,
                    self.ep_in,
                    buf[total..].as_mut_ptr(),
                    remaining as i32,
                    &mut transferred,
                    self.timeout.as_millis() as u32,
                );
                trace!(
                    "[USB READ EXACT] bulk_transfer returned: ret={}, transferred={}",
                    ret, transferred
                );
                if ret == LIBUSB_ERROR_TIMEOUT {
                    if total > 0 {
                        trace!(
                            "[USB READ EXACT] partial read: {}/{} bytes before timeout",
                            total,
                            buf.len()
                        );
                        break;
                    }
                    return Err("read_exact timeout".into());
                }
                if ret != 0 {
                    return Err(format!("read_exact bulk err: {}", ret));
                }
                if transferred == 0 {
                    std::thread::sleep(Duration::from_millis(READ_RETRY_DELAY_MS));
                    continue;
                }
                total += transferred as usize;
            }
        }
        trace!("[USB READ EXACT] total read: {}/{} bytes", total, buf.len());
        // RX trace
        if total > 0 {
            usb_trace("RX", "UsbDevice::read_exact", &buf[..total]);
        }
        Ok(total)
    }

    pub fn ctrl_transfer_in(
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
        unsafe {
            let mut buf = vec![0u8; len as usize];
            let ret = libusb1_sys::libusb_control_transfer(
                self.handle,
                rt,
                r,
                v,
                i,
                buf.as_mut_ptr(),
                len,
                self.timeout.as_millis() as u32,
            );
            if ret < 0 {
                trace!("[CTRL] IN error: {}", ret);
                Err(format!("ctrl_in {}", ret))
            } else {
                buf.truncate(ret as usize);
                // RX trace: 记录 control transfer 接收的数据
                usb_trace("RX", "UsbDevice::ctrl_transfer_in", &buf);
                trace!(
                    "[CTRL] IN OK: {:02X?}",
                    &buf[..std::cmp::min(buf.len(), 16)]
                );
                Ok(buf)
            }
        }
    }

    /// 复位 bulk IN 端点（清除 halt/stall 状态）
    #[allow(dead_code)]
    pub fn clear_halt_in(&mut self) -> Result<(), String> {
        self.clear_halt_ep(self.ep_in)
    }

    /// 复位 bulk OUT 端点（清除 halt/stall 状态）
    #[allow(dead_code)]
    pub fn clear_halt_out(&mut self) -> Result<(), String> {
        self.clear_halt_ep(self.ep_out)
    }

    /// 复位指定 bulk 端点
    pub fn clear_halt_ep(&mut self, ep: u8) -> Result<(), String> {
        unsafe {
            let ret = libusb1_sys::libusb_clear_halt(self.handle, ep);
            if ret != 0 {
                Err(format!("clear_halt ep=0x{:02X} err {}", ep, ret))
            } else {
                trace!("clear_halt ep=0x{:02X} OK", ep);
                Ok(())
            }
        }
    }

    pub fn ctrl_transfer_out(
        &mut self,
        rt: u8,
        r: u8,
        v: u16,
        i: u16,
        data: &[u8],
    ) -> Result<(), String> {
        // TX trace: 记录 control transfer 发送的数据
        usb_trace("TX", "UsbDevice::ctrl_transfer_out", data);
        trace!(
            "[CTRL] OUT rt=0x{:02X} r=0x{:02X} v=0x{:04X} i=0x{:04X} data={:02X?}",
            rt, r, v, i, data
        );
        unsafe {
            let ret = libusb1_sys::libusb_control_transfer(
                self.handle,
                rt,
                r,
                v,
                i,
                data.as_ptr() as *mut u8,
                data.len() as u16,
                self.timeout.as_millis() as u32,
            );
            if ret < 0 {
                trace!("[CTRL] OUT error: {}", ret);
                Err(format!("ctrl_out {}", ret))
            } else {
                trace!("[CTRL] OUT OK: {} bytes sent", ret);
                Ok(())
            }
        }
    }
}

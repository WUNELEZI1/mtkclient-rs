//! USB设备 IO 操作：读取 / 写入 / 控制传输 / 清除停顿
//!
//! - `读取`         — bulk IN，循环到 deadline 或首次读到数据
//! - `精确读取`   — bulk IN，循环到读满 buf.len() 为止
//! - `写入`        — bulk OUT（含 ZLP 处理）
//! - `控制传输输入`/`控制传输输出` — control transfer
//! - `清除输入端点停顿`/`清除输出端点停顿` — 复位 bulk 端点（stall）

use super::上下文::LIBUSB错误_超时;
use super::日志::{QUIET_USB_READ, usb_trace};
use super::设备::USB设备;
use log::trace;
use std::sync::atomic::Ordering;
use std::time::Duration;

impl USB设备 {
    pub fn 写入(&mut self, data: &[u8]) -> Result<usize, String> {
        if data.is_empty() {
            // ZLP (Zero Length Packet)
            usb_trace("TX", "USB设备::写入 ZLP", &[]);
            unsafe {
                let mut 已传输: i32 = 0;
                let 返回码 = libusb1_sys::libusb_bulk_transfer(
                    self.设备句柄,
                    self.输出端点,
                    std::ptr::null_mut(),
                    0,
                    &mut 已传输,
                    self.超时.as_millis() as u32,
                );
                if 返回码 != 0 {
                    return Err(format!("write ZLP err {}", 返回码));
                }
            }
            return Ok(0);
        }

        // TX trace: 记录发送的数据
        usb_trace("TX", "USB设备::写入", data);

        // Python usbwrite sends all data in one libusb_bulk_transfer call
        // No chunking - let libusb handle USB packetization internally
        unsafe {
            let mut 已传输: i32 = 0;
            let 返回码 = libusb1_sys::libusb_bulk_transfer(
                self.设备句柄,
                self.输出端点,
                data.as_ptr() as *mut u8,
                data.len() as i32,
                &mut 已传输,
                self.超时.as_millis() as u32,
            );
            if 返回码 != 0 {
                return Err(format!(
                    "write err {} (transferred={}/{})",
                    返回码,
                    已传输,
                    data.len()
                ));
            }
            Ok(已传输 as usize)
        }
    }

    pub fn 读取(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let 静默 = QUIET_USB_READ.load(Ordering::Relaxed);
        let mut 总计 = 0usize;
        let 截止时间 = std::time::Instant::now() + self.超时;
        if !静默 {
            trace!(
                "[USB READ] starting, buf_len={}, timeout={:?}ms",
                buf.len(),
                self.超时.as_millis()
            );
        }
        while 总计 < buf.len() {
            let 剩余 = buf.len() - 总计;
            unsafe {
                let mut 已传输: i32 = 0;
                let 现在 = std::time::Instant::now();
                if 现在 >= 截止时间 {
                    if !静默 {
                        trace!("[USB READ] deadline reached, breaking, total={}", 总计);
                    }
                    break;
                }
                let 剩余毫秒 = match (截止时间 - 现在).checked_sub(Duration::ZERO) {
                    Some(d) => {
                        let ms = d.as_millis() as u32;
                        if ms == 0 { 1 } else { ms }
                    }
                    None => break,
                };
                if !静默 {
                    trace!(
                        "[USB READ] calling bulk_transfer, remaining={}, timeout={}ms",
                        剩余, 剩余毫秒
                    );
                }
                let 返回码 = libusb1_sys::libusb_bulk_transfer(
                    self.设备句柄,
                    self.输入端点,
                    buf[总计..].as_mut_ptr(),
                    剩余 as i32,
                    &mut 已传输,
                    剩余毫秒,
                );
                if !静默 {
                    trace!(
                        "[USB READ] bulk_transfer returned: ret={}, transferred={}",
                        返回码, 已传输
                    );
                }

                if 返回码 == LIBUSB错误_超时 && self.超时.as_millis() < 50 {
                    // 优化：对于极短超时（如 flush/drain），超时即视为无更多数据，直接返回
                    return Ok(总计);
                }

                if 返回码 != 0 && 返回码 != LIBUSB错误_超时 {
                    if !静默 {
                        trace!("[USB READ] error, returning");
                    }
                    return Err(format!("read err {}", 返回码));
                }
                总计 += 已传输 as usize;

                // 核心修复：一旦读到任何数据，立即返回，不要等待读满整个 buf.len()
                // 这符合标准 read 语义，也避免了 handshake 等场景下的挂起
                if 总计 > 0 {
                    if !静默 {
                        trace!("[USB READ] data received ({} bytes), returning early", 总计);
                    }
                    break;
                }

                if 已传输 == 0 {
                    // ZLP 或设备忙，对齐 Python mtkclient：立即重试（无 sleep）
                    // Python usbread 在 timeout 时只是 timeout += 1; pass
                    if 返回码 == 0 {
                        // ZLP (zero-length packet): ret=0, transferred=0
                        if !静默 {
                            trace!("[USB READ] ZLP received, retrying");
                        }
                    }
                    // 让出 CPU 时间片，但不睡眠（对齐 Python 的 pass）
                    std::hint::spin_loop();
                    continue;
                }
            }
        }
        if !静默 {
            trace!("[USB READ] returning total={}", 总计);
        }
        // RX trace: 记录实际读取的数据
        if 总计 > 0 {
            usb_trace("RX", "USB设备::读取", &buf[..总计]);
        }
        Ok(总计)
    }

    /// 精确读取：循环 bulk transfer 直到读满 buf.len()
    /// 对齐 Python usbread(length) — 精确读 length 字节
    /// 修复：之前只调用一次 bulk_transfer，如果设备分多次返回数据（如先返回 2 字节再返回 2 字节），
    ///       会导致读不完整，残留字节污染后续 echo 通信。
    pub fn 精确读取(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if buf.is_empty() {
            return Ok(0);
        }
        trace!(
            "[USB READ EXACT] starting, buf_len={}, timeout={:?}ms",
            buf.len(),
            self.超时.as_millis()
        );
        let mut 总计 = 0usize;
        while 总计 < buf.len() {
            let 剩余 = buf.len() - 总计;
            unsafe {
                let mut 已传输: i32 = 0;
                let 返回码 = libusb1_sys::libusb_bulk_transfer(
                    self.设备句柄,
                    self.输入端点,
                    buf[总计..].as_mut_ptr(),
                    剩余 as i32,
                    &mut 已传输,
                    self.超时.as_millis() as u32,
                );
                trace!(
                    "[USB READ EXACT] bulk_transfer returned: ret={}, transferred={}",
                    返回码, 已传输
                );
                if 返回码 == LIBUSB错误_超时 {
                    if 总计 > 0 {
                        trace!(
                            "[USB READ EXACT] partial read: {}/{} bytes before timeout",
                            总计,
                            buf.len()
                        );
                        break;
                    }
                    return Err("read_exact timeout".into());
                }
                if 返回码 != 0 {
                    return Err(format!("read_exact bulk err: {}", 返回码));
                }
                if 已传输 == 0 {
                    // 对齐 Python：无 sleep 立即重试
                    std::hint::spin_loop();
                    continue;
                }
                总计 += 已传输 as usize;
            }
        }
        trace!("[USB READ EXACT] total read: {}/{} bytes", 总计, buf.len());
        // RX trace
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
        unsafe {
            let mut buf = vec![0u8; len as usize];
            let ret = libusb1_sys::libusb_control_transfer(
                self.设备句柄,
                rt,
                r,
                v,
                i,
                buf.as_mut_ptr(),
                len,
                self.超时.as_millis() as u32,
            );
            if ret < 0 {
                return Err(format!("ctrl_transfer_in err {}", ret));
            }
            buf.truncate(ret as usize);
            usb_trace("RX", "USB设备::控制传输输入", &buf);
            Ok(buf)
        }
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
        unsafe {
            let ret = libusb1_sys::libusb_control_transfer(
                self.设备句柄,
                rt,
                r,
                v,
                i,
                data.as_ptr() as *mut u8,
                data.len() as u16,
                self.超时.as_millis() as u32,
            );
            if ret < 0 {
                return Err(format!("ctrl_transfer_out err {}", ret));
            }
            Ok(ret as usize)
        }
    }

    pub fn 清除输入端点停顿(&mut self) -> Result<(), String> {
        unsafe {
            let ret = libusb1_sys::libusb_clear_halt(self.设备句柄, self.输入端点);
            if ret != 0 {
                return Err(format!("clear_halt ep_in err {}", ret));
            }
        }
        Ok(())
    }

    pub fn 清除输出端点停顿(&mut self) -> Result<(), String> {
        unsafe {
            let ret = libusb1_sys::libusb_clear_halt(self.设备句柄, self.输出端点);
            if ret != 0 {
                return Err(format!("clear_halt ep_out err {}", ret));
            }
        }
        Ok(())
    }
}

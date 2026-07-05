//! USB 设备 IO 操作：读取 / 写入 / 控制传输 / 清除停顿
//!
//! 使用 nusb Interface 的 control_transfer_in/out 和 bulk transfer_blocking。
//! 后续阶段 2 会将 bulk 读取改为异步流水线（submit/complete）。

use super::device::USB设备;
use super::log::{QUIET_USB_READ, usb_trace};
use log::trace;
use std::sync::atomic::Ordering;
use std::time::Duration;

use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};
use nusb::MaybeFuture;

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
        let ep_addr = 输出端点地址(self);
        let interface = self.获取interface_mut().ok_or("设备未初始化")?;
        let mut ep_out = interface.endpoint::<nusb::transfer::Bulk, nusb::transfer::Out>(ep_addr)
            .map_err(|e| format!("获取输出端点失败: {}", e))?;

        if data.is_empty() {
            // ZLP (Zero Length Packet)
            usb_trace("TX", "USB设备::写入 ZLP", &[]);
            let buf: nusb::transfer::Buffer = Vec::<u8>::new().into();
            ep_out.submit(buf);
            let result = ep_out.wait_next_complete(self.超时)
                .ok_or("ZLP 写入超时")?;
            result.status.map_err(|e| format!("write ZLP err: {:?}", e))?;
            return Ok(0);
        }

        usb_trace("TX", "USB设备::写入", data);

        let buf: nusb::transfer::Buffer = data.to_vec().into();
        ep_out.submit(buf);
        let result = ep_out.wait_next_complete(self.超时)
            .ok_or("写入超时")?;
        result.status.map_err(|e| format!("write err: {:?}", e))?;
        Ok(result.actual_len)
    }

    pub fn 读取(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let 静默 = QUIET_USB_READ.load(Ordering::Relaxed);
        let ep_addr = 输入端点地址(self);

        if !静默 {
            trace!(
                "[USB READ] starting, buf_len={}, timeout={:?}ms",
                buf.len(),
                self.超时.as_millis()
            );
        }

        let interface = self.获取interface_mut().ok_or("设备未初始化")?;
        let mut ep_in = interface.endpoint::<nusb::transfer::Bulk, nusb::transfer::In>(ep_addr)
            .map_err(|e| format!("获取输入端点失败: {}", e))?;

        let buffer = nusb::transfer::Buffer::new(buf.len());
        ep_in.submit(buffer);

        let result = match ep_in.wait_next_complete(self.超时) {
            Some(r) => r,
            None => {
                if !静默 {
                    trace!("[USB READ] timeout");
                }
                ep_in.cancel_all();
                return Ok(0);
            }
        };

        match result.status {
            Ok(()) => {
                let 实际长度 = result.actual_len;
                if !静默 {
                    trace!("[USB READ] got {} bytes", 实际长度);
                }
                let copy_len = 实际长度.min(buf.len());
                buf[..copy_len].copy_from_slice(&result.buffer[..copy_len]);
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
        trace!(
            "[USB READ EXACT] starting, buf_len={}, timeout={:?}ms",
            buf.len(),
            self.超时.as_millis()
        );

        let ep_addr = 输入端点地址(self);
        let interface = self.获取interface_mut().ok_or("设备未初始化")?;
        let mut ep_in = interface.endpoint::<nusb::transfer::Bulk, nusb::transfer::In>(ep_addr)
            .map_err(|e| format!("获取输入端点失败: {}", e))?;

        let mut 总计 = 0usize;
        while 总计 < buf.len() {
            let 剩余 = buf.len() - 总计;
            let buffer = nusb::transfer::Buffer::new(剩余);
            ep_in.submit(buffer);

            let result = match ep_in.wait_next_complete(self.超时) {
                Some(r) => r,
                None => {
                    ep_in.cancel_all();
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
            };

            match result.status {
                Ok(()) => {
                    let 实际长度 = result.actual_len;
                    trace!(
                        "[USB READ EXACT] got {} bytes (total: {}/{})",
                        实际长度, 总计 + 实际长度, buf.len()
                    );
                    if 实际长度 == 0 {
                        std::hint::spin_loop();
                        continue;
                    }
                    let copy_len = 实际长度.min(剩余);
                    buf[总计..总计 + copy_len].copy_from_slice(&result.buffer[..copy_len]);
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

        trace!("[USB READ EXACT] total read: {}/{} bytes", 总计, buf.len());
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
        let interface = self.获取interface_mut().ok_or("设备未初始化")?;

        let control = ControlIn {
            control_type: Self::转换控制类型(rt),
            recipient: Self::转换接收者(rt),
            request: r,
            value: v,
            index: i,
            length: len,
        };

        let result = interface.control_in(control, 超时).wait()
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
            rt, r, v, i, data.len()
        );
        usb_trace("TX", "USB设备::控制传输输出", data);

        let 超时 = self.超时;
        let interface = self.获取interface_mut().ok_or("设备未初始化")?;

        let control = ControlOut {
            control_type: Self::转换控制类型(rt),
            recipient: Self::转换接收者(rt),
            request: r,
            value: v,
            index: i,
            data,
        };

        interface.control_out(control, 超时).wait()
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
        interface.control_out(control, Duration::from_millis(100)).wait()
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
        interface.control_out(control, Duration::from_millis(100)).wait()
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

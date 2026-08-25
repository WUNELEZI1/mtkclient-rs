use crate::usb::device::UsbDevice;
use crate::usb::log::usb_trace;
use log::trace;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{ControlType, Recipient};

/// 获取 OUT 端点地址
fn out_ep_addr(device: &UsbDevice) -> u8 {
    device.out_ep
}

impl UsbDevice {
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
}

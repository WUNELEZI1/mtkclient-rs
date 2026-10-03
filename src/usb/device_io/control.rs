use crate::error::UsbError;
use crate::usb::device::UsbDevice;
use crate::usb::log::usb_trace;
use log::trace;
use nusb::MaybeFuture;
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};

use super::control_index_for_recipient;

impl UsbDevice {
    pub fn ctrl_in(&mut self, rt: u8, r: u8, v: u16, i: u16, len: u16) -> Result<Vec<u8>, String> {
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
            .map_err(|e| format!("{}: {:?}", UsbError::CtrlTransferFailed, e))?;

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
            .map_err(|e| format!("{}: {:?}", UsbError::CtrlTransferFailed, e))?;

        Ok(data.len())
    }

    /// 将 libusb 的 bmRequestType 转换为 nusb ControlType
    pub(crate) fn convert_ctrl_type(bm_request_type: u8) -> ControlType {
        let typ = (bm_request_type >> 5) & 0x03;
        match typ {
            0 => ControlType::Standard,
            1 => ControlType::Class,
            2 => ControlType::Vendor,
            _ => ControlType::Standard,
        }
    }

    /// 将 libusb 的 bmRequestType 转换为 nusb Recipient
    pub(crate) fn convert_receiver(bm_request_type: u8) -> Recipient {
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

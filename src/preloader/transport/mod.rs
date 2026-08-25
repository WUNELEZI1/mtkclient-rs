//! BROM 传输抽象层 — 统一 USB 和串口的读写接口
//!
//! - `BromTransport` trait — 所有 BROM 设备共用的协议原语
//! - `SerialPortTransport` — Windows COM 口实现
//! - `UsbDevice` 的 `BromTransport` impl — LibUSB 实现
//!
//! 设计要点：
//! 1. USB 专属方法（ctrl_transfer / clear_halt）在 trait 上有默认错误实现，
//!    避免在串口实现中重复冗余代码。
//! 2. 串口实现负责扫描可用 COM 口并区分 WinUSB / 串口驱动的 BROM 设备。

use std::time::Duration;

const SERIAL_OPEN_TIMEOUT_MS: u64 = 1000;
const FIND_BROM_INTERVAL_MS: u64 = 200;
const SERIAL_HANDSHAKE_BYTES: [u8; 4] = [0xA0, 0x0A, 0x50, 0x05];

pub(crate) mod serial;
pub(crate) mod usb;

// Re-exports — 保持 crate::preloader::transport::* 的对外可达性
pub use serial::SerialPortTransport;

/// BROM 传输抽象层 — 统一 USB 和串口的读写接口
pub trait BromTransport {
    fn write(&mut self, data: &[u8]) -> Result<usize, String>;
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    fn submit_read_request(&mut self, _len: usize) -> Result<bool, String> {
        Ok(false)
    }
    fn complete_read_request(&mut self, _buf: &mut [u8]) -> Result<usize, String> {
        Err("queued read not supported on this transport".to_string())
    }
    fn read_exact_vec(&mut self, len: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; len];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }
    fn cancel_pending_transfers(&mut self) {}
    /// 清空 USB IN pending data，写入前调用防止读取残留干扰
    fn drain_pending(&mut self) {}
    /// 循环排空 IN 管道残留（不 clear_halt，用于 DA 会话复用）
    fn drain_pipes(&mut self) {}
    fn set_timeout(&mut self, duration: Duration);
    fn get_timeout(&self) -> Duration;
    fn do_handshake(&mut self) -> Result<bool, String>;
    fn is_libusb(&self) -> bool;

    /// 获取 USB VID（仅 UsbDevice 有效，串口返回 None）
    fn get_vid(&self) -> Option<u16> {
        None
    }
    /// 获取 USB PID（仅 UsbDevice 有效，串口返回 None）
    fn get_pid(&self) -> Option<u16> {
        None
    }

    /// 获取 EP_OUT 最大包大小（对齐 Python usblib.write 的 pktsize）
    fn out_ep_max_packet_size(&self) -> u16 {
        512 // 默认回退值，USB 实现会覆盖为真实值
    }

    // USB 专属方法 — 默认返回错误，仅 UsbDevice 实现
    fn ctrl_transfer_out(
        &mut self,
        _req_type: u8,
        _req: u8,
        _value: u16,
        _index: u16,
        _data: &[u8],
    ) -> Result<usize, String> {
        Err("ctrl_transfer_out not supported on this transport".to_string())
    }
    fn ctrl_transfer_in(
        &mut self,
        _req_type: u8,
        _req: u8,
        _value: u16,
        _index: u16,
        _length: u16,
    ) -> Result<Vec<u8>, String> {
        Err("ctrl_transfer_in not supported on this transport".to_string())
    }
    fn clear_halt_in(&mut self) -> Result<(), String> {
        Err("clear_halt_in not supported on this transport".to_string())
    }
    fn clear_halt_out(&mut self) -> Result<(), String> {
        Err("clear_halt_out not supported on this transport".to_string())
    }

    /// USB 总线复位（默认不支持，仅 UsbDevice 实现）
    fn reset_device(&mut self) -> Result<(), String> {
        Err("reset_device not supported on this transport".to_string())
    }

    /// 关闭 USB 设备（默认不支持，仅 UsbDevice 实现）
    fn close_device(&mut self) -> Result<(), String> {
        Err("close_device not supported on this transport".to_string())
    }

    /// 重新打开 USB 设备（默认不支持，仅 UsbDevice 实现）
    fn reopen_device(&mut self, _context: &crate::usb::UsbContext) -> Result<(), String> {
        Err("reopen_device not supported on this transport".to_string())
    }
}

/// BROM 端口检测结果
#[derive(Debug)]
pub enum BromPortResult {
    /// 找到串口驱动的设备，返回 COM 口名称
    SerialPort(String),
    /// 找到 WinUSB 驱动的设备（已安装 libwdi 驱动）
    WinUsbDevice,
}

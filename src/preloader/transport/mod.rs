//! BROM transport abstraction — a unified read/write interface for USB and serial
//!
//! - `BromTransport` trait — protocol primitives shared by all BROM devices
//! - `SerialPortTransport` — Windows COM port implementation
//! - `BromTransport` impl for `UsbDevice` — LibUSB implementation
//!
//! Design notes:
//! 1. USB-only methods (ctrl_transfer / clear_halt) default to an error on the trait,
//!    avoiding duplicated code in the serial implementation.
//! 2. The serial implementation scans available COM ports and distinguishes WinUSB / serial-driven BROM devices.

use std::time::Duration;

const SERIAL_OPEN_TIMEOUT_MS: u64 = 1000;
const FIND_BROM_INTERVAL_MS: u64 = 200;
const SERIAL_HANDSHAKE_BYTES: [u8; 4] = [0xA0, 0x0A, 0x50, 0x05];

pub(crate) mod serial;
pub(crate) mod usb;

// Re-exports — keep crate::preloader::transport::* externally reachable
pub use serial::SerialPortTransport;

/// Under `--log 3`, print wire bytes as a timestamped hex dump.
///
/// Formatting only, no buffering or rewriting: the real-device write path (`UsbDevice::write`) and the
/// simulator (`da::engine`) see the same bytes, enabling byte-for-byte comparison.
pub fn trace_io(dir: &str, data: &[u8]) {
    if !log::log_enabled!(log::Level::Trace) || data.is_empty() {
        return;
    }
    let ts = crate::util::now_iso8601();
    let preview: String = data
        .iter()
        .take(64)
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(" ");
    if data.len() <= 64 {
        log::trace!(
            "[io] {} {} {}B\n{}",
            ts,
            dir,
            data.len(),
            crate::da::engine::hexdump(data)
        );
    } else {
        log::trace!(
            "[io] {} {} {}B (首 64B: {} ...)",
            ts,
            dir,
            data.len(),
            preview
        );
    }
}

/// BROM transport abstraction — a unified read/write interface for USB and serial
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
    /// Flush pending USB IN data; call before writing to avoid leftover reads interfering
    fn drain_pending(&mut self) {}
    /// Drain IN pipe leftovers in a loop (no clear_halt; used for DA session reuse)
    fn drain_pipes(&mut self) {}
    fn set_timeout(&mut self, duration: Duration);
    fn get_timeout(&self) -> Duration;
    fn do_handshake(&mut self) -> Result<bool, String>;
    fn is_libusb(&self) -> bool;

    /// Get the USB VID (valid only for UsbDevice; serial returns None)
    fn get_vid(&self) -> Option<u16> {
        None
    }
    /// Get the USB PID (valid only for UsbDevice; serial returns None)
    fn get_pid(&self) -> Option<u16> {
        None
    }

    /// Get the EP_OUT max packet size (matching pktsize in Python usblib.write)
    fn out_ep_max_packet_size(&self) -> u16 {
        512 // default fallback; the USB implementation overrides it with the real value
    }

    // USB-only methods — default to an error; only UsbDevice implements them
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

    /// USB bus reset (unsupported by default; only UsbDevice implements it)
    fn reset_device(&mut self) -> Result<(), String> {
        Err("reset_device not supported on this transport".to_string())
    }

    /// Close the USB device (unsupported by default; only UsbDevice implements it)
    fn close_device(&mut self) -> Result<(), String> {
        Err("close_device not supported on this transport".to_string())
    }

    /// Reopen the USB device (unsupported by default; only UsbDevice implements it)
    fn reopen_device(&mut self, _context: &crate::usb::UsbContext) -> Result<(), String> {
        Err("reopen_device not supported on this transport".to_string())
    }
}

/// BROM port detection result
#[derive(Debug)]
pub enum BromPortResult {
    /// Serial-driven device found; returns the COM port name
    SerialPort(String),
    /// WinUSB-driven device found (libwdi driver installed)
    WinUsbDevice,
}

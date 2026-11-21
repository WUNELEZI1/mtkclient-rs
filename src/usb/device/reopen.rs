//! USB device reopen / re-enumeration logic.

use super::{REOPEN_POLL_INTERVAL_MS, REOPEN_TIMEOUT_MS, UsbDevice};
use crate::usb::context::UsbContext;
use log::{debug, info};
use std::time::Duration;

/// Whether reconnect polling should continue: retry while elapsed time is below the total timeout.
///
/// Extracted as a pure function for easy unit testing (I/O-free). Matches penumbra `reenumerate`'s
/// "200ms interval + 10s total timeout" semantics.
pub(crate) fn reopen_should_retry(elapsed_ms: u64) -> bool {
    elapsed_ms < REOPEN_TIMEOUT_MS
}

impl UsbDevice {
    /// Reopen the USB device: close old handles, then poll for re-enumeration every 200ms (10s total timeout),
    /// reopening and claiming the interface.
    ///
    /// After a DA reboot, USB re-enumeration usually takes 1~3s; the old implementation waited 200ms once and then called
    /// `new()`, which inevitably failed while the device was not ready. Now it polls every `REOPEN_POLL_INTERVAL_MS`
    /// until it succeeds or the elapsed time exceeds `REOPEN_TIMEOUT_MS`.
    /// Follows penumbra `reenumerate()`.
    pub fn reopen(&mut self, context: &UsbContext) -> Result<(), String> {
        self.close();

        let start = std::time::Instant::now();
        // Assigned by the loop's single break branch, hence no initializer (avoids unused_assignments)
        let last_err: String;

        loop {
            // Wait for the device to settle / re-enumerate
            std::thread::sleep(Duration::from_millis(REOPEN_POLL_INTERVAL_MS));

            match UsbDevice::new(context) {
                Ok(mut new_device) => {
                    // Swap fields
                    self.in_ep_handle = new_device.in_ep_handle.take();
                    self.out_ep_handle = new_device.out_ep_handle.take();
                    self.interface = new_device.interface.take();
                    self.control_interface = new_device.control_interface.take();
                    self.device = new_device.device.take();
                    self.in_ep = new_device.in_ep;
                    self.out_ep = new_device.out_ep;
                    self.in_ep_max_packet_size = new_device.in_ep_max_packet_size;
                    self.iface_num = new_device.iface_num;
                    self.control_iface_num = new_device.control_iface_num;
                    self.in_buf.clear();
                    self.vid = new_device.vid;
                    self.pid = new_device.pid;
                    self.stage = new_device.stage;
                    self.timeout = new_device.timeout;
                    self.device_type = new_device.device_type;
                    self.closed = false;
                    info!("USB 重连成功（耗时 {}ms）", start.elapsed().as_millis());
                    return Ok(());
                }
                Err(e) => {
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    if !reopen_should_retry(elapsed_ms) {
                        last_err = e;
                        break;
                    }
                    debug!(
                        "[USB] 设备尚未重新枚举（已等待 {}ms），继续轮询: {}",
                        elapsed_ms, e
                    );
                }
            }
        }

        Err(format!(
            "USB 重连失败（{}ms 内未重新枚举到设备）: {}",
            REOPEN_TIMEOUT_MS, last_err
        ))
    }
}

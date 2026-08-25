use super::BromTransport;
use crate::usb::UsbDevice;
use std::time::Duration;

/// UsbDevice 实现 BROM 传输
impl BromTransport for UsbDevice {
    fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        UsbDevice::write(self, data)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        UsbDevice::read_exact(self, buf)
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        UsbDevice::read(self, buf)
    }

    fn submit_read_request(&mut self, len: usize) -> Result<bool, String> {
        UsbDevice::submit_read(self, len)
    }

    fn complete_read_request(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        UsbDevice::complete_read(self, buf)
    }

    fn read_exact_vec(&mut self, len: usize) -> Result<Vec<u8>, String> {
        UsbDevice::read_exact_vec(self, len)
    }

    fn cancel_pending_transfers(&mut self) {
        UsbDevice::cancel_pending_transfers(self)
    }

    fn drain_pending(&mut self) {
        UsbDevice::drain_pending(self)
    }

    fn drain_pipes(&mut self) {
        UsbDevice::drain_pipes(self)
    }

    fn set_timeout(&mut self, duration: Duration) {
        UsbDevice::set_timeout(self, duration);
    }

    fn get_timeout(&self) -> Duration {
        UsbDevice::get_timeout(self)
    }

    fn do_handshake(&mut self) -> Result<bool, String> {
        UsbDevice::do_handshake(self)
    }

    fn is_libusb(&self) -> bool {
        true
    }

    fn get_vid(&self) -> Option<u16> {
        Some(self.vid)
    }

    fn get_pid(&self) -> Option<u16> {
        Some(self.pid)
    }

    fn out_ep_max_packet_size(&self) -> u16 {
        UsbDevice::out_ep_max_packet_size(self)
    }

    fn ctrl_transfer_out(
        &mut self,
        req_type: u8,
        req: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<usize, String> {
        UsbDevice::ctrl_out(self, req_type, req, value, index, data)?;
        Ok(data.len())
    }

    fn ctrl_transfer_in(
        &mut self,
        req_type: u8,
        req: u8,
        value: u16,
        index: u16,
        length: u16,
    ) -> Result<Vec<u8>, String> {
        UsbDevice::ctrl_in(self, req_type, req, value, index, length)
    }

    fn clear_halt_in(&mut self) -> Result<(), String> {
        UsbDevice::clear_halt_in(self)
    }

    fn clear_halt_out(&mut self) -> Result<(), String> {
        UsbDevice::clear_halt_out(self)
    }

    fn reset_device(&mut self) -> Result<(), String> {
        UsbDevice::reset_device(self)
    }

    fn close_device(&mut self) -> Result<(), String> {
        UsbDevice::close(self);
        Ok(())
    }

    fn reopen_device(&mut self, context: &crate::usb::UsbContext) -> Result<(), String> {
        UsbDevice::reopen(self, context)
    }
}

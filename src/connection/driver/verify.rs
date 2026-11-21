//! WinUSB device open verification (legacy module)
//!
//! The former `get_device_instance_id` (enumerating USB device instance IDs via pnputil) was a
//! legacy implementation kept for interface compatibility. It has no call sites on any platform
//! any more (verification now happens by opening the device via nusb, see `poll_libusb_ready`).

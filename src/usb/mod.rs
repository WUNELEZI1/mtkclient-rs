//! USB communication subsystem.
//!
//! Handles low-level USB transport, device interaction, and trace logging.
//!
//! # Architecture
//! - `context` - USB stage enum and device detection (nusb needs no explicit context)
//! - `device` - USB device wrapper and lifecycle management (nusb Interface/Device)
//! - `device_io` - USB device I/O (read/write/control transfer/clear halt)
//! - `device_handshake` - BROM handshake protocol
//! - `diag` - USB connection diagnostics and status checks
//! - `log` - USB traffic trace log (--usb-log)

#[path = "context.rs"]
pub mod context;
pub mod device;
#[path = "device_handshake.rs"]
pub mod device_handshake;
pub mod device_io;
#[path = "diag.rs"]
pub mod diag;
#[path = "log.rs"]
pub mod log;

// Public re-exports; keep external call sites unchanged.
#[allow(unused_imports)]
pub use context::{UsbContext, UsbStage, get_first_mtk_vid_pid};
pub use device::UsbDevice;
#[allow(unused_imports)]
pub use log::{set_usb_log_switch, set_usb_read_quiet, usb_trace};
// Note: usb_trace_tx! / usb_trace_rx! are exported at the crate root via #[macro_export];
// callers must use crate::usb_trace_tx! / crate::usb_trace_rx!

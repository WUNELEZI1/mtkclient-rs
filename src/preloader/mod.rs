//! Preloader / BROM protocol layer
//!
//! - `core`     — Preloader struct definition
//! - `transport` — BromTransport trait + SerialPortTransport + UsbDevice impl
//! - `brom_init` — init / sync_brom / echo_* / get_hw_code / rword / flush_input
//! - `brom_read`   — read32_brom / read32_brom_batch (0xD1 protocol)
//! - `brom_reg_access` — generic register access via the 0xDA protocol
//! - `brom_io`   — send_da / jump_da / jump_bl / get_me_id / get_soc_id

pub(crate) mod brom_init;
#[path = "brom_io.rs"]
pub(crate) mod brom_io;
#[path = "brom_read.rs"]
pub(crate) mod brom_read;
#[path = "brom_reg_access.rs"]
pub(crate) mod brom_reg_access;
#[path = "core.rs"]
pub(crate) mod core;
pub(crate) mod transport;

// Public API re-exports (fully compatible with the original preloader.rs)
#[allow(unused_imports)]
pub use core::Preloader;
#[allow(unused_imports)]
pub use transport::{BromPortResult, BromTransport, SerialPortTransport};

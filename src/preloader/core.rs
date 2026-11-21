//! Preloader core struct
//!
//! ```text
//! pub struct Preloader {
//!     pub device: Box<dyn BromTransport>, // device transport abstraction (USB / serial)
//!     pub is_preloader_mode: bool,         // whether already in Preloader/DA mode
//!     pub chip: Option<ChipConfig>,        // chip config (filled after HW code match)
//!     pub brom_initialized: bool,          // whether BROM init succeeded (handshake + watchdog off + sync)
//! }
//! ```
//!
//! Impl methods live in sibling modules:
//! - `transport/`      — BromTransport trait + SerialPortTransport + UsbDevice impl
//! - `brom_init/`      — init / sync_brom / echo_* / get_hw_code / rword / flush_input
//! - `brom_reg_access.rs` — brom_register_access
//! - `brom_io.rs`      — send_da / jump_da / jump_bl / get_me_id / get_soc_id
//! - `brom_read.rs`    — read32_brom (closely tied to rword and other base reads)

use super::transport::BromTransport;
use crate::da::loader::header::DaRegion;
use crate::system::config::ChipConfig;
use std::sync::{Arc, Mutex};

pub struct Preloader {
    pub device: Box<dyn BromTransport>,
    pub is_preloader_mode: bool,
    pub chip: Option<ChipConfig>,
    /// Whether BROM init completed (true after a successful init)
    pub brom_initialized: bool,
    /// DA file pre-parsed by a background thread (includes header parse results)
    pub da_parsed: Option<Arc<Mutex<Option<(Vec<u8>, Vec<DaRegion>)>>>>,
    /// User-specified DA file path (--da); defaults to the built-in path when empty
    pub da_path: String,
}

impl Preloader {
    pub fn new(device: Box<dyn BromTransport>) -> Self {
        Preloader {
            device,
            is_preloader_mode: false,
            chip: None,
            brom_initialized: false,
            da_parsed: None,
            da_path: String::new(),
        }
    }
}

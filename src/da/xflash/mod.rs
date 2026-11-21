//! DA/XFlash subsystem
//!
//! Handles DA (Download Agent) upload, XFlash protocol communication, and partition operations
//!
//! # Architecture
//! - `protocol` - XFlash protocol primitives (pack3, xread, status, ack)
//! - `emi` - EMI data extraction
//! - `da_load` - main DA loading flow
//! - `diag` - device diagnostics and info retrieval
//! - `io` - partition read/write operations

#[path = "da_load.rs"]
mod da_load;
#[path = "diag.rs"]
mod diag;
#[path = "emi.rs"]
mod emi;
mod io;
pub mod protocol;

// Explicitly re-export the public protocol primitives that `pub use protocol::*;`
// previously exposed, so `crate::da::xflash::*` keeps the exact same public surface.
#[allow(unused_imports)]
pub use protocol::{
    AckResult, CMD_BOOT_TO, CMD_FORMAT, CMD_INIT_EXT_RAM, CMD_MAGIC, CMD_READ_DATA,
    CMD_SETUP_ENVIRONMENT, CMD_SETUP_HW_INIT_PARAMS, CMD_SHUTDOWN, CMD_SYNC_SIGNAL, CMD_WRITE_DATA,
    DA_EXT_BOOT_ADDR, DATA_TYPE_FLOW, DATA_TYPE_MESSAGE, GET_DA_VER_CMD, GET_PKT_LEN,
    SET_META_BOOT_MODE, SET_PKT_LEN, SHUTDOWN_BOOTMODE_HOME_SCREEN, chunk_checksum, pack3,
};

use crate::preloader::Preloader;

/// Full EMMC info struct (from send_devctrl(0x01010C))
pub struct EmmcInfo {
    /// Boot1 size (bytes)
    pub boot1_size: u64,
    /// Boot2 size (bytes)
    pub boot2_size: u64,
    /// RPMB size (bytes), if available
    pub rpmb_size: u64,
    /// Total user data size (bytes)
    pub user_size: u64,
    /// Block size (bytes)
    pub block_size: u32,
    /// EMMC type description string
    pub emmc_type: String,
    /// CID register content (16 bytes)
    pub cid: Vec<u8>,
}

/// DAXFlash struct handling the XFlash protocol
pub struct DAXFlash<'a> {
    pub preloader: &'a mut Preloader,
    /// EMI data — written by the emi module's `load_preloader_emi`
    pub(crate) emi: Option<Vec<u8>>,
    pub(crate) emi_version: u32,
    pub(crate) da2_data: Vec<u8>,
    pub(crate) da2_base_addr: u64,
    pub daext: bool,
    pub(crate) last_gpt_data: Option<Vec<u8>>,
    pub patch_da: bool,
    pub da_x_speed: u8,
    /// Cached DA file data to avoid re-reading in upload_da1/da2
    pub(crate) da_file_data: Option<Vec<u8>>,
    /// Currently used preloader file path, saved to .state
    pub(crate) preloader_path: Option<String>,
    /// Optional DA query failures recorded in this process, synced to .state after session save
    pub(crate) optional_query_failures: Vec<String>,
    /// Dynamic partition (super) metadata cache
    pub(crate) super_metadata: Option<crate::partition::lp::SuperMetadata>,
    /// Cached active A/B slot ('a' / 'b'), filled by slot::detect_active_slot
    pub(crate) active_slot: Option<char>,
}

impl<'a> DAXFlash<'a> {
    pub fn new(preloader: &'a mut Preloader) -> Self {
        DAXFlash {
            preloader,
            emi: None,
            emi_version: 0,
            da2_data: Vec::new(),
            da2_base_addr: 0x40000000,
            daext: false,
            last_gpt_data: None,
            super_metadata: None,
            active_slot: None,
            patch_da: true,
            da_x_speed: 3,
            da_file_data: None,
            preloader_path: None,
            optional_query_failures: Vec::new(),
        }
    }
}

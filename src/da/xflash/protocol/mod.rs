//! XFlash protocol primitive layer.
//!
//! This module holds the lowest-level packet/state-machine primitives of the XFlash
//! protocol, reused by all high-level DAXFlash methods (setup/upload/read/erase/...):
//! - constant definitions (CMD_MAGIC, all CMD_* command codes)
//! - packet header packing (pack3)
//! - sync/status reads (xflash_sync, status, xread, xread_data)
//! - ACK responses (ack, send_ack)
//! - device control commands (send_devctrl, get/set_*_devctrl wrappers)
//!
//! Design principle: keep a 1:1 mapping to the protocol layer of Python mtkclient
//! xflash_lib.py, with no dependency on upper-layer DAXFlash business logic, so it
//! stays easy to unit test and trace.

pub(crate) mod ack;
pub(crate) mod devctrl;
pub(crate) mod read;
pub(crate) mod rpmb;
pub(crate) mod shutdown;

// =============================================================================
// XFlash command constants
// =============================================================================

/// Magic identifier for all XFlash packet headers.
pub const CMD_MAGIC: u32 = 0xFEEEEEEF;
/// XFlash header `data_type`: normal flow packet (command response / data).
/// See penumbra `DataType::Flow`.
pub const DATA_TYPE_FLOW: u32 = 0x1;
/// XFlash header `data_type`: device asynchronous message packet (DA log / status notice).
/// See penumbra `DataType::Message`. The DA may inject such packets at any time; if
/// their payload is not consumed, subsequent headers get misaligned (protocol desync).
pub const DATA_TYPE_MESSAGE: u32 = 0x2;
pub const CMD_SYNC_SIGNAL: u32 = 0x434E5953;
pub const CMD_SETUP_ENVIRONMENT: u32 = 0x010100;
pub const CMD_SETUP_HW_INIT_PARAMS: u32 = 0x010101;
pub const CMD_INIT_EXT_RAM: u32 = 0x01000A;
pub const CMD_BOOT_TO: u32 = 0x010008;
pub const CMD_READ_DATA: u32 = 0x010005; // XFlash read-partition command
pub const CMD_WRITE_DATA: u32 = 0x010004; // write-data command

pub const CMD_FORMAT: u32 = 0x010003; // format command

// =============================================================================
// Full XFlash DA command code table (aligned with MTKAuthPass.exe + mtkclient xflash_lib.py)
// =============================================================================

// --- Basic commands (0x01xxxx) ---
pub const CMD_SHUTDOWN: u32 = 0x010007; // * shutdown/reboot (with bootmode argument)

// --- Device control subcommands (0x02xxxx) - sent via CMD_DEVICE_CTRL ---
pub const SET_META_BOOT_MODE: u32 = 0x020006; // * set Meta Boot Mode

// --- Info query subcommands (0x04xxxx) ---
pub const GET_PKT_LEN: u32 = 0x040007; // get packet length
/// Note: in mtkclient's Cmd table, 0x040008 is actually **GET_RANDOM_ID**, not
/// "set packet length". This name is kept only for compatibility with historical
/// event handlers and the explicit experimental `MTK_FORCE_PKT_LEN` switch in the
/// write path (see `partition/write/cmd.rs`); regular read/write paths **must not**
/// send it, otherwise it is effectively "a GET with an argument" that desyncs the
/// DA state machine, and the following READ_DATA returns 0xC0010003
/// (Unsupported command).
pub const SET_PKT_LEN: u32 = 0x040008;

// --- Other commands ---
pub const GET_DA_VER_CMD: u32 = 0x010106; // get DA version/chip info

// --- DA extended boot address ---
pub const DA_EXT_BOOT_ADDR: u32 = 0x4FFF0000;

// =============================================================================
// Timeout constants (ms)
// =============================================================================

/// Maximum timeout (ms) for DA control reads (status / xread / xread_data) waiting
/// for the first byte. The serial default read timeout is only
/// `SERIAL_OPEN_TIMEOUT_MS` (1s), but a slow Preloader serial DA may take more than
/// 1s to respond to heavy commands (GET_PKT_LEN, setup_hw, send_devctrl, write
/// status, etc.), causing `Operation timed out` on status reads and command failure.
/// DA control reads therefore use a longer first-byte timeout. USB reads use a
/// self-built loop, where a long timeout is equally safe.
/// Note: this cooperates with the fast-fail of `with_short_timeout` - if the caller
/// has explicitly set a shorter timeout (e.g. 200ms/50ms for optional queries), take
/// `max(current, this value)` so the fast-fail semantics are not overridden.
const DA_CTRL_TIMEOUT_MS: u64 = 5000;
/// Short timeout for optional queries (ms)
const SHORT_QUERY_TIMEOUT_MS: u64 = 200;
/// SLA query timeout (ms)
const SLA_QUERY_TIMEOUT_MS: u64 = 50;

// =============================================================================
// Shutdown bootmode (aligned with xflash_lib.py ShutDownModes)
// =============================================================================

/// bootmode values for DA SHUTDOWN (aligned with mtkclient xflash_lib.py ShutDownModes).
///
/// This project always uses HOME_SCREEN (=1, reboot into the system) when rebooting
/// through DA SHUTDOWN: targets such as fastboot / recovery / fastbootd / meta are
/// handled by the caller first writing misc/para or using XML SET-BOOT-MODE, then
/// booting the system with HOME_SCREEN and letting LK read the boot arguments, so no
/// other DA-side bootmode is needed.
pub const SHUTDOWN_BOOTMODE_HOME_SCREEN: u32 = 1;

// =============================================================================
// ACK response enum - replaces raw u32 return values
// =============================================================================

/// ACK response enum - replaces raw u32 return values.
#[derive(Debug, PartialEq)]
pub enum AckResult {
    /// status == 0, device ready, continue transfer
    Continue,
    /// status != 0, transfer aborted or device error
    Terminated(u32),
}

// =============================================================================
// Header helpers
// =============================================================================

/// pack3: builds an XFlash argument header (magic(4) + data_type(4) + length(4)).
pub fn pack3(magic: u32, data_type: u32, length: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&magic.to_le_bytes());
    buf[4..8].copy_from_slice(&data_type.to_le_bytes());
    buf[8..12].copy_from_slice(&length.to_le_bytes());
    buf
}

/// 16-bit additive checksum of a write chunk (aligned with penumbra `download_data`).
///
/// The DA expects a 16-bit additive sum before each block: sum all bytes as u32 and
/// keep the low 16 bits. `& 0xFFFF` is essential - the old implementation used an
/// untruncated 32-bit wrapping sum while the DA only compares the low 16 bits, so a
/// nonzero high part made the DA report a checksum failure. Bytes are accumulated in
/// batches of 8 to reduce loop iterations.
pub fn chunk_checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 8 <= data.len() {
        sum = sum
            .wrapping_add(data[i] as u32)
            .wrapping_add(data[i + 1] as u32)
            .wrapping_add(data[i + 2] as u32)
            .wrapping_add(data[i + 3] as u32)
            .wrapping_add(data[i + 4] as u32)
            .wrapping_add(data[i + 5] as u32)
            .wrapping_add(data[i + 6] as u32)
            .wrapping_add(data[i + 7] as u32);
        i += 8;
    }
    while i < data.len() {
        sum = sum.wrapping_add(data[i] as u32);
        i += 1;
    }
    sum & 0xFFFF
}

// =============================================================================
// Private helpers
// =============================================================================

fn hex_str(data: &[u8]) -> String {
    crate::util::hex_str(data)
}

// =============================================================================
// Public re-exports (keeps crate::da::xflash::protocol::* reachable)
// =============================================================================

// =============================================================================
// Unit tests - protocol primitive regression checks
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack3_layout() {
        let pkt = pack3(0xDEADBEEF, 0x12345678, 0x9ABCDEF0);
        // magic
        assert_eq!(pkt[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
        // data_type
        assert_eq!(pkt[4..8], [0x78, 0x56, 0x34, 0x12]);
        // length
        assert_eq!(pkt[8..12], [0xF0, 0xDE, 0xBC, 0x9A]);
    }

    #[test]
    fn ack_result_equality() {
        assert_eq!(AckResult::Continue, AckResult::Continue);
        assert_ne!(AckResult::Continue, AckResult::Terminated(1));
    }

    #[test]
    fn chunk_checksum_is_additive_16bit() {
        assert_eq!(chunk_checksum(&[]), 0);
        assert_eq!(chunk_checksum(&[0x01]), 0x01);
        assert_eq!(chunk_checksum(&[0xFF, 0x01]), 0x100);
        // Matches the naive implementation (including samples crossing the 8-byte batch boundary)
        let naive = |d: &[u8]| d.iter().map(|&b| b as u32).sum::<u32>() & 0xFFFF;
        let sample: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        assert_eq!(chunk_checksum(&sample), naive(&sample));
        // The tail must also be counted when length is not a multiple of 8
        assert_eq!(chunk_checksum(&[0u8; 17]), 0);
        assert_eq!(chunk_checksum(&[0u8; 16]), 0);
    }

    #[test]
    fn chunk_checksum_truncates_to_16_bits() {
        // 255 * 300 = 76500 = 0x12AD4 -> low 16 bits 0x2AD4 (the old implementation returned 0x12AD4)
        let data = vec![0xFFu8; 300];
        assert_eq!(chunk_checksum(&data), 0x2AD4);
        assert!(chunk_checksum(&data) <= 0xFFFF);
    }
}

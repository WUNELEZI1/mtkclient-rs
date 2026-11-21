//! MTK wire protocol constants (BROM + XFlash), shared by `da::engine` (host side) and `transport::vdev` (device side).
//!
//! Only **values** live here, no logic, so the dependency direction `core ← transport ← da` is not violated.
//! The values themselves must match the existing implementation in `da::xflash::protocol`,
//! which the conformance test at the end of this module enforces:
//! if the two sides drift, the test fails immediately.

// =============================================================================
// BROM layer
// =============================================================================

/// BROM handshake byte sequence (sent byte by byte; the device replies bitwise inverted).
pub const BROM_HANDSHAKE: [u8; 4] = [0xA0, 0x0A, 0x50, 0x05];

/// GET_HW_CODE: returns 4 bytes BE (hw_code, hw_sub_code)
pub const BROM_GET_HW_CODE: u8 = 0xFD;
/// GET_TARGET_CONFIG: returns 4 bytes BE config + 2 bytes BE status
pub const BROM_GET_TARGET_CONFIG: u8 = 0xD8;
/// BROM sync: returns 8 bytes of hw info
pub const BROM_SYNC: u8 = 0xFC;
/// SEND_DA: 4-byte address/length params + data body
pub const BROM_SEND_DA: u8 = 0xD7;
/// JUMP_DA: 4-byte jump address (sent twice, the second triggers)
pub const BROM_JUMP_DA: u8 = 0xD5;
/// Status word returned when SEND_DA is rejected by the SLA
pub const BROM_STATUS_SLA_REQUIRED: u16 = 0x1D0D;
/// Sync byte emitted by DA after a successful JUMP_DA
pub const BROM_JUMP_ACK: u8 = 0xC0;
/// Reply byte of BROM sync FE (observed as 0x03)
pub const BROM_SYNC_FE_ACK: u8 = 0x03;

/// Simulation-only: exploit handshake command (does not exist on real devices; recognized only by `transport::vdev`).
///
/// On real hardware kamakiri2 / carbonara are timing-sensitive USB control-transfer sequences
/// that cannot be reproduced in a pure byte-stream simulation; the simulated device uses one
/// explicit command to mean "the device side accepted this exploit", so the chain
/// "plan → execute → capability upgrade" itself can be validated.
pub const SIM_EXPLOIT_CMD: u8 = 0x1F;

// =============================================================================
// XFlash (DA) layer
// =============================================================================

/// Packet header magic
pub const XFLASH_MAGIC: u32 = 0xFEEEEEEF;
/// data_type: flow packet
pub const XFLASH_TYPE_FLOW: u32 = 0x1;
/// data_type: device async message (log)
pub const XFLASH_TYPE_MESSAGE: u32 = 0x2;

/// Packet header length
pub const XFLASH_HEADER_LEN: usize = 12;

/// DA init-complete signal (sent proactively by the device)
pub const XFLASH_SYNC_SIGNAL: u32 = 0x434E5953;

/// Command: READ_DATA (read flash)
pub const XFLASH_CMD_READ_DATA: u32 = 0x010005;
/// Command: WRITE_DATA (write flash)
pub const XFLASH_CMD_WRITE_DATA: u32 = 0x010004;
/// Command: FORMAT (erase / format)
pub const XFLASH_CMD_FORMAT: u32 = 0x010003;
/// Command: DEVICE_CTRL prefix
pub const XFLASH_CMD_DEVICE_CTRL: u32 = 0x010009;
/// Command: GET_DA_VER
pub const XFLASH_CMD_GET_DA_VER: u32 = 0x010106;
/// Sub-command: eMMC info query (mtkclient `Cmd.GET_EMMC_INFO = 0x040001`, same value after zero-extension)
pub const XFLASH_CMD_EMMC_INFO: u32 = 0x0004_0001;
/// devctrl: DA extensions CUSTOM_ACK
pub const XFLASH_DEVCTRL_ACK: u32 = 0x0F0000;
/// devctrl: CUSTOM_SET_STORAGE
pub const XFLASH_DEVCTRL_SET_STORAGE: u32 = 0x0F0005;
/// devctrl: CC_OPTIONAL_DOWNLOAD_ACT (must be sent after a write, otherwise DA considers the transaction unfinished)
///
/// Value taken from mtkclient `Library/DA/xflash/xflash_param.py`:
/// `CC_OPTIONAL_DOWNLOAD_ACT = 0x080005`.
pub const XFLASH_DEVCTRL_CC_OPTIONAL_DOWNLOAD_ACT: u32 = 0x08_0005;

/// Write param block length (storage/parttype/addr/size + 32-byte NandExtension)
pub const XFLASH_WRITE_PARAM_LEN: usize = 56;
/// Erase (FORMAT) param block length: same as above
pub const XFLASH_FORMAT_PARAM_LEN: usize = 56;
/// Param chunk size (matches mtkclient `send_param`: payload is sent in 0x200 chunks)
pub const XFLASH_PARAM_CHUNK: usize = 0x200;

/// FORMAT in progress
pub const XFLASH_STATUS_CONTINUE: u32 = 0x4004_0004;
/// FORMAT complete
pub const XFLASH_STATUS_COMPLETE: u32 = 0x4004_0005;
/// Write checksum error (the device side rejects this chunk)
pub const XFLASH_STATUS_CHECKSUM: u32 = 0xC003_0006;
/// Expected magic for the DA extensions ACK
pub const XFLASH_ACK_MAGIC: u32 = 0xA1A2A3A4;
/// Status word for an unsupported command
pub const XFLASH_STATUS_UNSUPPORTED: u32 = 0xC001_0004;

/// READ_DATA param block length (56 bytes, rest zero-padded)
pub const XFLASH_READ_PARAM_LEN: usize = 56;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::da::xflash::protocol as p;

    #[test]
    fn constants_match_legacy_protocol_layer() {
        // The host side (engine) and the existing DAXFlash implementation must use the same command codes,
        // otherwise simulation passes while the real device fails.
        assert_eq!(XFLASH_MAGIC, p::CMD_MAGIC);
        assert_eq!(XFLASH_TYPE_FLOW, p::DATA_TYPE_FLOW);
        assert_eq!(XFLASH_TYPE_MESSAGE, p::DATA_TYPE_MESSAGE);
        assert_eq!(XFLASH_CMD_READ_DATA, p::CMD_READ_DATA);
        assert_eq!(XFLASH_CMD_WRITE_DATA, p::CMD_WRITE_DATA);
        assert_eq!(XFLASH_CMD_FORMAT, p::CMD_FORMAT);
        assert_eq!(XFLASH_CMD_DEVICE_CTRL, 0x010009);
        assert_eq!(XFLASH_CMD_GET_DA_VER, p::GET_DA_VER_CMD);
        assert_eq!(XFLASH_SYNC_SIGNAL, p::CMD_SYNC_SIGNAL);
    }

    #[test]
    fn handshake_bytes_are_mtk_standard() {
        assert_eq!(BROM_HANDSHAKE, [0xA0, 0x0A, 0x50, 0x05]);
        // The device reply is the bytewise inverse
        assert_eq!(BROM_HANDSHAKE.map(|b| !b), [0x5F, 0xF5, 0xAF, 0xFA]);
    }
}

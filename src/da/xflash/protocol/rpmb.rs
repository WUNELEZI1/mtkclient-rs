//! RPMB extension commands (CUSTOM_RPMB_*, matching the mtkclient xflash extension)
//!
//! RPMB (Replay Protected Memory Block) is a protected area of eMMC/UFS;
//! reads and writes must be HMAC-signed by the device using the RPMB key. The DA extension provides the following commands:
//!
//! | Command | Value | Purpose |
//! |---|---|---|
//! | `CUSTOM_RPMB_SET_KEY` | 0x0F0006 | write the 32-byte RPMB key (verified by device echo) |
//! | `CUSTOM_RPMB_PROG_KEY`| 0x0F0007 | let the platform program the key itself (some chips) |
//! | `CUSTOM_RPMB_INIT`    | 0x0F0008 | initialize the RPMB session and return the device-derived key |
//! | `CUSTOM_RPMB_READ`    | 0x0F0009 | read in 0x100-byte frames |
//! | `CUSTOM_RPMB_WRITE`   | 0x0F000A | write in 0x100-byte frames |
//!
//! Unlike `send_devctrl`, RPMB commands do **not** return data right after `cmd`;
//! parameters (sector/sectors/frame) must be sent first, so this module uses
//! `rpmb_cmd`, which only does the two-stage handshake, avoiding premature xread timeouts.

use super::{CMD_MAGIC, pack3};
use crate::da::xflash::DAXFlash;
use log::{debug, info};

pub const CUSTOM_RPMB_SET_KEY: u32 = 0x0F0006;
#[allow(dead_code)]
pub const CUSTOM_RPMB_PROG_KEY: u32 = 0x0F0007; // reserved: some platforms use it to program the key
pub const CUSTOM_RPMB_INIT: u32 = 0x0F0008;
pub const CUSTOM_RPMB_READ: u32 = 0x0F0009;
pub const CUSTOM_RPMB_WRITE: u32 = 0x0F000A;

/// The RPMB key is always 32 bytes
pub const RPMB_KEY_LEN: usize = 32;
/// Length of a single RPMB frame
pub const RPMB_FRAME_LEN: usize = 0x100;

/// JEDEC RPMB result codes (matching the mtkclient `rpmb_error` table)
const RPMB_ERRORS: &[&str] = &[
    "OK",                                    // 0x0000
    "General failure",                       // 0x0001
    "Authentication failure",                // 0x0002
    "Counter failure",                       // 0x0003
    "Address failure",                       // 0x0004
    "Write failure",                         // 0x0005
    "Read failure",                          // 0x0006
    "Authentication key not yet programmed", // 0x0007
    "Invalid number of bytes",               // 0x0008
];

/// Translate an RPMB result code into readable text
pub fn rpmb_error_msg(code: u16) -> String {
    match RPMB_ERRORS.get(code as usize) {
        Some(msg) => format!("{} (0x{:04X})", msg, code),
        None => format!("Unknown RPMB error (0x{:04X})", code),
    }
}

/// Parse a 32-byte hex key (tolerates spaces and a `0x` prefix)
pub fn parse_key_hex(s: &str) -> Result<[u8; RPMB_KEY_LEN], String> {
    let cleaned: String = s
        .trim()
        .trim_start_matches("0x")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != '-')
        .collect();
    if cleaned.len() != RPMB_KEY_LEN * 2 {
        return Err(format!(
            "RPMB 密钥必须是 {} 字节（{} 个 hex 字符），当前 {} 个字符",
            RPMB_KEY_LEN,
            RPMB_KEY_LEN * 2,
            cleaned.len()
        ));
    }
    let mut key = [0u8; RPMB_KEY_LEN];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16).map_err(|_| {
            format!(
                "RPMB 密钥含非法十六进制字符: {}",
                &cleaned[i * 2..i * 2 + 2]
            )
        })?;
    }
    Ok(key)
}

/// Generate a 32-byte random RPMB key (prefers the system entropy source, falls back to time entropy)
pub fn generate_key() -> [u8; RPMB_KEY_LEN] {
    let mut key = [0u8; RPMB_KEY_LEN];
    // Note: do not use std::fs::read("/dev/urandom") — that char device never returns EOF,
    // so it reads until memory is exhausted (a flaw in the old genkey implementation).
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut key).is_ok() {
            return key;
        }
    }
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    for b in key.iter_mut() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        *b = (seed >> 24) as u8;
    }
    key
}

impl<'a> DAXFlash<'a> {
    /// Send a 4-byte integer parameter over XFlash
    fn rpmb_send_u32(&mut self, v: u32) -> Result<(), String> {
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&v.to_le_bytes())?;
        Ok(())
    }

    /// Send a data-block parameter over XFlash
    fn rpmb_send_data(&mut self, data: &[u8]) -> Result<(), String> {
        let pkt = pack3(CMD_MAGIC, 0x01, data.len() as u32);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(data)?;
        Ok(())
    }

    /// Mirrors Python `cmd()`: DEVICE_CTRL → cmd two-stage handshake, no data read
    fn rpmb_cmd(&mut self, cmd: u32) -> Result<bool, String> {
        if !self.daext {
            return Err("DA Extensions 未启用，无法使用 RPMB 命令".to_string());
        }
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;
        let st1 = self.status()?;

        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&cmd.to_le_bytes())?;
        let st2 = self.status()?;

        debug!(
            "[rpmb] cmd=0x{:06X} stage1=0x{:08X} stage2=0x{:08X}",
            cmd, st1, st2
        );
        Ok(st1 == 0 && st2 == 0)
    }

    /// Write the RPMB key and verify the device echo (**irreversible**: the key cannot be read back)
    pub fn custom_rpmb_set_key(&mut self, key: &[u8]) -> Result<(), String> {
        if key.len() != RPMB_KEY_LEN {
            return Err(format!(
                "RPMB 密钥长度必须为 {} 字节，当前 {}",
                RPMB_KEY_LEN,
                key.len()
            ));
        }
        if !self.rpmb_cmd(CUSTOM_RPMB_SET_KEY)? {
            return Err("设备拒绝 CUSTOM_RPMB_SET_KEY（该平台可能不支持软件烧写密钥）".to_string());
        }
        self.rpmb_send_data(key)?;
        let echo = self.xread_data()?;
        let status = self.status()?;
        if status != 0 {
            return Err(format!("写入 RPMB 密钥失败: status=0x{:08X}", status));
        }
        if echo != key {
            return Err(format!(
                "RPMB 密钥回显不一致（写入 {} 字节，回显 {} 字节）",
                key.len(),
                echo.len()
            ));
        }
        info!("{}", crate::i18n::t("log_rpmb_key_ok"));
        Ok(())
    }

    /// Initialize an RPMB session; when `key` is provided, program the key first.
    /// Returns the device-derived RPMB key (empty on some platforms).
    pub fn custom_rpmb_init(&mut self, key: Option<&[u8]>) -> Result<Option<Vec<u8>>, String> {
        if let Some(k) = key {
            self.custom_rpmb_set_key(k)?;
        }
        if !self.rpmb_cmd(CUSTOM_RPMB_INIT)? {
            return Err("CUSTOM_RPMB_INIT 失败（RPMB 未初始化或平台不支持）".to_string());
        }
        // Platforms 0x1209/0x1129 require mode=1
        let hw_code = self.preloader.chip.as_ref().map(|c| c.hw_code).unwrap_or(0);
        let mode: u32 = if hw_code == 0x1209 || hw_code == 0x1129 {
            1
        } else {
            0
        };
        self.rpmb_send_u32(mode)?;
        let status = self.status()?;
        if status != 0 {
            return Err(format!(
                "RPMB 初始化失败: {}",
                rpmb_error_msg(status as u16)
            ));
        }
        let derived = self.xread_data()?;
        let _ = self.status();
        if derived.is_empty() {
            info!("RPMB 初始化成功（设备未返回派生密钥）");
            Ok(None)
        } else {
            info!("RPMB 初始化成功，派生密钥: {}", hex(&derived));
            Ok(Some(derived))
        }
    }

    /// Read RPMB (in 0x100-byte frames)
    pub fn custom_rpmb_read(&mut self, sector: u32, sectors: u32) -> Result<Vec<u8>, String> {
        if sectors == 0 {
            return Ok(Vec::new());
        }
        if !self.rpmb_cmd(CUSTOM_RPMB_READ)? {
            return Err("CUSTOM_RPMB_READ 不被支持".to_string());
        }
        self.rpmb_send_u32(sector)?;
        self.rpmb_send_u32(sectors)?;

        let mut out = Vec::with_capacity(sectors as usize * RPMB_FRAME_LEN);
        for i in 0..sectors {
            let frame = self.xread_data()?;
            if frame.len() != RPMB_FRAME_LEN {
                let code = if frame.len() >= 2 {
                    u16::from_le_bytes([frame[0], frame[1]])
                } else {
                    0xFFFF
                };
                let _ = self.status();
                return Err(format!(
                    "读取 sector 0x{:X} 失败: {}",
                    sector + i,
                    rpmb_error_msg(code)
                ));
            }
            out.extend_from_slice(&frame);
        }
        let status = self.status()?;
        if status != 0 {
            return Err(format!("RPMB 读取结束状态异常: 0x{:08X}", status));
        }
        Ok(out)
    }

    /// Write RPMB (data length must be a multiple of 0x100)
    pub fn custom_rpmb_write(&mut self, sector: u32, data: &[u8]) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }
        if !data.len().is_multiple_of(RPMB_FRAME_LEN) {
            return Err(format!(
                "RPMB 写入长度必须是 {} 的整数倍，当前 {}",
                RPMB_FRAME_LEN,
                data.len()
            ));
        }
        let sectors = (data.len() / RPMB_FRAME_LEN) as u32;
        if !self.rpmb_cmd(CUSTOM_RPMB_WRITE)? {
            return Err("CUSTOM_RPMB_WRITE 不被支持".to_string());
        }
        self.rpmb_send_u32(sector)?;
        self.rpmb_send_u32(sectors)?;

        for i in 0..sectors as usize {
            let off = i * RPMB_FRAME_LEN;
            self.rpmb_send_data(&data[off..off + RPMB_FRAME_LEN])?;
            let resp = self.xread_data()?;
            let code = if resp.len() >= 2 {
                u16::from_le_bytes([resp[0], resp[1]])
            } else {
                0
            };
            if code != 0 {
                let _ = self.status();
                return Err(format!(
                    "写入 sector 0x{:X} 失败: {}",
                    sector + i as u32,
                    rpmb_error_msg(code)
                ));
            }
        }
        let status = self.status()?;
        if status != 0 {
            return Err(format!("RPMB 写入结束状态异常: 0x{:08X}", status));
        }
        Ok(())
    }

    /// Query the device RPMB size in bytes; 0 means unknown/unsupported
    pub fn rpmb_size(&mut self) -> Result<u64, String> {
        let info = self.get_emmc_info()?;
        Ok(info.rpmb_size)
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02X}", x)).collect()
}

/// For CLI display: format the RPMB size in MB
pub fn fmt_rpmb_size(bytes: u64) -> String {
    if bytes == 0 {
        "未知".to_string()
    } else if bytes >= 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes / 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_mapping() {
        assert!(rpmb_error_msg(0).contains("OK"));
        assert!(rpmb_error_msg(0x0007).contains("not yet programmed"));
        assert!(rpmb_error_msg(0x1234).contains("Unknown"));
    }

    #[test]
    fn parse_key_accepts_common_formats() {
        let raw = "00112233445566778899AABBCCDDEEFF00112233445566778899AABBCCDDEEFF";
        let k = parse_key_hex(raw).unwrap();
        assert_eq!(k[0], 0x00);
        assert_eq!(k[31], 0xFF);
        assert_eq!(parse_key_hex(&format!("0x{}", raw)).unwrap(), k);
        assert_eq!(
            parse_key_hex(
                "00112233 44556677 8899AABB CCDDEEFF 00112233 44556677 8899AABB CCDDEEFF"
            )
            .unwrap(),
            k
        );
    }

    #[test]
    fn parse_key_rejects_bad_input() {
        assert!(parse_key_hex("00").is_err());
        assert!(parse_key_hex(&"GG".repeat(32)).is_err());
    }

    #[test]
    fn generate_key_is_random_and_sized() {
        let a = generate_key();
        let b = generate_key();
        assert_eq!(a.len(), RPMB_KEY_LEN);
        assert_ne!(a, b, "两次生成的密钥不应相同");
    }

    #[test]
    fn size_formatting() {
        assert_eq!(fmt_rpmb_size(0), "未知");
        assert_eq!(fmt_rpmb_size(4096 * 1024), "4.00 MB");
    }
}

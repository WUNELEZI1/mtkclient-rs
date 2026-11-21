//! Image extraction commands.
//!
//! - `cmd_dumppreloader` — extract the Preloader
//!   - DA mode (Preloader mode has loaded DA / BROM bypassed and upload_da done):
//!     read 512KB in one shot from device memory (preloader partition addr=0) via XFlash
//!     `read_data`; no Kamakiri BROM exploit needed — faster, more stable, and not
//!     subject to BROM memory read locks.
//!     Matches GeekFlashTool's "export Preloader in DA mode" behavior.
//!   - Pure BROM mode (DA not loaded): fall back to exploit payload injection (legacy path).

use crate::color::Colorize;
use log::{info, warn};

use crate::da::DAXFlash;
use crate::usb::UsbContext;

/// Bytes read from device memory for the Preloader (512KB, enough to cover the preloader image + BRLYT header)
const PRELOADER_DUMP_SIZE: u64 = 0x80000;
/// Measured on GeekFlashTool: DA-mode preloader read uses parttype=1, addr=0, and content starts with "EMMC_BOOT"
const PRELOADER_DUMP_ADDR: u64 = 0x0;
const PRELOADER_DUMP_PARTTYPE: u32 = 0x1;
const PRELOADER_MAGIC: &[u8] = b"EMMC_BOOT";

/// Extract the Preloader.
///
/// Prefer DA mode, reading directly from device memory (fast, stable, no exploit);
/// fall back to the BROM exploit path only when DA is not loaded (pure BROM) or the DA read fails validation.
///
/// `context` is only needed by the BROM exploit fallback (a direct DA read does not use it). In a DA-mode
/// session the device is no longer in BROM state, so exploits cannot run — pass `None`; if the DA read fails
/// and `context` is `None`, this returns a clear error instead of failing silently.
pub fn cmd_dumppreloader(
    da: &mut DAXFlash,
    context: Option<&UsbContext>,
) -> Result<(), Box<dyn std::error::Error>> {
    if da.daext {
        match read_preloader_via_da(da) {
            Ok((data, filename)) => return save_preloader(data, filename),
            Err(e) => warn!("DA 模式读取 Preloader 失败 ({}), 回退 BROM exploit 路径", e),
        }
    }

    // Fallback: extract via BROM exploit payload injection (legacy path).
    // Call dump_preloader_payload directly without bypass_security first, because
    // dump_preloader_payload injects the payload internally and bypass would change the BROM state
    let ctx = context.ok_or(
        "DUMPPRELOADER 在 DA 模式下读取失败，且无法回退到 BROM exploit（设备已在 DA 模式）",
    )?;
    let (data, filename) = da
        .preloader
        .dump_preloader_payload(ctx)
        .map_err(|e| format!("Exploit 提取失败: {}", e))?;
    save_preloader(data, filename)
}

/// DA mode: read the preloader straight from device memory via XFlash read_data
fn read_preloader_via_da(da: &mut DAXFlash) -> Result<(Vec<u8>, String), String> {
    let data = da.readflash_data_ex(
        PRELOADER_DUMP_ADDR,
        PRELOADER_DUMP_SIZE,
        PRELOADER_DUMP_PARTTYPE,
    )?;
    if data.len() < PRELOADER_MAGIC.len() || &data[0..PRELOADER_MAGIC.len()] != PRELOADER_MAGIC {
        return Err("读出数据缺少 EMMC_BOOT 头，可能非 Preloader".to_string());
    }
    let filename = extract_preloader_name(&data);
    Ok((data, filename))
}

/// Guess the filename from the preloader binary (first printable run ending in .bin and containing "preloader")
fn extract_preloader_name(data: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(data) {
        for w in s.split(|c: char| !(c.is_ascii_graphic() || c == ' ')) {
            if w.len() > 4 && w.ends_with(".bin") && w.contains("preloader") {
                return w.trim().to_string();
            }
        }
    }
    "preloader_dumped.bin".to_string()
}

/// Save the extracted result
fn save_preloader(data: Vec<u8>, filename: String) -> Result<(), Box<dyn std::error::Error>> {
    if data.is_empty() {
        warn!("提取完成但未收到有效数据");
        return Ok(());
    }
    let output = if filename.is_empty() {
        "preloader_dumped.bin".to_string()
    } else {
        filename
    };
    std::fs::write(&output, &data)?;
    info!(
        "{}",
        format!("Preloader 已提取并保存到: {}", output).green()
    );
    Ok(())
}

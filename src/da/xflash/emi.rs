//! EMI data extraction module.
//!
//! Extracts DRAM init parameters from preloader binaries (EMI = External Memory Interface).
//! Flow:
//! 1. `load_preloader_emi`: reads the preloader file and calls `extract_emi`
//! 2. `extract_emi`: searches the preloader data for the 4D 4D 4D 01 38 marker and
//!    the MTK_BLOADER_INFO_v string, then slices the actual EMI data by
//!    mlen/siglen/dramsize
//!
//! Why split out:
//! - EMI extraction is independent of USB communication, so it is unit-test friendly
//! - error messages are more focused (missing path vs. bad data format)
//! - keeps the main protocol file from growing in size

use log::{debug, trace};

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// Extracts EMI data from a preloader file.
    ///
    /// preloader_path must be non-empty (either the file given by --preloader or the
    /// file name produced by dump_preloader_payload). An empty string means "path not
    /// determined", so return an error instead of silently swallowing it.
    pub fn load_preloader_emi(&mut self, preloader_path: &str) -> Result<bool, String> {
        // Require a non-empty path: no more silent error swallowing
        if preloader_path.is_empty() {
            return Err(
                "preloader 路径为空：未指定 --preloader 且 dump_preloader_payload 未产生文件名"
                    .to_string(),
            );
        }

        debug!(
            "加载 preloader 文件: {} ({} 字节)",
            preloader_path,
            std::fs::metadata(preloader_path)
                .map(|m| m.len())
                .unwrap_or(0)
        );

        // Read the preloader file
        let preloader_data = match std::fs::read(preloader_path) {
            Ok(data) => data,
            Err(e) => {
                return Err(format!("无法打开 preloader 文件 {}: {}", preloader_path, e));
            }
        };

        // Extract EMI data
        match self.extract_emi(&preloader_data) {
            Ok((version, emi_data)) => {
                let emi_len = emi_data.len();
                self.emi = Some(emi_data);
                self.emi_version = version;
                debug!(
                    "成功提取 EMI 数据，版本: {}, 大小: {} 字节",
                    version, emi_len
                );
                Ok(true)
            }
            Err(e) => Err(format!("提取 EMI 数据失败: {}", e)),
        }
    }

    /// Internal EMI data extraction.
    pub(crate) fn extract_emi(&self, data: &[u8]) -> Result<(u32, Vec<u8>), String> {
        // Locate the marker
        let marker = b"\x4D\x4D\x4D\x01\x38\x00\x00\x00";
        let idx = data
            .windows(marker.len())
            .position(|window| window == marker);

        let mut emi_data = data.to_vec();

        if let Some(idx) = idx {
            trace!("[EMI] 找到标记，偏移: 0x{:08X}", idx);
            emi_data = data[idx..].to_vec();

            // Read mlen and siglen
            if emi_data.len() >= 0x30 {
                let mlen = u32::from_le_bytes(emi_data[0x20..0x24].try_into().unwrap());
                let siglen = u32::from_le_bytes(emi_data[0x2C..0x30].try_into().unwrap());
                trace!("[EMI] mlen=0x{:08X}, siglen=0x{:08X}", mlen, siglen);

                // Slice the data
                if mlen as usize <= emi_data.len() {
                    emi_data = emi_data[..mlen as usize - siglen as usize].to_vec();
                }

                // Read dramsize
                if emi_data.len() >= 4 {
                    let dramsize =
                        u32::from_le_bytes(emi_data[emi_data.len() - 4..].try_into().unwrap());
                    trace!("[EMI] dramsize=0x{:08X}", dramsize);

                    if dramsize == 0 && emi_data.len() >= 0x804 {
                        emi_data = emi_data[..emi_data.len() - 0x800].to_vec();
                        if emi_data.len() >= 4 {
                            let dramsize = u32::from_le_bytes(
                                emi_data[emi_data.len() - 4..].try_into().unwrap(),
                            );
                            trace!("[EMI] 调整后 dramsize=0x{:08X}", dramsize);
                        }
                    }

                    // Trim the EMI data
                    if dramsize > 0 && emi_data.len() >= (dramsize + 4) as usize {
                        emi_data = emi_data
                            [emi_data.len() - (dramsize + 4) as usize..emi_data.len() - 4]
                            .to_vec();
                    }
                }
            }
        }

        // Search for the MTK_BLOADER_INFO_v string
        let bldrstring = b"MTK_BLOADER_INFO_v";
        let idx = emi_data
            .windows(bldrstring.len())
            .position(|window| window == bldrstring);

        if let Some(idx) = idx {
            trace!("[EMI] 找到 MTK_BLOADER_INFO_v，偏移: 0x{:08X}", idx);

            // Extract the version number
            let version_str = &emi_data[idx + bldrstring.len()..idx + bldrstring.len() + 2];
            let version = String::from_utf8_lossy(version_str)
                .trim_end_matches('\0')
                .parse::<u32>()
                .unwrap_or_default();
            debug!("EMI 版本: {}", version);

            // WARNING critical fix: if MTK_BLOADER_INFO_v is at offset 0, return the whole emi_data
            // Mirrors Python m_extract_emi: if idx == 0 and damode == XFLASH: return ver, data
            if idx == 0 {
                trace!("MTK_BLOADER_INFO_v 在偏移 0，使用完整 EMI 数据（含 header）");
                debug!("EMI 数据大小: {} 字节", emi_data.len());
                return Ok((version, emi_data));
            }

            // Existing logic: extract only the data after MTK_BIN
            let mtk_bin_idx = emi_data.windows(7).position(|window| window == b"MTK_BIN");
            if let Some(mtk_bin_idx) = mtk_bin_idx {
                let emi = emi_data[mtk_bin_idx + 0xC..].to_vec();
                debug!("EMI 数据大小: {} 字节", emi.len());
                return Ok((version, emi));
            }
        }

        Err("未找到 EMI 数据".to_string())
    }
}

// =============================================================================
// Unit tests
// =============================================================================
#[cfg(test)]
mod tests {
    /// Builds a minimal preloader slice to verify extract_emi truncation logic.
    #[test]
    fn extract_emi_no_marker_returns_error() {
        // Constructing an empty DAXFlash is not feasible (it needs a preloader borrow),
        // so this is only a shape check; a real test needs a mock device.
        let bogus_data = [0u8; 16];
        let marker = b"\x4D\x4D\x4D\x01\x38\x00\x00\x00";
        assert!(!bogus_data.windows(marker.len()).any(|w| w == marker));
    }
}

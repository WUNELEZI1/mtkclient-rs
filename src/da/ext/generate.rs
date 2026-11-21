//! DA Extensions generation
//!
//! - `generate_da_extensions` — fills the da_x.bin template with key function addresses from the DA2 segment
//! - `find_binary_wildcard`   — single-byte wildcard byte search

use log::{trace, warn};

use crate::da::DAXFlash;
use crate::da::ext::patch::find_binary;

impl<'a> DAXFlash<'a> {
    /// Generate the DA extensions binary data
    /// Matches Python xflash.py patch() lines 69-182
    pub(crate) fn generate_da_extensions(&self) -> Option<Vec<u8>> {
        let da2 = &self.da2_data;
        let da2address = self.da2_base_addr;

        // Load the template at runtime from data/generic/da_x.bin (no longer hardcoded into the binary)
        let mut daextdata = match crate::system::compress::read_file_auto_decompress(
            std::path::Path::new("data/generic/da_x.bin"),
        ) {
            Ok(data) => data,
            Err(e) => {
                warn!("加载 DA extensions 模板 da_x.bin 失败: {}", e);
                return None;
            }
        };

        // 1. register_devctrl: \x38\xB5\x05\x46\x0C\x20
        let register_devctrl = find_binary(da2, b"\x38\xB5\x05\x46\x0C\x20", 0);

        // 2. mmc_get_card: \x4B\x4F\xF4\x3C\x72 (after found, -1)
        //    alternative: \xA3\xEB\x00\x13\x18\x1A\x02\xEB\x00\x10 (after found, -10)
        let mut mmc_get_card = find_binary(da2, b"\x4B\x4F\xF4\x3C\x72", 0);
        if let Some(pos) = mmc_get_card {
            mmc_get_card = Some(pos.saturating_sub(1));
        } else {
            mmc_get_card = find_binary(da2, b"\xA3\xEB\x00\x13\x18\x1A\x02\xEB\x00\x10", 0)
                .map(|p| p.saturating_sub(10));
        }

        // 3. mmc_set_part_config: loop-search \xC3\x69\x0A\x46\x10\xB5 until the +20 position is \xB3\x21
        //    alternative: \xC3\x69\x13\xF0\x01\x03
        let mut mmc_set_part_config: Option<usize> = None;
        let mut pos = 0;
        while let Some(found) = find_binary(da2, b"\xC3\x69\x0A\x46\x10\xB5", pos) {
            if found + 22 <= da2.len() && da2[found + 20] == 0xB3 && da2[found + 21] == 0x21 {
                mmc_set_part_config = Some(found);
                break;
            }
            pos = found + 1;
        }
        if mmc_set_part_config.is_none() {
            mmc_set_part_config = find_binary(da2, b"\xC3\x69\x13\xF0\x01\x03", 0);
        }

        // 4. mmc_rpmb_send_command: \xF8\xB5\x06\x46\x9D\xF8\x18\x50
        //    alternative: \x2D\xE9\xF0\x41\x4F\xF6\xFD\x74
        let mut mmc_rpmb_send_command = find_binary(da2, b"\xF8\xB5\x06\x46\x9D\xF8\x18\x50", 0);
        if mmc_rpmb_send_command.is_none() {
            mmc_rpmb_send_command = find_binary(da2, b"\x2D\xE9\xF0\x41\x4F\xF6\xFD\x74", 0);
        }

        // 5. g_ufs_hba: three alternative patterns; extract the 4-byte pointer that follows
        let mut g_ufs_hba: Option<u32> = None;
        let mut ptr_g_ufs_hba_found = false;

        // Alternative 1: \x20\x46\x0B\xB0\xBD\xE8\xF0\x83\x00\xBF, read 4 bytes at +10
        if let Some(p) = find_binary(da2, b"\x20\x46\x0B\xB0\xBD\xE8\xF0\x83\x00\xBF", 0)
            && p + 14 <= da2.len()
        {
            g_ufs_hba = Some(u32::from_le_bytes([
                da2[p + 10],
                da2[p + 11],
                da2[p + 12],
                da2[p + 13],
            ]));
            ptr_g_ufs_hba_found = true;
        }
        // Alternative 2: \x20\x46\x0D\xB0\xBD\xE8\xF0\x83, read 4 bytes at +8
        if g_ufs_hba.is_none()
            && let Some(p) = find_binary(da2, b"\x20\x46\x0D\xB0\xBD\xE8\xF0\x83", 0)
            && p + 12 <= da2.len()
        {
            g_ufs_hba = Some(u32::from_le_bytes([
                da2[p + 8],
                da2[p + 9],
                da2[p + 10],
                da2[p + 11],
            ]));
            ptr_g_ufs_hba_found = true;
        }
        // Alternative 3: \x21\x46\x02\xF0\x02\xFB\x1B\xE6\x00\xBF, read 4 bytes at +10+0x8
        if g_ufs_hba.is_none()
            && let Some(p) = find_binary(da2, b"\x21\x46\x02\xF0\x02\xFB\x1B\xE6\x00\xBF", 0)
            && p + 18 <= da2.len()
        {
            g_ufs_hba = Some(u32::from_le_bytes([
                da2[p + 18],
                da2[p + 19],
                da2[p + 20],
                da2[p + 21],
            ]));
            ptr_g_ufs_hba_found = true;
        }

        // 6. ufshcd_get_free_tag and ufshcd_queuecommand (needed only by UFS devices)
        let ufshcd_get_free_tag = if ptr_g_ufs_hba_found {
            find_binary_wildcard(da2, &[0xB5, 0x00, 0xB1, 0x90, 0xF8], 1)
        } else {
            None
        };
        let ufshcd_queuecommand = if ptr_g_ufs_hba_found {
            find_binary(da2, b"\x2D\xE9\xF8\x43\x01\x27", 0)
        } else {
            None
        };

        // Critical check: register_devctrl and mmc_get_card must be found
        if register_devctrl.is_none() || mmc_get_card.is_none() {
            warn!("无法找到关键函数 (register_devctrl 或 mmc_get_card)");
            return None;
        }

        // Compute absolute addresses (Thumb mode: addr + base | 1)
        let register_devctrl_addr =
            (register_devctrl.unwrap() as u32).wrapping_add(da2address as u32) | 1;
        let mmc_get_card_addr = (mmc_get_card.unwrap() as u32).wrapping_add(da2address as u32) | 1;
        let mmc_set_part_config_addr = mmc_set_part_config
            .map(|p| (p as u32).wrapping_add(da2address as u32) | 1)
            .unwrap_or(0);
        let mmc_rpmb_send_command_addr = mmc_rpmb_send_command
            .map(|p| (p as u32).wrapping_add(da2address as u32) | 1)
            .unwrap_or(0);
        let ufshcd_get_free_tag_addr = ufshcd_get_free_tag
            .map(|p| (p as u32).wrapping_add((da2address - 1) as u32) | 1)
            .unwrap_or(0);
        let ufshcd_queuecommand_addr = ufshcd_queuecommand
            .map(|p| (p as u32).wrapping_add(da2address as u32) | 1)
            .unwrap_or(0);
        let g_ufs_hba_addr = g_ufs_hba.unwrap_or(0);

        // efuse address
        let efuse_addr: u32 = 0x11ce0000;

        // Locate placeholders and replace them
        let register_ptr = daextdata.windows(4).position(|w| w == b"\x11\x11\x11\x11");
        let mmc_get_card_ptr = daextdata.windows(4).position(|w| w == b"\x22\x22\x22\x22");
        let mmc_set_part_config_ptr = daextdata.windows(4).position(|w| w == b"\x33\x33\x33\x33");
        let mmc_rpmb_send_command_ptr = daextdata.windows(4).position(|w| w == b"\x44\x44\x44\x44");
        let ufshcd_queuecommand_ptr = daextdata.windows(4).position(|w| w == b"\x55\x55\x55\x55");
        let ufshcd_get_free_tag_ptr = daextdata.windows(4).position(|w| w == b"\x66\x66\x66\x66");
        let ptr_g_ufs_hba_ptr = daextdata.windows(4).position(|w| w == b"\x77\x77\x77\x77");
        let efuse_addr_ptr = daextdata.windows(4).position(|w| w == b"\x88\x88\x88\x88");

        if let Some(p) = register_ptr {
            daextdata[p..p + 4].copy_from_slice(&register_devctrl_addr.to_le_bytes());
        }
        if let Some(p) = mmc_get_card_ptr {
            daextdata[p..p + 4].copy_from_slice(&mmc_get_card_addr.to_le_bytes());
        }
        if let Some(p) = mmc_set_part_config_ptr {
            daextdata[p..p + 4].copy_from_slice(&mmc_set_part_config_addr.to_le_bytes());
        }
        if let Some(p) = mmc_rpmb_send_command_ptr {
            daextdata[p..p + 4].copy_from_slice(&mmc_rpmb_send_command_addr.to_le_bytes());
        }
        if let Some(p) = ufshcd_queuecommand_ptr {
            daextdata[p..p + 4].copy_from_slice(&ufshcd_queuecommand_addr.to_le_bytes());
        }
        if let Some(p) = ufshcd_get_free_tag_ptr {
            daextdata[p..p + 4].copy_from_slice(&ufshcd_get_free_tag_addr.to_le_bytes());
        }
        if let Some(p) = ptr_g_ufs_hba_ptr {
            daextdata[p..p + 4].copy_from_slice(&g_ufs_hba_addr.to_le_bytes());
        }
        if let Some(p) = efuse_addr_ptr {
            daextdata[p..p + 4].copy_from_slice(&efuse_addr.to_le_bytes());
        }

        trace!("DA Extensions 生成成功:");
        trace!("  register_devctrl 地址: 0x{:08X}", register_devctrl_addr);
        trace!("  mmc_get_card 地址: 0x{:08X}", mmc_get_card_addr);
        trace!(
            "  mmc_set_part_config 地址: 0x{:08X}",
            mmc_set_part_config_addr
        );
        trace!(
            "  mmc_rpmb_send_command 地址: 0x{:08X}",
            mmc_rpmb_send_command_addr
        );

        Some(daextdata)
    }
}

/// Byte search with a wildcard (the byte at wildcard_idx is ignored)
pub fn find_binary_wildcard(data: &[u8], pattern: &[u8], wildcard_idx: usize) -> Option<usize> {
    if pattern.is_empty() || wildcard_idx >= pattern.len() {
        return None;
    }
    for i in 0..data.len().saturating_sub(pattern.len()) {
        let mut found = true;
        for (j, &p) in pattern.iter().enumerate() {
            if j == wildcard_idx {
                continue; // skip the wildcard position
            }
            if data[i + j] != p {
                found = false;
                break;
            }
        }
        if found {
            return Some(i);
        }
    }
    None
}

//! DA Extensions 生成
//!
//! - `generate_da_extensions` — 用 DA2 段中的关键函数地址填充 da_x.bin 模板
//! - `find_binary_wildcard`   — 单字节通配符字节搜索

use log::{trace, warn};

use crate::da_extension::DAXFlash;
use crate::da_ext_cmd::{DA_EXTENSIONS_TEMPLATE, patch::find_binary};

impl<'a> DAXFlash<'a> {
    /// 生成 DA extensions 二进制数据
    /// 对齐 Python xflash.py patch() 第 69-182 行
    pub(crate) fn generate_da_extensions(&self) -> Option<Vec<u8>> {
        let da2 = &self.da2_data;
        let da2address = self.da2_base_addr;

        // 复制模板
        let mut daextdata = DA_EXTENSIONS_TEMPLATE.to_vec();

        // 1. register_devctrl: \x38\xB5\x05\x46\x0C\x20
        let register_devctrl = find_binary(da2, b"\x38\xB5\x05\x46\x0C\x20", 0);

        // 2. mmc_get_card: \x4B\x4F\xF4\x3C\x72 (找到后 -1)
        //    备选: \xA3\xEB\x00\x13\x18\x1A\x02\xEB\x00\x10 (找到后 -10)
        let mut mmc_get_card = find_binary(da2, b"\x4B\x4F\xF4\x3C\x72", 0);
        if let Some(pos) = mmc_get_card {
            mmc_get_card = Some(pos.saturating_sub(1));
        } else {
            mmc_get_card = find_binary(da2, b"\xA3\xEB\x00\x13\x18\x1A\x02\xEB\x00\x10", 0)
                .map(|p| p.saturating_sub(10));
        }

        // 3. mmc_set_part_config: 循环搜索 \xC3\x69\x0A\x46\x10\xB5，直到 +20 位置是 \xB3\x21
        //    备选: \xC3\x69\x13\xF0\x01\x03
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
        //    备选: \x2D\xE9\xF0\x41\x4F\xF6\xFD\x74
        let mut mmc_rpmb_send_command = find_binary(da2, b"\xF8\xB5\x06\x46\x9D\xF8\x18\x50", 0);
        if mmc_rpmb_send_command.is_none() {
            mmc_rpmb_send_command = find_binary(da2, b"\x2D\xE9\xF0\x41\x4F\xF6\xFD\x74", 0);
        }

        // 5. g_ufs_hba: 三种备选 pattern，提取后面的 4 字节指针
        let mut g_ufs_hba: Option<u32> = None;
        let mut ptr_g_ufs_hba_found = false;

        // 备选 1: \x20\x46\x0B\xB0\xBD\xE8\xF0\x83\x00\xBF，+10 位置读 4 字节
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
        // 备选 2: \x20\x46\x0D\xB0\xBD\xE8\xF0\x83，+8 位置读 4 字节
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
        // 备选 3: \x21\x46\x02\xF0\x02\xFB\x1B\xE6\x00\xBF，+10+0x8 位置读 4 字节
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

        // 6. ufshcd_get_free_tag 和 ufshcd_queuecommand（只有 UFS 设备需要）
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

        // 关键检查：register_devctrl 和 mmc_get_card 必须找到
        if register_devctrl.is_none() || mmc_get_card.is_none() {
            warn!("无法找到关键函数 (register_devctrl 或 mmc_get_card)");
            return None;
        }

        // 计算绝对地址（Thumb 模式: addr + base | 1）
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

        // efuse 地址
        let efuse_addr: u32 = 0x11ce0000;

        // 查找占位符并替换
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

/// 带通配符的字节搜索（wildcard_idx 位置的字节忽略）
pub fn find_binary_wildcard(data: &[u8], pattern: &[u8], wildcard_idx: usize) -> Option<usize> {
    if pattern.is_empty() || wildcard_idx >= pattern.len() {
        return None;
    }
    for i in 0..data.len().saturating_sub(pattern.len()) {
        let mut found = true;
        for (j, &p) in pattern.iter().enumerate() {
            if j == wildcard_idx {
                continue; // 跳过通配符位置
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

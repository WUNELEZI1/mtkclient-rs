use crate::da_xflash::{CMD_MAGIC, DAXFlash, SET_META_BOOT_MODE, pack3};
use log::{debug, info, warn};

/// DA extensions 模板（预编译的 da_x.bin）
const DA_EXTENSIONS_TEMPLATE: &[u8] =
    include_bytes!("../mtkclient-2.0.1/mtkclient/payloads/da_x.bin");

impl<'a> DAXFlash<'a> {
    /// 在二进制数据中搜索模式（支持 `.` 0x2E 作为单字节通配符）
    /// 对齐 Python utils.py find_binary()
    fn find_binary(data: &[u8], pattern: &[u8], start_pos: usize) -> Option<usize> {
        if pattern.is_empty() || start_pos >= data.len() {
            return None;
        }
        let search_data = &data[start_pos..];

        // 按 0x2E ('.') 分割为多段（. 是通配符）
        let mut segments: Vec<&[u8]> = Vec::new();
        let mut last = 0;
        for (i, &b) in pattern.iter().enumerate() {
            if b == b'.' {
                if i > last {
                    segments.push(&pattern[last..i]);
                }
                last = i + 1;
            }
        }
        if last < pattern.len() {
            segments.push(&pattern[last..]);
        }

        // 如果没有通配符，直接精确匹配
        if segments.len() <= 1 {
            return search_data
                .windows(pattern.len())
                .position(|w| w == pattern)
                .map(|p| p + start_pos);
        }

        // 搜索第一段，对每个位置检查后续段（跳过通配符字节）
        let seg0 = segments[0];
        let mut search_start = 0;
        while search_start + seg0.len() <= search_data.len() {
            if let Some(idx) = search_data[search_start..]
                .windows(seg0.len())
                .position(|w| w == seg0)
            {
                let base = search_start + idx;
                let mut pos = base + seg0.len();
                let mut ok = true;
                for seg in &segments[1..] {
                    pos += 1; // skip wildcard byte (.)
                    if pos + seg.len() > search_data.len() {
                        ok = false;
                        break;
                    }
                    if search_data[pos..pos + seg.len()] != **seg {
                        ok = false;
                        break;
                    }
                    pos += seg.len();
                }
                if ok {
                    return Some(base + start_pos);
                }
                search_start = base + 1;
            } else {
                break;
            }
        }
        None
    }

    /// DA1/DA2 公共修补 patches（8 个相同）
    const COMMON_PATCHES: &'static [(&'static [u8], &'static [u8], &'static str)] = &[
        (
            b"\xA3\x68\x7B\xB1\x28\x46",
            b"\x01\x23\xA3\x60\x28\x46",
            "oppo security",
        ),
        (
            b"\xB3\xF5\x80\x7F\x01\xD1",
            b"\xB3\xF5\x80\x7F\x01\xD1\x4F\xF0\x00\x00\x4F\xF0\x00\x00\x70\x47",
            "mt6739 c30",
        ),
        (
            b"\xB3\xF5\x80\x7F\x04\xBF\x4F\xF4\x80\x73\x05\xF0\x11\xB8\x4F\xF0\xFF\x30\x70\x47",
            b"\xB3\xF5\x80\x7F\x04\xBF\x4F\xF4\x80\x73\x4F\xF0\x00\x00\x4F\xF0\x00\x00\x70\x47",
            "regular",
        ),
        (
            b"\x10\xB5\x0C\x68\x02\x68",
            b"\x10\xB5\x01\x20\x10\xBD",
            "ram blacklist",
        ),
        (
            b"\x08\xB5\x10\x4B\x7B\x44\x1B\x68\x1B\x68",
            b"\x00\x20\x70\x47\x00\x00\x00\x00\x00\x00",
            "seclib_sec_usbdl_enabled",
        ),
        (b"Preloader Start", b"Patched L Start", "Patched loader msg"),
        (
            b"\xF0\xB5\x8B\xB0\x02\xAE\x20\x25\x0C\x46\x07\x46",
            b"\x00\x20\x70\x47\x00\x00\x00\x00\x00\x20\x53\x74\x61\x72\x74",
            "sec_img_auth",
        ),
        (
            b"\xFF\xC0\xF3\x40\x00\x08\xBD",
            b"\xFF\x4F\xF0\x00\x00\x08\xBD",
            "get_vfy_policy",
        ),
    ];

    /// 应用修补 patches
    fn apply_patches(data: &mut [u8], patches: &[(&[u8], &[u8], &str)], tag: &str) -> bool {
        let mut patched = false;
        for (pattern, replacement, name) in patches {
            if let Some(idx) = Self::find_binary(data, pattern, 0) {
                let end = idx + replacement.len();
                if end <= data.len() {
                    data[idx..end].copy_from_slice(replacement);
                    info!("已修补 {}: {}", tag, name);
                    patched = true;
                }
            }
        }
        patched
    }

    /// DA1 patch：对齐 Python patch_da1 + patch_preloader_security_da1
    pub(crate) fn patch_da1(data: &mut [u8]) {
        let mut patched = Self::apply_patches(data, Self::COMMON_PATCHES, "DA1");

        // DA1 独有：hash_check3
        let hash3_pattern = b"\x14\x2C\xF6\x2E\xFE\xE7";
        if let Some(idx) = Self::find_binary(data, hash3_pattern, 0) {
            let replacement = b"\x00\x00\x00\x00\x00\x00";
            let end = idx + replacement.len();
            if end <= data.len() {
                data[idx..end].copy_from_slice(replacement);
                info!("已修补 DA1: hash_check3");
                patched = true;
            }
        }

        // DA1 独有：da_version_check
        if let Some(idx) = Self::find_binary(data, b"\x1F\xB5\x00\x23\x01\xA8\x00\x93\x00\xF0", 0) {
            data[idx..idx + 4].copy_from_slice(b"\x00\x20\x70\x47");
            info!("已修补 DA1: 版本检查");
            patched = true;
        } else {
            warn!("DA1 version check 模式未找到");
        }

        // DA1 独有：hash_check
        if let Some(idx) = Self::find_binary(data, b"\x04\x00\x07\xC0", 0) {
            let end = idx + 4;
            if end <= data.len() {
                data[idx..end].copy_from_slice(b"\x00\x00\x00\x00");
                info!("已修补 DA1: hash_check");
                patched = true;
            }
        }

        // DA1 独有：hash_check2
        if let Some(idx) = Self::find_binary(data, b"\xCC\xF2\x07\x09", 0) {
            let replacement = b"\x4F\xF0\x00\x09";
            let end = idx + replacement.len();
            if end <= data.len() {
                data[idx..end].copy_from_slice(replacement);
                info!("已修补 DA1: hash_check2");
                patched = true;
            }
        }

        if !patched {
            warn!("DA1 无修补应用");
        }
    }

    /// DA2 patch：对齐 Python patch_da2 + patch_preloader_security_da2
    pub(crate) fn patch_da2(data: &mut [u8]) {
        let mut patched = Self::apply_patches(data, Self::COMMON_PATCHES, "DA2");

        // DA2 独有：huawei security
        if let Some(idx) = Self::find_binary(data, b"\x01\x2B\x03\xD1\x01\x23", 0) {
            data[idx..idx + 4].copy_from_slice(b"\x00\x00\x00\x00");
            info!("已修补 DA2: huawei security");
            patched = true;
        }

        // DA2 独有：oppo security（复杂逻辑）
        if Self::find_binary(data, b"[oplus]", 0).is_some()
            || Self::find_binary(data, b"[OPPO]", 0).is_some()
        {
            if let Some(oppo) = Self::find_binary(data, b"\x0A\x00\x00\xE0.\x00\x00\xE0", 0)
                && oppo >= 4
            {
                let auth_flag_ptr = u32::from_le_bytes(data[oppo - 4..oppo].try_into().unwrap());
                info!(
                    "已修补 DA2: oppo security (mt6765, ptr=0x{:08X})",
                    auth_flag_ptr
                );
                patched = true;
            }

            let mut oppo_pos = 0;
            let mut oppo_patched = false;
            while oppo_pos < data.len() {
                if let Some(oppo) = data[oppo_pos..]
                    .windows(6)
                    .position(|w| w == b"\x01\x3B\x01\x2B\x08\xD9")
                {
                    let actual = oppo_pos + oppo;
                    data[actual..actual + 4].copy_from_slice(b"\x01\x20\x08\xBD");
                    oppo_patched = true;
                    oppo_pos = actual + 1;
                } else {
                    break;
                }
            }
            if oppo_patched {
                info!("已修补 DA2: oppo security (loop)");
                patched = true;
            }
        }

        // DA2 独有：hash binding
        if let Some(idx) = Self::find_binary(data, b"\x01\x23\x03\x60\x00\x20\x70\x47\x70\xB5", 0) {
            data[idx..idx + 1].copy_from_slice(b"\x00");
            info!("已修补 DA2: hash binding");
            patched = true;
        }

        // DA2 独有：hash check
        if let Some(idx) = Self::find_binary(data, &0xC0070004u32.to_le_bytes(), 0) {
            data[idx..idx + 4].copy_from_slice(&0u32.to_le_bytes());
            info!("已修补 DA2: hash check (0xC0070004)");
            patched = true;
        } else if let Some(idx) = Self::find_binary(data, b"\x4F\xF0\x04\x09\xCC\xF2\x07\x09", 0) {
            data[idx..idx + 8].copy_from_slice(b"\x4F\xF0\x00\x09\x4F\xF0\x00\x09");
            info!("已修补 DA2: hash check (arm pattern)");
            patched = true;
        } else if let Some(idx) = Self::find_binary(
            data,
            b"\x4F\xF0\x04\x09\x32\x46\x01\x98\x03\x99\xCC\xF2\x07\x09",
            0,
        ) {
            data[idx..idx + 14]
                .copy_from_slice(b"\x4F\xF0\x00\x09\x32\x46\x01\x98\x03\x99\x4F\xF0\x00\x09");
            info!("已修补 DA2: hash check (arm pattern 2)");
            patched = true;
        } else {
            warn!("DA2 hash check 未找到");
        }

        // DA2 独有：security check
        if let Some(idx) = Self::find_binary(data, b"\x01\x23\x03\x60\x00\x20\x70\x47\x70\xB5", 0) {
            data[idx..idx + 2].copy_from_slice(b"\x00\x23");
            info!("已修补 DA2: security check");
            patched = true;
        }

        // DA2 独有：anti-rollback
        if let Some(idx) = Self::find_binary(data, &0xC0020053u32.to_le_bytes(), 0) {
            data[idx..idx + 4].copy_from_slice(&0u32.to_le_bytes());
            info!("已修补 DA2: DA version anti-rollback");
            patched = true;
        }

        // DA2 独有：SBC
        if let Some(idx) = Self::find_binary(data, b"\x02\x4B\x18\x68\xC0\xF3\x40\x00\x70\x47", 0) {
            data[idx + 4..idx + 8].copy_from_slice(b"\x4F\xF0\x00\x00");
            info!("已修补 DA2: SBC");
            patched = true;
        }

        // DA2 独有：register read/write
        if let Some(idx) = Self::find_binary(data, &0xC004000Du32.to_le_bytes(), 0) {
            data[idx..idx + 4].copy_from_slice(&0u32.to_le_bytes());
            info!("已修补 DA2: register read/write");
            patched = true;
        }

        // DA2 独有：write not allowed pattern 1
        let mut idx = 0;
        let mut write_patched = false;
        while let Some(pos) = data[idx..]
            .windows(8)
            .position(|w| w == b"\x37\xB5\x00\x23\x04\x46\x02\xA8")
        {
            let actual_idx = idx + pos;
            data[actual_idx..actual_idx + 8].copy_from_slice(b"\x37\xB5\x00\x20\x03\xB0\x30\xBD");
            write_patched = true;
            idx = actual_idx + 1;
        }
        if write_patched {
            info!("已修补 DA2: write not allowed (pattern 1)");
            patched = true;
        }

        // DA2 独有：write not allowed pattern 2
        if let Some(idx) = Self::find_binary(data, b"\x0C\x23\xCC\xF2\x02\x03", 0) {
            data[idx..idx + 6].copy_from_slice(b"\x00\x23\x00\x23\x00\x23");
            if let Some(idx2) = Self::find_binary(data, b"\x2A\x23\xCC\xF2\x02\x03", 0) {
                data[idx2..idx2 + 6].copy_from_slice(b"\x00\x23\x00\x23\x00\x23");
            }
            info!("已修补 DA2: write not allowed (pattern 2)");
            patched = true;
        }

        if !patched {
            warn!("DA2 无修补应用");
        }
    }

    /// 生成 DA extensions 二进制数据
    /// 对齐 Python xflash.py patch() 第 69-182 行
    pub(crate) fn generate_da_extensions(&self) -> Option<Vec<u8>> {
        let da2 = &self.da2_data;
        let da2address = self.da2_base_addr;

        // 复制模板
        let mut daextdata = DA_EXTENSIONS_TEMPLATE.to_vec();

        // 1. register_devctrl: \x38\xB5\x05\x46\x0C\x20
        let register_devctrl = Self::find_binary(da2, b"\x38\xB5\x05\x46\x0C\x20", 0);

        // 2. mmc_get_card: \x4B\x4F\xF4\x3C\x72 (找到后 -1)
        //    备选: \xA3\xEB\x00\x13\x18\x1A\x02\xEB\x00\x10 (找到后 -10)
        let mut mmc_get_card = Self::find_binary(da2, b"\x4B\x4F\xF4\x3C\x72", 0);
        if let Some(pos) = mmc_get_card {
            mmc_get_card = Some(pos.saturating_sub(1));
        } else {
            mmc_get_card = Self::find_binary(da2, b"\xA3\xEB\x00\x13\x18\x1A\x02\xEB\x00\x10", 0)
                .map(|p| p.saturating_sub(10));
        }

        // 3. mmc_set_part_config: 循环搜索 \xC3\x69\x0A\x46\x10\xB5，直到 +20 位置是 \xB3\x21
        //    备选: \xC3\x69\x13\xF0\x01\x03
        let mut mmc_set_part_config: Option<usize> = None;
        let mut pos = 0;
        while let Some(found) = Self::find_binary(da2, b"\xC3\x69\x0A\x46\x10\xB5", pos) {
            if found + 22 <= da2.len() && da2[found + 20] == 0xB3 && da2[found + 21] == 0x21 {
                mmc_set_part_config = Some(found);
                break;
            }
            pos = found + 1;
        }
        if mmc_set_part_config.is_none() {
            mmc_set_part_config = Self::find_binary(da2, b"\xC3\x69\x13\xF0\x01\x03", 0);
        }

        // 4. mmc_rpmb_send_command: \xF8\xB5\x06\x46\x9D\xF8\x18\x50
        //    备选: \x2D\xE9\xF0\x41\x4F\xF6\xFD\x74
        let mut mmc_rpmb_send_command =
            Self::find_binary(da2, b"\xF8\xB5\x06\x46\x9D\xF8\x18\x50", 0);
        if mmc_rpmb_send_command.is_none() {
            mmc_rpmb_send_command = Self::find_binary(da2, b"\x2D\xE9\xF0\x41\x4F\xF6\xFD\x74", 0);
        }

        // 5. g_ufs_hba: 三种备选 pattern，提取后面的 4 字节指针
        let mut g_ufs_hba: Option<u32> = None;
        let mut ptr_g_ufs_hba_found = false;

        // 备选 1: \x20\x46\x0B\xB0\xBD\xE8\xF0\x83\x00\xBF，+10 位置读 4 字节
        if let Some(p) = Self::find_binary(da2, b"\x20\x46\x0B\xB0\xBD\xE8\xF0\x83\x00\xBF", 0)
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
            && let Some(p) = Self::find_binary(da2, b"\x20\x46\x0D\xB0\xBD\xE8\xF0\x83", 0)
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
            && let Some(p) = Self::find_binary(da2, b"\x21\x46\x02\xF0\x02\xFB\x1B\xE6\x00\xBF", 0)
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
            Self::find_binary_wildcard(da2, &[0xB5, 0x00, 0xB1, 0x90, 0xF8], 1)
        } else {
            None
        };
        let ufshcd_queuecommand = if ptr_g_ufs_hba_found {
            Self::find_binary(da2, b"\x2D\xE9\xF8\x43\x01\x27", 0)
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

        debug!("DA Extensions 生成成功:");
        debug!("  register_devctrl 地址: 0x{:08X}", register_devctrl_addr);
        debug!("  mmc_get_card 地址: 0x{:08X}", mmc_get_card_addr);
        debug!(
            "  mmc_set_part_config 地址: 0x{:08X}",
            mmc_set_part_config_addr
        );
        debug!(
            "  mmc_rpmb_send_command 地址: 0x{:08X}",
            mmc_rpmb_send_command_addr
        );

        Some(daextdata)
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

    /// 带通配符的 find_binary（支持 . 通配符，公开版本）
    #[allow(dead_code)]
    pub fn find_binary_with_wildcard(
        data: &[u8],
        pattern: &[u8],
        start_pos: usize,
    ) -> Option<usize> {
        Self::find_binary(data, pattern, start_pos)
    }

    /// CUSTOM_READMEM（0x0F0001）通过 DA2 读物理内存
    /// 对齐 Python xflash.py: custom_read(addr, length)
    /// 协议: cmd(CUSTOM_READMEM) → xsend(addr64) → xsend(sz32) → xread() → status()
    #[allow(dead_code)]
    pub fn custom_readmem(&mut self, addr: u64, length: usize) -> Result<Vec<u8>, String> {
        const MAX_CHUNK: usize = 0x10000;
        let mut data = Vec::with_capacity(length);
        let mut pos: usize = 0;

        while pos < length {
            // 1. cmd(CUSTOM_READMEM) = DEVICE_CTRL → status → 0x0F0001 → status
            let devctrl_pkt = pack3(CMD_MAGIC, 0x01, 4);
            self.preloader.device.write(&devctrl_pkt)?;
            self.preloader.device.write(&0x010009u32.to_le_bytes())?;
            let st1 = self.status()?;
            if st1 != 0 {
                return Err(format!("custom_readmem DEVICE_CTRL status: 0x{:08X}", st1));
            }

            let cmd_pkt = pack3(CMD_MAGIC, 0x01, 4);
            self.preloader.device.write(&cmd_pkt)?;
            self.preloader.device.write(&0x0F0001u32.to_le_bytes())?;
            let st2 = self.status()?;
            if st2 != 0 {
                return Err(format!(
                    "custom_readmem CUSTOM_READMEM status: 0x{:08X}",
                    st2
                ));
            }

            // 2. xsend(addr, 8 bytes, is64bit=True)
            let chunk_addr = addr + pos as u64;
            let addr_pkt = pack3(CMD_MAGIC, 0x01, 8);
            self.preloader.device.write(&addr_pkt)?;
            self.preloader.device.write(&chunk_addr.to_le_bytes())?;

            // 3. xsend(sz, 4 bytes)
            let sz = std::cmp::min(length - pos, MAX_CHUNK);
            let sz_pkt = pack3(CMD_MAGIC, 0x01, 4);
            self.preloader.device.write(&sz_pkt)?;
            self.preloader.device.write(&(sz as u32).to_le_bytes())?;

            // 4. xread() → 读回数据
            let chunk = self.xread_data()?;
            data.extend_from_slice(&chunk);
            pos += chunk.len();

            // 5. status()
            let st3 = self.status()?;
            if st3 != 0 {
                debug!("custom_readmem chunk status: 0x{:08X}", st3);
                break;
            }
        }

        if data.len() > length {
            data.truncate(length);
        }
        Ok(data)
    }

    /// 设置 meta boot 模式
    /// 对齐 Python xflash_lib.py set_meta()
    pub fn set_meta(&mut self, mode: &str) -> Result<(), String> {
        let (boot_mode, com_type, com_id) = match mode {
            "usb" => (0x01u8, 0x02u8, 0x00u8),
            "off" => (0x00u8, 0x00u8, 0x00u8),
            _ => return Err(format!("Unknown meta mode: {}", mode)),
        };
        let data = vec![boot_mode, com_type, com_id];
        self.send_devctrl(SET_META_BOOT_MODE, Some(&data))?;
        info!("Meta boot 模式已设置为: {}", mode);
        Ok(())
    }

    /// 在 DA 模式下开启 ADB，然后重启进系统
    pub fn enable_adb_and_reboot(&mut self) -> Result<(), String> {
        info!("在 DA 模式下开启 ADB...");
        self.set_meta("usb")?;
        info!("重启设备进入系统...");
        self.boot_to(0x4FFF0000, &[], false, 0.0)?;
        Ok(())
    }
}

use crate::color::Colorize;
use log::info;

use crate::da::DAXFlash;

pub fn vbmeta_disable(da: &mut DAXFlash, mode: u32) -> Result<(), String> {
    info!("开始 vbmeta 禁用 (mode={})...", mode);

    // 尝试读取当前 A/B 槽位，优先修补当前槽位的 vbmeta
    let slot = detect_active_slot(da);
    if let Ok(Some(ref s)) = slot {
        info!("检测到当前 A/B 槽位: {}，优先修补对应槽位的 vbmeta", s);
    }

    // 扩展候选分区列表，对齐 C# 版支持的 vbmeta 变体
    let candidates = [
        "vbmeta_a",
        "vbmeta_b",
        "vbmeta",
        "vbmeta_system_a",
        "vbmeta_system_b",
        "vbmeta_vendor_a",
        "vbmeta_vendor_b",
        "vbmeta_product_a",
        "vbmeta_product_b",
        "vbmeta_system_ext_a",
        "vbmeta_system_ext_b",
        "vbmeta_odm_a",
        "vbmeta_odm_b",
    ];
    let mut patched_any = false;

    for name in &candidates {
        // 如果能确定槽位，跳过非当前槽位的分区
        if let Ok(Some(ref s)) = slot {
            if name.ends_with("_a") && s != "a" {
                continue;
            }
            if name.ends_with("_b") && s != "b" {
                continue;
            }
            // 无后缀的分区始终尝试（非 A/B 设备）
        }

        if let Ok((addr, size)) = da.find_partition_addr(name) {
            info!(
                "  找到 vbmeta 分区: {} @ 0x{:X}, 大小: 0x{:X}",
                name, addr, size
            );

            let data = da.readflash_data(addr, size)?;
            info!("  读取到 {} 字节", data.len());

            let modified = patch_vbmeta_data(&data, mode)?;
            da.write_flash_data(addr, &modified, 1, 8)?;
            info!("{}", format!("  {} 已修补", name).green());
            patched_any = true;
        }
    }

    if !patched_any {
        return Err("未找到 vbmeta 相关分区".to_string());
    }

    info!("{}", "vbmeta 禁用成功".green());
    Ok(())
}

fn patch_vbmeta_data(data: &[u8], mode: u32) -> Result<Vec<u8>, String> {
    // AVB vbmeta header 完整大小 = 0x80 + 48(release_string) = 0xB0
    // 我们只修改 flags 字段(0x78)，最少需要 0x7C 字节
    // 使用 0x80 留 4 字节余量
    if data.len() < 0x80 {
        return Err("vbmeta 数据太小".to_string());
    }

    if &data[0..4] != b"AVB0" {
        return Err("非 vbmeta 格式 (缺少 AVB0 签名)".to_string());
    }

    let mut result = data.to_vec();

    // flags 位于偏移 0x78（u32），紧接 rollback_index(0x70, u64) 之后
    //
    // AVB flags 位定义 (avb_vbmeta_image.h):
    //   bit 0 (0x01) = AVB_VBMETA_IMAGE_FLAGS_HASHTREE_DISABLED
    //   bit 1 (0x02) = AVB_VBMETA_IMAGE_FLAGS_VERIFICATION_DISABLED
    //
    // mode 作为 flags 直接写入：
    //   0 = 0x00: 全部启用
    //   1 = 0x01: hashtree 校验禁用（dm-verity off），签名验证保留
    //   2 = 0x02: 签名验证禁用，hashtree 校验保留
    //   3 = 0x03: 全部禁用（推荐刷机场景）
    //
    // ★ AVB header 中所有字段均为大端序（network byte order）
    let flags_offset = 0x78;
    let old_flags = u32::from_be_bytes(result[flags_offset..flags_offset + 4].try_into().unwrap());
    result[flags_offset..flags_offset + 4].copy_from_slice(&mode.to_be_bytes());

    info!(
        "  vbmeta: flags @0x78 = 0x{:08X} -> 0x{:08X}",
        old_flags, mode
    );

    // algorithm_type(0x1C) 和 hash_offset(0x20) 不动

    Ok(result)
}

/// 检测当前活跃的 A/B 槽位（通过读取 misc 分区的 bootctrl 结构）
fn detect_active_slot(da: &mut DAXFlash) -> Result<Option<String>, String> {
    let (misc_addr, misc_size) = match da.find_partition_addr("misc") {
        Ok((addr, size)) => (addr, size.min(0x10000)), // 最多读 64KB
        Err(_) => return Ok(None),
    };

    let misc_data = match da.readflash_data(misc_addr, misc_size) {
        Ok(data) => data,
        Err(_) => return Ok(None),
    };

    if misc_data.len() < 0x2010 {
        return Ok(None);
    }

    // 检查 bootctrl 结构（偏移 0x2000）
    let bootctrl_offset = 0x2000;
    let magic = u32::from_le_bytes([
        misc_data[bootctrl_offset],
        misc_data[bootctrl_offset + 1],
        misc_data[bootctrl_offset + 2],
        misc_data[bootctrl_offset + 3],
    ]);

    if magic == 0x424F4F54 {
        // "BOOT"
        let active_slot = u32::from_le_bytes([
            misc_data[bootctrl_offset + 8],
            misc_data[bootctrl_offset + 9],
            misc_data[bootctrl_offset + 10],
            misc_data[bootctrl_offset + 11],
        ]);
        let slot = if active_slot == 0 { "a" } else { "b" };
        return Ok(Some(slot.to_string()));
    }

    // fallback: 在数据中搜索槽位标记
    let data_str = String::from_utf8_lossy(&misc_data);
    if data_str.contains("_a") && !data_str.contains("_b") {
        return Ok(Some("a".to_string()));
    } else if data_str.contains("_b") && !data_str.contains("_a") {
        return Ok(Some("b".to_string()));
    }

    Ok(None)
}

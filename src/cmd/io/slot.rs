use crate::color::Colorize;
use crate::da::DAXFlash;
use log::{info, warn};

/// A/B slot management
/// slot show — show the current slot
/// slot a    — switch to slot A
/// slot b    — switch to slot B
pub fn cmd_slot(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient slot show/a/b".into());
    }

    let action = args[0].as_str();
    match action {
        "show" => show_slot(da)?,
        "a" => set_slot(da, 'a')?,
        "b" => set_slot(da, 'b')?,
        _ => {
            return Err(
                format!("用法: mtkclient slot show|a|b（未知槽位操作: {}）", action).into(),
            );
        }
    }

    Ok(())
}

/// Read and display the current A/B slot
fn show_slot(da: &mut DAXFlash) -> Result<(), String> {
    // Try to read slot info from the misc partition
    let slot = read_slot_from_misc(da)?;
    match slot {
        Some(s) => {
            info!("{}", format!("当前 A/B 槽位: {}", s).green().bold());
        }
        None => {
            info!("未检测到 A/B 槽位信息（设备可能不支持 A/B 分区）");
        }
    }
    Ok(())
}

/// Set the A/B slot
fn set_slot(da: &mut DAXFlash, slot: char) -> Result<(), String> {
    info!("设置 A/B 槽位为 {}...", slot);

    // Read the misc partition into memory
    let (misc_addr, misc_size) = da
        .find_partition_addr("misc")
        .map_err(|e| format!("查找 misc 分区失败: {}", e))?;

    let mut new_misc = da
        .readflash_data(misc_addr, misc_size)
        .map_err(|e| format!("读取 misc 分区失败: {}", e))?;

    // Check the bootctrl struct first (offset 0x2000)
    let bootctrl_offset = 0x2000;
    if new_misc.len() >= bootctrl_offset + 12 {
        let magic = u32::from_le_bytes([
            new_misc[bootctrl_offset],
            new_misc[bootctrl_offset + 1],
            new_misc[bootctrl_offset + 2],
            new_misc[bootctrl_offset + 3],
        ]);
        if magic == 0x424F4F54 {
            // bootctrl struct exists; update active_slot in place
            let new_slot_val = if slot == 'a' { 0u32 } else { 1u32 };
            new_misc[bootctrl_offset + 8..bootctrl_offset + 12]
                .copy_from_slice(&new_slot_val.to_le_bytes());
            info!("bootctrl 结构已更新 (active_slot={})", new_slot_val);
        } else {
            // No bootctrl struct; initialize one
            warn!("misc 分区中无 bootctrl 结构，创建新的...");
            new_misc[bootctrl_offset..bootctrl_offset + 4]
                .copy_from_slice(&0x424F4F54u32.to_le_bytes()); // "BOOT"
            new_misc[bootctrl_offset + 4..bootctrl_offset + 8].copy_from_slice(&1u32.to_le_bytes()); // version
            new_misc[bootctrl_offset + 8..bootctrl_offset + 12]
                .copy_from_slice(&(if slot == 'a' { 0u32 } else { 1u32 }).to_le_bytes()); // active_slot
        }
    }

    // Write the buffer straight to the misc partition
    write_partition_from_memory(da, "misc", &new_misc)?;

    // Cache invalidation: `da.active_slot` caches "read misc once per session".
    // The slot was just rewritten, so it must be refreshed here; otherwise later
    // vbmeta / r boot / partition-name resolution still uses the old slot
    // (classic symptom: zyb vbmeta right after switching edits the previous slot).
    da.active_slot = Some(slot);
    log::debug!("active_slot 缓存已更新为 {:?}", da.active_slot);

    info!("{}", format!("A/B 槽位已设置为 {}", slot).green().bold());
    Ok(())
}

/// Read slot info from the misc partition
fn read_slot_from_misc(da: &mut DAXFlash) -> Result<Option<String>, String> {
    let (misc_addr, misc_size) = match da.find_partition_addr("misc") {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let misc_data = match da.readflash_data(misc_addr, misc_size) {
        Ok(data) => data,
        Err(_) => return Ok(None),
    };

    if misc_data.len() < 0x2010 {
        return Ok(None);
    }

    // Check the bootctrl struct (offset 0x2000)
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
        let slot = if active_slot == 0 {
            "a".to_string()
        } else {
            "b".to_string()
        };
        return Ok(Some(slot));
    }

    // Fall back to searching for slot markers in the data
    let data_str = String::from_utf8_lossy(&misc_data);
    if data_str.contains("_a") && !data_str.contains("_b") {
        return Ok(Some("a".to_string()));
    } else if data_str.contains("_b") && !data_str.contains("_a") {
        return Ok(Some("b".to_string()));
    }

    Ok(None)
}

/// Write partition data directly from memory (no temp file).
///
/// Writes `data` to the given partition, resolving its address and handling 512-byte alignment.
pub(crate) fn write_partition_from_memory(
    da: &mut DAXFlash,
    part_name: &str,
    data: &[u8],
) -> Result<(), String> {
    let (addr, _size) = da
        .find_partition_addr(part_name)
        .map_err(|e| format!("查找分区 {} 失败: {}", part_name, e))?;

    da.write_flash_data(addr, data, 1, 8)
        .map_err(|e| format!("写入分区 {} 失败: {}", part_name, e))?;
    Ok(())
}

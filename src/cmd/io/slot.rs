use crate::color::Colorize;
use crate::da::DAXFlash;
use log::{info, warn};

/// A/B 槽位管理
/// slot show — 显示当前槽位
/// slot a    — 切换到 A 槽
/// slot b    — 切换到 B 槽
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

/// 读取并显示当前 A/B 槽位
fn show_slot(da: &mut DAXFlash) -> Result<(), String> {
    // 尝试从 misc 分区读取槽位信息
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

/// 设置 A/B 槽位
fn set_slot(da: &mut DAXFlash, slot: char) -> Result<(), String> {
    info!("设置 A/B 槽位为 {}...", slot);

    // 读取 misc 分区到内存
    let (misc_addr, misc_size) = da
        .find_partition_addr("misc")
        .map_err(|e| format!("查找 misc 分区失败: {}", e))?;

    let mut new_misc = da
        .readflash_data(misc_addr, misc_size)
        .map_err(|e| format!("读取 misc 分区失败: {}", e))?;

    // 优先检查 bootctrl 结构（偏移 0x2000）
    let bootctrl_offset = 0x2000;
    if new_misc.len() >= bootctrl_offset + 12 {
        let magic = u32::from_le_bytes([
            new_misc[bootctrl_offset],
            new_misc[bootctrl_offset + 1],
            new_misc[bootctrl_offset + 2],
            new_misc[bootctrl_offset + 3],
        ]);
        if magic == 0x424F4F54 {
            // 已有 bootctrl 结构，直接修改 active_slot
            let new_slot_val = if slot == 'a' { 0u32 } else { 1u32 };
            new_misc[bootctrl_offset + 8..bootctrl_offset + 12]
                .copy_from_slice(&new_slot_val.to_le_bytes());
            info!("bootctrl 结构已更新 (active_slot={})", new_slot_val);
        } else {
            // 没有 bootctrl 结构，初始化一个
            warn!("misc 分区中无 bootctrl 结构，创建新的...");
            new_misc[bootctrl_offset..bootctrl_offset + 4]
                .copy_from_slice(&0x424F4F54u32.to_le_bytes()); // "BOOT"
            new_misc[bootctrl_offset + 4..bootctrl_offset + 8].copy_from_slice(&1u32.to_le_bytes()); // version
            new_misc[bootctrl_offset + 8..bootctrl_offset + 12]
                .copy_from_slice(&(if slot == 'a' { 0u32 } else { 1u32 }).to_le_bytes()); // active_slot
        }
    }

    // 直接从内存写入 misc 分区
    write_partition_from_memory(da, "misc", &new_misc)?;

    info!("{}", format!("A/B 槽位已设置为 {}", slot).green().bold());
    Ok(())
}

/// 从 misc 分区读取槽位信息
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
        let slot = if active_slot == 0 {
            "a".to_string()
        } else {
            "b".to_string()
        };
        return Ok(Some(slot));
    }

    // 尝试在数据中搜索槽位标记
    let data_str = String::from_utf8_lossy(&misc_data);
    if data_str.contains("_a") && !data_str.contains("_b") {
        return Ok(Some("a".to_string()));
    } else if data_str.contains("_b") && !data_str.contains("_a") {
        return Ok(Some("b".to_string()));
    }

    Ok(None)
}

/// 从内存直接写入分区数据（无需临时文件）
///
/// 将 data 写入到指定分区，自动查找分区地址并处理 512 字节对齐
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

use log::info;

use crate::da::DAXFlash;
use crate::system::paths::get_tmp_path;

/// FRP OEM unlock: modify the OEM unlock flag in the persistent data block of the frp partition
/// The OEM unlock flag is the last byte of the frp partition: 0 = locked (factory default), 1 = unlocked
/// Also clears FRP account-lock data (bytes around keywords such as "FactoryResetProtection")
pub fn frp_unlock(da: &mut DAXFlash) -> Result<(), String> {
    info!("FRP OEM 解锁（修改标志位方式）...");

    let backup_path = get_tmp_path("frp_backup.bin");
    let unlocked_path = get_tmp_path("frp_unlocked.bin");

    // 1. Read the frp partition
    da.read_partition("frp", &backup_path.to_string_lossy())?;
    let mut frp_data = std::fs::read(&backup_path).map_err(|e| format!("读取备份失败: {}", e))?;
    info!("frp 分区: {} 字节", frp_data.len());

    // 2. Set the OEM unlock flag (last byte of the partition → 1 = unlocked)
    if !frp_data.is_empty() {
        let last_idx = frp_data.len() - 1;
        let old_flag = frp_data[last_idx];
        if old_flag != 1 {
            frp_data[last_idx] = 1;
            info!(
                "  OEM unlock 标志位: 0x{:02X} -> 0x01 (分区末尾 offset 0x{:X})",
                old_flag, last_idx
            );
        } else {
            info!("  OEM unlock 标志位已经是 0x01（已解锁）");
        }
    }

    // 3. Clear FRP account-lock data
    patch_frp_data(&mut frp_data);

    // 4. Write back
    std::fs::write(&unlocked_path, &frp_data).map_err(|e| format!("写入临时文件失败: {}", e))?;
    da.write_partition("frp", &unlocked_path.to_string_lossy())?;

    info!("FRP OEM 解锁完成");
    Ok(())
}

/// FRP OEM lock: restore the OEM unlock flag of the frp partition to the locked state
pub fn frp_lock(da: &mut DAXFlash) -> Result<(), String> {
    info!("FRP OEM 锁定（修改标志位方式）...");

    let backup_path = get_tmp_path("frp_backup.bin");
    let locked_path = get_tmp_path("frp_locked.bin");

    // 1. Read the frp partition
    da.read_partition("frp", &backup_path.to_string_lossy())?;
    let mut frp_data = std::fs::read(&backup_path).map_err(|e| format!("读取备份失败: {}", e))?;
    info!("frp 分区: {} 字节", frp_data.len());

    // 2. Reset the OEM unlock flag (last byte of the partition → 0 = locked)
    if !frp_data.is_empty() {
        let last_idx = frp_data.len() - 1;
        let old_flag = frp_data[last_idx];
        if old_flag != 0 {
            frp_data[last_idx] = 0;
            info!(
                "  OEM unlock 标志位: 0x{:02X} -> 0x00 (分区末尾 offset 0x{:X})",
                old_flag, last_idx
            );
        } else {
            info!("  OEM unlock 标志位已经是 0x00（已锁定）");
        }
    }

    // 3. Write back
    std::fs::write(&locked_path, &frp_data).map_err(|e| format!("写入临时文件失败: {}", e))?;
    da.write_partition("frp", &locked_path.to_string_lossy())?;

    info!("FRP OEM 锁定完成");
    Ok(())
}

/// Clear FRP account-lock data (in place)
fn patch_frp_data(data: &mut [u8]) {
    // Method 1: "FRP\0" magic followed by a status flag at offset 0x04
    if data.len() >= 5 && &data[0..4] == b"FRP\0" && data[4] == 0x01 {
        data[4] = 0x00;
        info!("  清除 FRP 状态标志: 0x01 -> 0x00");
    }

    // Method 2: look for the "FactoryResetProtection" string
    if let Some(idx) = data
        .windows(22)
        .position(|w| w == b"FactoryResetProtection")
    {
        let flag_pos = idx + 22;
        if flag_pos < data.len() && data[flag_pos] != 0x00 {
            data[flag_pos] = 0x00;
            info!("  清除 FactoryResetProtection 状态标志");
        }
    }

    // Method 3: find and clear Google account related data
    let patterns: &[&[u8]] = &[b"google", b"FRP", b"android_id", b"device_policy"];
    for pattern in patterns {
        if let Some(idx) = data.windows(pattern.len()).position(|w| w == *pattern) {
            let start = idx.saturating_sub(16);
            let end = (idx + pattern.len() + 32).min(data.len());
            for b in &mut data[start..end] {
                *b = 0;
            }
            info!(
                "  清除 {} 相关数据 @ offset 0x{:X}",
                String::from_utf8_lossy(pattern),
                start
            );
        }
    }
}

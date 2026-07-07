use log::info;

use crate::da::DAXFlash;

/// FRP OEM 解锁：修改 frp 分区中 persistent 数据块的 OEM unlock 标志位
/// OEM unlock 标志位于 frp 分区的最后一个字节：0=锁定(出厂默认), 1=解锁
/// 同时清除 FRP 账户锁数据（"FactoryResetProtection" 等关键字附近的数据）
pub fn frp_unlock(da: &mut DAXFlash) -> Result<(), String> {
    info!("FRP OEM 解锁（修改标志位方式）...");

    // 1. 读取 frp 分区
    da.读取分区("frp", "frp_backup.bin")?;
    let mut frp_data = std::fs::read("frp_backup.bin")
        .map_err(|e| format!("读取备份失败: {}", e))?;
    info!("frp 分区: {} 字节", frp_data.len());

    // 2. 修改 OEM unlock 标志位（分区最后一个字节 → 1 = 解锁）
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

    // 3. 清除 FRP 账户锁数据
    patch_frp_data(&mut frp_data);

    // 4. 写回
    std::fs::write("frp_unlocked.bin", &frp_data)
        .map_err(|e| format!("写入临时文件失败: {}", e))?;
    da.写入分区("frp", "frp_unlocked.bin")?;

    info!("FRP OEM 解锁完成");
    Ok(())
}

/// FRP OEM 锁定：恢复 frp 分区的 OEM unlock 标志位为锁定状态
pub fn frp_lock(da: &mut DAXFlash) -> Result<(), String> {
    info!("FRP OEM 锁定（修改标志位方式）...");

    // 1. 读取 frp 分区
    da.读取分区("frp", "frp_backup.bin")?;
    let mut frp_data = std::fs::read("frp_backup.bin")
        .map_err(|e| format!("读取备份失败: {}", e))?;
    info!("frp 分区: {} 字节", frp_data.len());

    // 2. 修改 OEM unlock 标志位（分区最后一个字节 → 0 = 锁定）
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

    // 3. 写回
    std::fs::write("frp_locked.bin", &frp_data)
        .map_err(|e| format!("写入临时文件失败: {}", e))?;
    da.写入分区("frp", "frp_locked.bin")?;

    info!("FRP OEM 锁定完成");
    Ok(())
}

fn find_frp_partition(da: &mut DAXFlash) -> Result<String, String> {
    let candidates = [
        "frp",
        "persistent",
        "config",
        "nvram",
        "protect1",
        "protect2",
    ];
    for name in &candidates {
        if da.find_partition_addr(name).is_ok() {
            return Ok(name.to_string());
        }
    }
    Err("未找到 FRP 相关分区 (frp/persistent/config/nvram)".to_string())
}

/// 清除 FRP 账户锁数据（原地修改）
fn patch_frp_data(data: &mut [u8]) {
    // 方法 1: "FRP\0" 魔数 + offset 0x04 状态标志
    if data.len() >= 5 && &data[0..4] == b"FRP\0" && data[4] == 0x01 {
        data[4] = 0x00;
        info!("  清除 FRP 状态标志: 0x01 -> 0x00");
    }

    // 方法 2: 查找 "FactoryResetProtection" 字符串
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

    // 方法 3: 查找 Google 账号相关数据并清除
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

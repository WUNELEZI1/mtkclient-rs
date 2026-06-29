use colored::Colorize;
use log::info;

use crate::DA扩展::DAXFlash;

pub fn frp_unlock(da: &mut DAXFlash) -> Result<(), String> {
    info!("开始 FRP OEM 解锁...");

    let frp_part = find_frp_partition(da)?;
    let (addr, size) = da.find_partition_addr(&frp_part)?;

    info!(
        "  找到 FRP 分区: {} @ 0x{:X}, 大小: 0x{:X}",
        frp_part, addr, size
    );

    let data = da.readflash_data(addr, size)?;
    info!("  读取到 {} 字节", data.len());

    let modified = patch_frp_data(&data)?;

    da.write_flash_data(addr, &modified, 1, 8)?;
    info!("{}", "FRP OEM 解锁成功".green());
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

fn patch_frp_data(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut result = data.to_vec();

    // FRP 数据结构分析:
    // 在 Android 6-10 中, FRP 数据通常位于分区开头
    // 关键标志位:
    //   - "FactoryResetProtection" 字符串后的状态标志
    //   - offset 0x00: 'F' 'R' 'P' 标志
    //   - offset 0x04: state (0x01=enabled, 0x00=disabled)

    // 方法 1: 查找并修改 state 标志
    if data.len() >= 5 && &data[0..4] == b"FRP\0" && result[4] == 0x01 {
        result[4] = 0x00;
        info!("  修改 FRP 状态标志: 0x01 -> 0x00");
        return Ok(result);
    }

    // 方法 2: 查找 "FactoryResetProtection" 字符串
    if let Some(idx) = data
        .windows(22)
        .position(|w| w == b"FactoryResetProtection")
    {
        let flag_pos = idx + 22;
        if flag_pos < result.len() && result[flag_pos] != 0x00 {
            result[flag_pos] = 0x00;
            info!("  修改 FactoryResetProtection 状态标志");
        }
        return Ok(result);
    }

    // 方法 3: 查找 Google 账号相关数据并清除
    // 通常包含 "google" 或 "FRP" 关键字
    let patterns: &[&[u8]] = &[b"google", b"FRP", b"android_id", b"device_policy"];
    for pattern in patterns {
        if let Some(idx) = data.windows(pattern.len()).position(|w| w == *pattern) {
            // 将匹配位置附近的数据清零
            let start = idx.saturating_sub(16);
            let end = (idx + pattern.len() + 32).min(result.len());
            for b in &mut result[start..end] {
                *b = 0;
            }
            info!(
                "  清除 {} 相关数据 @ offset 0x{:X}",
                String::from_utf8_lossy(pattern),
                start
            );
        }
    }

    // 方法 4: 暴力清除前 512 字节中的非零数据
    // 这是最激进的方法, 适用于未知 FRP 格式
    let limit = 512.min(result.len());
    let mut cleared = 0;
    for b in &mut result[..limit] {
        if *b != 0 {
            *b = 0;
            cleared += 1;
        }
    }
    if cleared > 0 {
        info!("  清除前 512 字节中 {} 个非零字节", cleared);
    }

    Ok(result)
}

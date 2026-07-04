//! 设备 IO 命令
//!
//! - `cmd_read`     — 读取分区数据到文件
//! - `cmd_write`    — 写入文件到分区（支持 verify）
//! - `cmd_erase`    — 擦除分区
//! - `cmd_reboot`   — 重启设备（支持 system/fastboot/recovery/fastbootd）
//! - `cmd_slot`     — 显示/切换 A/B 槽位

use colored::Colorize;
use log::{info, warn};

use crate::da::DAXFlash;

/// 读取分区数据到文件
pub fn cmd_read(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err("用法: mtkclient r <part> <file>".into());
    }
    da.读取分区(&args[0], &args[1])
        .map_err(|e| format!("读取失败: {}", e))?;
    info!("{}", format!("{} -> {}", args[0], args[1]).green());
    Ok(())
}

/// 写入文件到分区
pub fn cmd_write(
    da: &mut DAXFlash,
    args: &[String],
    verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err("用法: mtkclient w <part> <file>".into());
    }
    let result = if verify {
        da.写入分区带校验(&args[0], &args[1])
    } else {
        da.写入分区(&args[0], &args[1])
    };
    result.map_err(|e| format!("写入失败: {}", e))?;
    info!("{}", format!("{} <- {}", args[0], args[1]).green());
    Ok(())
}

/// 擦除分区
pub fn cmd_erase(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient e <part>".into());
    }
    da.擦除分区(&args[0])
        .map_err(|e| format!("擦除失败: {}", e))?;
    info!("{}", format!("{} 已擦除", args[0]).green());
    Ok(())
}

/// 重启设备
/// 支持模式: system(默认), fastboot, recovery, fastbootd
pub fn cmd_reboot(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mode = args.first().map(|s| s.as_str()).unwrap_or("system");

    match mode {
        "system" => {
            // 正常重启：直接发送 reset 命令
            do_reboot(da, "system")?;
        }
        "fastboot" | "recovery" | "fastbootd" => {
            // 重启到指定模式：修改 misc 分区的 bootloader_message，然后正常重启
            info!("准备重启到 {} 模式...", mode);
            set_bootloader_message(da, mode)?;
            do_reboot(da, mode)?;
        }
        _ => {
            return Err(format!(
                "未知重启模式: {}。支持: system, fastboot, recovery, fastbootd",
                mode
            )
            .into());
        }
    }

    Ok(())
}

/// 执行实际重启
fn do_reboot(da: &mut DAXFlash, mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    match da.reset_device() {
        Ok(()) => {
            info!("{}", format!("设备已重启到 {} 模式", mode).green());
            crate::connection::reset_session();
            Ok(())
        }
        Err(e) => {
            warn!("DA 重启失败，回退到 BROM jump_bl: {}", e);
            da.close_device(true);
            crate::connection::reset_session();
            info!("{}", format!("设备已重启到 {} 模式", mode).green());
            Ok(())
        }
    }
}

/// 设置 misc 分区的 bootloader_message 以重启到指定模式
fn set_bootloader_message(da: &mut DAXFlash, mode: &str) -> Result<(), String> {
    // bootloader_message 结构（存储在 misc 分区起始处）：
    // char command[32];      // "boot-recovery", "boot-fastboot" 等
    // char status[32];       // 状态
    // char recovery[1024];   // recovery 命令
    // char stage[32];        // 阶段信息
    // char reserved[1184];   // 保留

    let mut msg = vec![0u8; 32 + 32 + 1024]; // 只修改前 1088 字节

    let cmd_str = match mode {
        "fastboot" => "boot-fastboot",
        "recovery" => "boot-recovery",
        "fastbootd" => "boot-fastboot", // fastbootd 也使用 boot-fastboot
        _ => "",
    };

    if !cmd_str.is_empty() {
        let cmd_bytes = cmd_str.as_bytes();
        msg[..cmd_bytes.len()].copy_from_slice(cmd_bytes);
        info!("已设置 bootloader_message command='{}'", cmd_str);
    }

    // 读取 misc 分区，修改前 1088 字节，写回
    let misc_data = match da.读取分区("misc", "misc_tmp_read.bin") {
        Ok(_) => std::fs::read("misc_tmp_read.bin").unwrap_or_else(|_| vec![0u8; 0x1000]),
        Err(e) => {
            warn!("读取 misc 分区失败 ({})，尝试直接写入", e);
            vec![0u8; 0x1000] // 4KB 默认值
        }
    };

    let mut new_misc = misc_data;
    if new_misc.len() < msg.len() {
        new_misc.resize(msg.len(), 0);
    }
    new_misc[..msg.len()].copy_from_slice(&msg);

    // 写回 misc 分区
    std::fs::write("misc_reboot_tmp.bin", &new_misc).map_err(|e| e.to_string())?;
    da.写入分区("misc", "misc_reboot_tmp.bin")
        .map_err(|e| format!("写入 misc 分区失败: {}", e))?;

    info!("misc 分区已更新");
    Ok(())
}

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
        _ => return Err(format!("未知槽位操作: {}。支持: show, a, b", action).into()),
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

    // 读取 misc 分区
    let misc_data = match da.读取分区("misc", "misc_tmp_read2.bin") {
        Ok(_) => std::fs::read("misc_tmp_read2.bin").map_err(|e| e.to_string())?,
        Err(e) => return Err(format!("读取 misc 分区失败: {}", e)),
    };

    let mut new_misc = misc_data.clone();

    // 尝试在 misc 分区中查找并修改槽位标记
    // A/B 槽位信息通常存储在 misc 分区的 bootctrl 区域（偏移 0x2000 或 0x4000）
    // 或者在 bootloader_message 的保留区域中
    // 这里尝试查找已知的槽位标记模式
    let slot_marker_a = b"_a";
    let slot_marker_b = b"_b";

    // 简单策略：查找并替换槽位标记
    let modified = if slot == 'a' {
        replace_slot_markers(&mut new_misc, slot_marker_b, slot_marker_a)
    } else {
        replace_slot_markers(&mut new_misc, slot_marker_a, slot_marker_b)
    };

    if modified == 0 {
        warn!("未在 misc 分区中找到槽位标记，尝试写入 bootctrl 结构...");
        // 如果找不到标记，尝试在偏移 0x2000 处写入槽位信息
        if new_misc.len() < 0x2010 {
            new_misc.resize(0x2010, 0);
        }
        // bootctrl 结构：magic(4) + version(4) + active_slot(4) + ...
        let bootctrl_offset = 0x2000;
        new_misc[bootctrl_offset..bootctrl_offset + 4]
            .copy_from_slice(&0x424F4F54u32.to_le_bytes()); // "BOOT"
        new_misc[bootctrl_offset + 4..bootctrl_offset + 8].copy_from_slice(&1u32.to_le_bytes()); // version
        new_misc[bootctrl_offset + 8..bootctrl_offset + 12]
            .copy_from_slice(&(if slot == 'a' { 0u32 } else { 1u32 }).to_le_bytes()); // active_slot
    }

    // 写回 misc 分区
    std::fs::write("misc_slot_tmp.bin", &new_misc).map_err(|e| e.to_string())?;
    da.写入分区("misc", "misc_slot_tmp.bin")
        .map_err(|e| format!("写入 misc 分区失败: {}", e))?;

    info!("{}", format!("A/B 槽位已设置为 {}", slot).green().bold());
    Ok(())
}

/// 从 misc 分区读取槽位信息
fn read_slot_from_misc(da: &mut DAXFlash) -> Result<Option<String>, String> {
    let misc_data = match da.读取分区("misc", "misc_tmp_read3.bin") {
        Ok(_) => match std::fs::read("misc_tmp_read3.bin") {
            Ok(data) => data,
            Err(_) => return Ok(None),
        },
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

/// 在字节数组中替换槽位标记
fn replace_slot_markers(data: &mut [u8], from: &[u8], to: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i + from.len() <= data.len() {
        if &data[i..i + from.len()] == from {
            data[i..i + to.len()].copy_from_slice(to);
            count += 1;
        }
        i += 1;
    }
    count
}

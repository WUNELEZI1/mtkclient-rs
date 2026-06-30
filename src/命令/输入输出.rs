//! 设备 IO 命令
//!
//! - `cmd_read`    — 读取分区数据到文件
//! - `cmd_write`   — 写入文件到分区（支持 verify）
//! - `cmd_erase`   — 擦除分区
//! - `cmd_vbmeta`  — 修补 vbmeta (0/1/2/3)
//! - `cmd_reset`   — 重启设备
//! - `cmd_unlock`  — 解锁 Bootloader
//! - `cmd_lock`    — 锁定 Bootloader

use colored::Colorize;
use log::{info, warn};

use crate::DA扩展::DAXFlash;

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

/// 修补 vbmeta
pub fn cmd_vbmeta(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient vbmeta <mode> (0/1/2/3)".into());
    }
    let mode = args[0].parse::<u32>().map_err(|_| "无效模式")?;
    da.patch_vbmeta(mode)
        .map_err(|e| format!("修补失败: {}", e))?;
    info!("{}", "vbmeta 已修补".green());
    Ok(())
}

/// 重启设备
pub fn cmd_reset(da: &mut DAXFlash) -> Result<(), Box<dyn std::error::Error>> {
    match da.reset_device() {
        Ok(()) => {
            info!("{}", "设备已通过 DA 重启".green());
            // 设备重启后会退出 DA 模式，下次启动时无法复用会话
            crate::连接管理::reset_session();
            Ok(())
        }
        Err(e) => {
            warn!("DA 重启失败，回退到 BROM jump_bl: {}", e);
            da.close_device(true);
            // 设备重启后会退出 DA 模式，下次启动时无法复用会话
            crate::连接管理::reset_session();
            info!("{}", "设备已重启".green());
            Ok(())
        }
    }
}

/// 解锁 Bootloader
pub fn cmd_unlock(da: &mut DAXFlash) -> Result<(), Box<dyn std::error::Error>> {
    da.unlock_bootloader()
        .map_err(|e| format!("解锁失败: {}", e))?;
    info!("{}", "Bootloader 已解锁".green());
    Ok(())
}

/// 锁定 Bootloader
pub fn cmd_lock(da: &mut DAXFlash) -> Result<(), Box<dyn std::error::Error>> {
    da.lock_bootloader()
        .map_err(|e| format!("锁定失败: {}", e))?;
    info!("{}", "Bootloader 已锁定".green());
    Ok(())
}

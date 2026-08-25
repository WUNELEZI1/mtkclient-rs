use super::slot::write_partition_from_memory;
use crate::da::DAXFlash;
use log::{info, warn};

/// 设置 misc 分区的 bootloader_message 以重启到指定模式
pub(crate) fn set_bootloader_message(da: &mut DAXFlash, mode: &str) -> Result<(), String> {
    // bootloader_message 结构（存储在 misc 分区起始处）：
    // char command[32];      // "boot-recovery", "boot-fastboot" 等
    // char status[32];       // 状态
    // char recovery[1024];   // recovery 命令
    // char stage[32];        // 阶段信息
    // char reserved[1184];   // 保留

    let mut msg = vec![0u8; 32 + 32 + 1024]; // 只修改前 1088 字节

    let cmd_str = match mode {
        "fastboot" => "bootloader", // 进入 lk bootloader（adb reboot bootloader）
        "recovery" => "boot-recovery",
        "fastbootd" => "boot-fastboot", // 进入 fastbootd（用户空间 fastboot）
        _ => "",
    };

    if !cmd_str.is_empty() {
        let cmd_bytes = cmd_str.as_bytes();
        msg[..cmd_bytes.len()].copy_from_slice(cmd_bytes);
        info!("已设置 bootloader_message command='{}'", cmd_str);
    }

    // 读取 misc 分区到内存
    let (misc_addr, misc_size) = match da.find_partition_addr("misc") {
        Ok(r) => r,
        Err(e) => {
            warn!("查找 misc 分区失败 ({}），使用默认值", e);
            // 无法获取分区地址，分配一个最小的缓冲区
            let mut new_misc = vec![0u8; 0x1000];
            new_misc[..msg.len()].copy_from_slice(&msg);
            return write_partition_from_memory(da, "misc", &new_misc);
        }
    };

    let mut new_misc = match da.readflash_data(misc_addr, misc_size) {
        Ok(data) => data,
        Err(e) => {
            warn!("读取 misc 分区失败 ({})，使用默认值", e);
            vec![0u8; 0x1000]
        }
    };

    if new_misc.len() < msg.len() {
        new_misc.resize(msg.len(), 0);
    }
    new_misc[..msg.len()].copy_from_slice(&msg);

    // 直接从内存写入 misc 分区
    write_partition_from_memory(da, "misc", &new_misc)?;

    info!("misc 分区已更新");
    Ok(())
}

/// 通过 para 分区（BORA: Boot Option Recovery Area）设置启动模式
///
/// MTK para 分区结构：
///   偏移 0x00: boot_mode (u32) — 控制下一次启动模式
///   0 = NORMAL (system)
///   1 = FACTORY (recovery)
///   2 = FTM (fastbootd)
///   3 = META (meta mode)
///   4 = ATE (factory test)
///   5 = BROM (download mode)
///
/// 这是 misc 分区之外的第二种方案，部分设备的 LK 只识别 para
pub(crate) fn set_boot_mode_via_para(da: &mut DAXFlash, mode: &str) -> Result<(), String> {
    let boot_mode: u32 = match mode {
        "system" => 0, // 正常启动
        "recovery" => 1,
        "fastbootd" => 2,
        "fastboot" => 5, // BROM download mode，等同于 bootloader
        _ => return Err(format!("para 不支持的启动模式: {}", mode)),
    };

    // 读取 para 分区到内存
    let (para_addr, para_size) = match da.find_partition_addr("para") {
        Ok(r) => r,
        Err(e) => {
            warn!("查找 para 分区失败 ({})，回退到 misc", e);
            return set_bootloader_message(da, mode);
        }
    };

    let mut new_para = match da.readflash_data(para_addr, para_size) {
        Ok(data) => data,
        Err(e) => {
            warn!("读取 para 分区失败 ({})，回退到 misc", e);
            return set_bootloader_message(da, mode);
        }
    };

    // para 分区通常至少 4KB，确保能容纳 boot_mode
    if new_para.len() < 4 {
        new_para.resize(4, 0);
    }

    let old_mode = u32::from_le_bytes([new_para[0], new_para[1], new_para[2], new_para[3]]);
    new_para[0..4].copy_from_slice(&boot_mode.to_le_bytes());

    // 直接从内存写入 para 分区
    write_partition_from_memory(da, "para", &new_para)?;

    info!(
        "para 分区已更新: boot_mode 0x{:X} -> 0x{:X} ({})",
        old_mode, boot_mode, mode
    );
    Ok(())
}

use super::slot::write_partition_from_memory;
use crate::da::DAXFlash;
use log::{info, warn};

/// Set the bootloader_message in the misc partition to reboot into the given mode
pub(crate) fn set_bootloader_message(da: &mut DAXFlash, mode: &str) -> Result<(), String> {
    // bootloader_message layout (stored at the start of the misc partition):
    // char command[32];      // "boot-recovery", "boot-fastboot", etc.
    // char status[32];       // status
    // char recovery[1024];   // recovery command
    // char stage[32];        // stage info
    // char reserved[1184];   // reserved

    let mut msg = vec![0u8; 32 + 32 + 1024]; // only the first 1088 bytes are modified

    let cmd_str = match mode {
        "fastboot" => "bootloader", // enter the lk bootloader (adb reboot bootloader)
        "recovery" => "boot-recovery",
        "fastbootd" => "boot-fastboot", // enter fastbootd (userspace fastboot)
        _ => "",
    };

    if !cmd_str.is_empty() {
        let cmd_bytes = cmd_str.as_bytes();
        msg[..cmd_bytes.len()].copy_from_slice(cmd_bytes);
        info!("已设置 bootloader_message command='{}'", cmd_str);
    }

    // Read the misc partition into memory
    let (misc_addr, misc_size) = match da.find_partition_addr("misc") {
        Ok(r) => r,
        Err(e) => {
            warn!("查找 misc 分区失败 ({}），使用默认值", e);
            // Partition address unavailable; allocate a minimal buffer
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

    // Write the misc partition directly from memory
    write_partition_from_memory(da, "misc", &new_misc)?;

    info!("misc 分区已更新");
    Ok(())
}

/// Set the boot mode via the para partition (BORA: Boot Option Recovery Area)
///
/// MTK para partition layout:
///   offset 0x00: boot_mode (u32) — selects the next boot mode
///   0 = NORMAL (system)
///   1 = FACTORY (recovery)
///   2 = FTM (fastbootd)
///   3 = META (meta mode)
///   4 = ATE (factory test)
///   5 = BROM (download mode)
///
/// Second option besides misc; on some devices LK only honors para
pub(crate) fn set_boot_mode_via_para(da: &mut DAXFlash, mode: &str) -> Result<(), String> {
    let boot_mode: u32 = match mode {
        "system" => 0, // normal boot
        "recovery" => 1,
        "fastbootd" => 2,
        "fastboot" => 5, // BROM download mode, equivalent to bootloader
        _ => return Err(format!("para 不支持的启动模式: {}", mode)),
    };

    // Read the para partition into memory
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

    // The para partition is usually at least 4KB; ensure it can hold boot_mode
    if new_para.len() < 4 {
        new_para.resize(4, 0);
    }

    let old_mode = u32::from_le_bytes([new_para[0], new_para[1], new_para[2], new_para[3]]);
    new_para[0..4].copy_from_slice(&boot_mode.to_le_bytes());

    // Write the para partition directly from memory
    write_partition_from_memory(da, "para", &new_para)?;

    info!(
        "para 分区已更新: boot_mode 0x{:X} -> 0x{:X} ({})",
        old_mode, boot_mode, mode
    );
    Ok(())
}

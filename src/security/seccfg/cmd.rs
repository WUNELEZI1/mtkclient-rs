//! Bootloader 解锁/锁定命令入口
//!
//! 流程：读 seccfg 分区 → 识别 V3/V4 → 修改 lock_state → SEJ 签名 → 写回
//! 优化：如果 seccfg 状态已匹配目标，直接退出不执行 HACC/写入

use colored::Colorize;
use log::info;

use crate::da::DAXFlash;

use super::build::build_v4_image_online;
use super::v3::SecCfgV3;
use super::v4::SecCfgV4;

/// 在线模式解锁 bootloader
/// 对齐刷机匣：读取 seccfg 分区，更新锁状态并重新签名后写回
pub fn unlock_bootloader(da: &mut DAXFlash) -> Result<(), String> {
    info!("开始在线解锁 Bootloader...");
    let (seccfg_addr, seccfg_size) = da.find_partition_addr("seccfg")?;
    let seccfg_data = da.readflash_data(seccfg_addr, seccfg_size)?;
    info!(
        "  读取 seccfg 分区: addr=0x{:X}, size={} 字节",
        seccfg_addr,
        seccfg_data.len()
    );

    // 从 chip 缓存获取 hw_code，不使用 get_hw_code()（BROM echo 协议）
    // DA 已加载后发送 BROM echo 命令会破坏 DA 状态机
    let hw_code = da.preloader.chip.as_ref().map(|c| c.hw_code).unwrap_or(0);
    let new_data = if seccfg_data.len() >= 28
        && u32::from_le_bytes(seccfg_data[0..4].try_into().unwrap()) == SecCfgV4::MAGIC
    {
        let v4 = SecCfgV4::parse(&seccfg_data)?;
        // 早期退出：如果已经解锁，跳过 HACC 和写入
        if v4.lock_state == 3 {
            info!("设备已处于解锁状态，跳过 HACC 签名和写入");
            return Ok(());
        }
        build_v4_image_online(da, &v4, "unlock", seccfg_data.len(), hw_code)?
    } else {
        let v3 = SecCfgV3::parse(&seccfg_data)?;
        v3.create("unlock", seccfg_data.len())?
    };

    // 对齐 Python mtkclient：HACC 后直接 writeflash，不延迟不同步
    da.write_flash_data(seccfg_addr, &new_data, 1, 8)?;
    info!("{}", "Bootloader 解锁成功".green().bold());

    info!("Bootloader 已解锁");
    info!("提示: 如果设备因 dm-verity 无法启动，请手动运行 'zyb vbmeta 3' 禁用验证");

    Ok(())
}

/// 在线模式锁定 bootloader
pub fn lock_bootloader(da: &mut DAXFlash) -> Result<(), String> {
    info!("开始在线锁定 Bootloader...");
    let (seccfg_addr, seccfg_size) = da.find_partition_addr("seccfg")?;
    let seccfg_data = da.readflash_data(seccfg_addr, seccfg_size)?;

    // 从 chip 缓存获取 hw_code，不使用 get_hw_code()（BROM echo 协议）
    // DA 已加载后发送 BROM echo 命令会破坏 DA 状态机
    let hw_code = da.preloader.chip.as_ref().map(|c| c.hw_code)
        .ok_or_else(|| "芯片配置不可用，无法执行 HACC 签名".to_string())?;
    let new_data = if seccfg_data.len() >= 28
        && u32::from_le_bytes(seccfg_data[0..4].try_into().unwrap()) == SecCfgV4::MAGIC
    {
        let v4 = SecCfgV4::parse(&seccfg_data)?;
        // 早期退出：如果已经锁定，跳过 HACC 和写入
        if v4.lock_state == 1 {
            info!("设备已处于锁定状态，跳过 HACC 签名和写入");
            return Ok(());
        }
        build_v4_image_online(da, &v4, "lock", seccfg_data.len(), hw_code)?
    } else {
        let v3 = SecCfgV3::parse(&seccfg_data)?;
        v3.create("lock", seccfg_data.len())?
    };

    // 对齐 Python mtkclient：HACC 后直接 writeflash，不延迟不同步
    da.write_flash_data(seccfg_addr, &new_data, 1, 8)?;
    info!("Bootloader 锁定成功");
    Ok(())
}

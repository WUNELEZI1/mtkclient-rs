//! Bootloader 解锁/锁定命令入口
//!
//! 流程：读 seccfg 分区 → 识别 V3/V4 → 修改 lock_state → SEJ 签名 → 写回

use log::info;

use crate::da_extension::DAXFlash;

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

    let hw_code = da.preloader.get_hw_code().unwrap_or(0);
    let new_data = if seccfg_data.len() >= 28
        && u32::from_le_bytes(seccfg_data[0..4].try_into().unwrap()) == SecCfgV4::MAGIC
    {
        let v4 = SecCfgV4::parse(&seccfg_data)?;
        build_v4_image_online(da, &v4, "unlock", seccfg_data.len(), hw_code)?
    } else {
        let v3 = SecCfgV3::parse(&seccfg_data)?;
        v3.create("unlock", seccfg_data.len())?
    };

    da.write_flash_data(seccfg_addr, &new_data, 1, 8)?;
    info!("Bootloader 解锁成功");
    Ok(())
}

/// 在线模式锁定 bootloader
pub fn lock_bootloader(da: &mut DAXFlash) -> Result<(), String> {
    info!("开始在线锁定 Bootloader...");
    let (seccfg_addr, seccfg_size) = da.find_partition_addr("seccfg")?;
    let seccfg_data = da.readflash_data(seccfg_addr, seccfg_size)?;

    let hw_code = da.preloader.get_hw_code().unwrap_or(0);
    let new_data = if seccfg_data.len() >= 28
        && u32::from_le_bytes(seccfg_data[0..4].try_into().unwrap()) == SecCfgV4::MAGIC
    {
        let v4 = SecCfgV4::parse(&seccfg_data)?;
        build_v4_image_online(da, &v4, "lock", seccfg_data.len(), hw_code)?
    } else {
        let v3 = SecCfgV3::parse(&seccfg_data)?;
        v3.create("lock", seccfg_data.len())?
    };

    da.write_flash_data(seccfg_addr, &new_data, 1, 8)?;
    info!("Bootloader 锁定成功");
    Ok(())
}

#![allow(dead_code)] // 预留：FRP OEM 解锁/回锁接口，后续接命令路由时启用

use crate::da_xflash::DAXFlash;
use log::info;

fn update_oem_flag(mut data: Vec<u8>, enable: bool) -> Result<Vec<u8>, String> {
    if data.len() < 32 {
        return Err(format!("FRP 数据太小: {} 字节", data.len()));
    }

    // 按回读样本，OEM 开关位于 FRP 开头 32 字节的尾部。
    // 其余字节保持原样，避免破坏设备其它 FRP 记录。
    data[31] = if enable { 0x01 } else { 0x00 };
    Ok(data)
}

/// 通过修改 FRP 分区完成 OEM unlocking
pub fn unlock_oem_via_frp(da: &mut DAXFlash) -> Result<(), String> {
    let (frp_addr, frp_size) = da.find_partition_addr("frp")?;
    info!(
        "开始修改 FRP OEM 开关: addr=0x{:X}, size={} 字节",
        frp_addr, frp_size
    );

    let frp_data = da.readflash_data(frp_addr, frp_size)?;
    let new_data = update_oem_flag(frp_data, true)?;
    da.write_flash_data(frp_addr, &new_data, 1, 8)?;

    info!("FRP OEM unlocking 已启用");
    Ok(())
}

/// 回锁 FRP OEM 开关
pub fn lock_oem_via_frp(da: &mut DAXFlash) -> Result<(), String> {
    let (frp_addr, frp_size) = da.find_partition_addr("frp")?;
    info!(
        "开始回锁 FRP OEM 开关: addr=0x{:X}, size={} 字节",
        frp_addr, frp_size
    );

    let frp_data = da.readflash_data(frp_addr, frp_size)?;
    let new_data = update_oem_flag(frp_data, false)?;
    da.write_flash_data(frp_addr, &new_data, 1, 8)?;

    info!("FRP OEM unlocking 已关闭");
    Ok(())
}

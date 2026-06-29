#![allow(dead_code)]

use colored::Colorize;
use log::info;

use crate::DA扩展::DAXFlash;

pub fn vbmeta_disable(da: &mut DAXFlash, mode: u32) -> Result<(), String> {
    info!("开始 vbmeta 禁用 (mode={})...", mode);

    let candidates = ["vbmeta_a", "vbmeta_b", "vbmeta"];
    let mut patched_any = false;

    for name in &candidates {
        if let Ok((addr, size)) = da.find_partition_addr(name) {
            info!(
                "  找到 vbmeta 分区: {} @ 0x{:X}, 大小: 0x{:X}",
                name, addr, size
            );

            let data = da.readflash_data(addr, size)?;
            info!("  读取到 {} 字节", data.len());

            let modified = patch_vbmeta_data(&data, mode)?;
            da.write_flash_data(addr, &modified, 1, 8)?;
            info!("{}", format!("  {} 已修补", name).green());
            patched_any = true;
        }
    }

    if !patched_any {
        return Err("未找到 vbmeta 相关分区 (vbmeta_a/vbmeta_b/vbmeta)".to_string());
    }

    info!("{}", "vbmeta 禁用成功".green());
    Ok(())
}

fn patch_vbmeta_data(data: &[u8], mode: u32) -> Result<Vec<u8>, String> {
    if data.len() < 0x7C {
        return Err("vbmeta 数据太小".to_string());
    }

    // 验证 vbmeta 签名
    if &data[0..4] != b"AVB0" {
        return Err("非 vbmeta 格式 (缺少 AVB0 签名)".to_string());
    }

    let mut result = data.to_vec();

    // vbmeta[0x78..0x7C] = vbmode
    // mode 含义:
    //   0 = 验证启用 + 校验启用 (默认)
    //   1 = 验证禁用 + 校验启用
    //   2 = 验证启用 + 校验禁用
    //   3 = 验证禁用 + 校验禁用 (完全禁用)
    let flags_offset = 0x78;
    let old_flags = u32::from_le_bytes(result[flags_offset..flags_offset + 4].try_into().unwrap());
    result[flags_offset..flags_offset + 4].copy_from_slice(&mode.to_le_bytes());

    info!("  修改 vbmeta flags: 0x{:08X} -> 0x{:08X}", old_flags, mode);

    Ok(result)
}

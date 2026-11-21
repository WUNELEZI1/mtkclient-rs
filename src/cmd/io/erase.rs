use crate::color::Colorize;
use crate::da::DAXFlash;
use log::{info, warn};

/// Erase user data (factory reset)
///
/// **erase + format**: first erase the userdata, md_udc and metadata partitions,
/// then flash a minimal f2fs image to userdata (in-house generator, no mke2fs dependency),
/// so the partition stays mountable (some bootloaders do not rebuild the filesystem automatically).
///
/// Notes:
///   - The metadata partition stores dynamic-partition metadata; erasing it clears the super_metadata cache.
///   - The next dynamic-partition read will re-read metadata from the device.
///   - The bootloader rebuilds the metadata partition after reboot.
///   - Order: erase userdata → format userdata (f2fs) → erase md_udc → erase metadata.
pub fn cmd_erase_data(da: &mut DAXFlash) -> Result<(), Box<dyn std::error::Error>> {
    let partitions = ["userdata", "md_udc", "metadata"];

    warn!(
        "{}",
        "═══════════════════════════════════════════════"
            .yellow()
            .bold()
    );
    warn!("{}", "  即将执行恢复出厂设置！".yellow().bold());
    warn!("{}", "  将擦除: userdata, md_udc, metadata".yellow().bold());
    warn!(
        "{}",
        "  metadata 擦除后，super_metadata 缓存将被清除"
            .yellow()
            .bold()
    );
    warn!(
        "{}",
        "  设备重启后 bootloader 会自动重建 metadata"
            .yellow()
            .bold()
    );
    warn!(
        "{}",
        "═══════════════════════════════════════════════"
            .yellow()
            .bold()
    );

    for &part in &partitions {
        info!("{}", crate::i18n::tr("log_erase_start", &[part]));
        da.erase_partition(part)
            .map_err(|e| format!("擦除 {} 失败: {}", part, e))?;
        info!("{}", crate::i18n::tr("log_erased", &[part]).green());

        // format: flash a minimal f2fs to userdata after erasing (erase + format belt and braces)
        if part == "userdata" {
            info!("{}", crate::i18n::tr("log_format_start", &[part]));
            let fmt = da
                .find_partition_addr(part)
                .map_err(|e| format!("查找 {} 失败: {}", part, e))
                .and_then(|(addr, size)| {
                    crate::fs::format_image_for(part, size)
                        .map(|img| (addr, img))
                        .ok_or_else(|| "不支持该分区的格式化".to_string())
                });
            match fmt {
                Ok((addr, img)) => match da.write_flash_data(addr, &img, 1, 8) {
                    Ok(()) => info!("{}", crate::i18n::tr("log_format_done", &[part]).green()),
                    Err(e) => warn!("格式化 {} 失败（已擦除，跳过格式化）: {}", part, e),
                },
                Err(e) => warn!("生成 {} 格式化镜像失败: {}", part, e),
            }
        }
    }

    // After erasing metadata, drop the super_metadata cache and re-read it on demand
    da.super_metadata = None;
    info!(
        "{}",
        "super_metadata 缓存已清除（下次需要时自动重新读取）".dimmed()
    );

    info!("{}", "恢复出厂设置完成！请重启设备。".green().bold());
    info!("提示: 重启后执行 reboot 命令让设备进入系统");
    Ok(())
}

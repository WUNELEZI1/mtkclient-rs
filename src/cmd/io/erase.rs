use crate::color::Colorize;
use crate::da::DAXFlash;
use log::{info, warn};

/// 擦除分区
pub fn cmd_erase(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient e <part>".into());
    }
    da.erase_partition(&args[0])
        .map_err(|e| format!("擦除失败: {}", e))?;
    info!("{}", crate::i18n::tr("log_erased", &[&args[0]]).green());
    Ok(())
}

/// 清除用户数据（恢复出厂设置）
///
/// **erase + format**：先擦除 userdata、md_udc、metadata 三个分区，
/// 再对 userdata 刷入最小 f2fs 镜像（自研生成器，无 mke2fs 依赖），
/// 确保分区处于可挂载状态（部分 bootloader 不会自动重建文件系统）。
///
/// 注意：
///   - metadata 分区存储动态分区元数据，擦除后 super_metadata 缓存被清除。
///   - 下次需要动态分区读操作时，将从设备重新读取 metadata。
///   - 设备重启后 bootloader 会自动重建 metadata 分区。
///   - 顺序：erase userdata → format userdata(f2fs) → erase md_udc → erase metadata。
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

        // format：userdata 在擦除后刷入最小 f2fs（erase + format 双保险）
        if part == "userdata" {
            info!("{}", crate::i18n::tr("log_format_start", &[part]));
            let fmt = da
                .find_partition_addr(part)
                .map_err(|e| format!("查找 {} 失败: {}", part, e))
                .and_then(|(addr, size)| {
                    crate::system::fsimg::format_image_for(part, size)
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

    // metadata 擦除后，清除 super_metadata 缓存，下次需要时重新读取
    da.super_metadata = None;
    info!(
        "{}",
        "super_metadata 缓存已清除（下次需要时自动重新读取）".dimmed()
    );

    info!("{}", "恢复出厂设置完成！请重启设备。".green().bold());
    info!("提示: 重启后执行 reboot 命令让设备进入系统");
    Ok(())
}

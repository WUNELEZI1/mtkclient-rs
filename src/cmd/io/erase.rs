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
    info!("{}", format!("{} 已擦除", args[0]).green());
    Ok(())
}

/// 清除用户数据（恢复出厂设置）
///
/// 擦除 userdata、md_udc、metadata 三个分区。
/// 刷入新系统后需要执行此命令才能正常开机。
///
/// 注意：
///   - metadata 分区存储动态分区元数据，擦除后 super_metadata 缓存被清除。
///   - 下次需要动态分区读操作时，将从设备重新读取 metadata。
///   - 设备重启后 bootloader 会自动重建 metadata 分区。
///   - 擦除顺序：userdata → md_udc → metadata（metadata 最后擦除）。
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
        info!("正在擦除 {}...", part);
        da.erase_partition(part)
            .map_err(|e| format!("擦除 {} 失败: {}", part, e))?;
        info!("{}", format!("{} 已擦除", part).green());
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

//! 设备 IO 命令
//!
//! - `cmd_read`     — 读取分区数据到文件
//! - `cmd_write`    — 写入文件到分区（支持 verify）
//! - `cmd_erase`    — 擦除分区
//! - `cmd_reboot`   — 重启设备（支持 system/fastboot/recovery/fastbootd）
//! - `cmd_slot`     — 显示/切换 A/B 槽位
//! - `cmd_peek`     — 读取设备内存（hex dump 输出）
//! - `cmd_poke`     — 写入设备内存（hex 数据输入）

use colored::Colorize;
use log::{info, warn};
use std::time::Duration;

use crate::da::DAXFlash;

/// 读取分区数据到文件
///
/// 用法：
///   mtkclient r <part> <file>                     → 读取物理/逻辑分区
///   mtkclient r super --dp <logical_part> <file>   → 读取 super 内的动态分区
///
/// --dp 示例：
///   mtkclient r super --dp system system.img       → 读取 super 内的 system
///   mtkclient r super --dp vendor vendor.img       → 读取 super 内的 vendor
pub fn cmd_read(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err(
            "用法: mtkclient r <part> <file> 或 mtkclient r super --dp <logical_part> <file>"
                .into(),
        );
    }

    // 解析 --dp 参数：r super --dp system output.img
    let dp_idx = args.iter().position(|s| s == "--dp");
    if let Some(idx) = dp_idx {
        // 动态分区模式
        if args[0].to_lowercase() != "super" {
            return Err("--dp 只能与 super 分区一起使用".into());
        }
        let logical_name = args.get(idx + 1).ok_or("--dp 后缺少逻辑分区名")?;
        let output_file = args.get(idx + 2).ok_or("缺少输出文件名")?;

        info!(
            "读取 super 内的动态分区: {} → {}",
            logical_name, output_file
        );
        da.读取动态分区(logical_name, output_file)
            .map_err(|e| format!("读取动态分区失败: {}", e))?;
        info!(
            "{}",
            format!("super[{}] -> {}", logical_name, output_file).green()
        );
        return Ok(());
    }

    // 普通分区读取
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
        da.擦除分区(part)
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

/// 重启设备
///
/// 用法:
///   reboot                          → 正常重启到系统
///   reboot fastboot                 → 重启到 Bootloader（lk fastboot）
///   reboot recovery                 → 重启到 Recovery
///   reboot fastbootd                → 重启到 fastbootd（userspace fastboot）
///   reboot meta                     → 重启到 META 模式
///   reboot <mode> --via <method>    → 指定重启方式
///
/// --via 参数（仅对 fastboot/recovery/fastbootd/meta 有意义；system 默认忽略 --via）:
///   para      (默认) 通过 para 分区设置 boot_mode（最稳定）
///   misc      通过 misc 分区设置 bootloader_message
///   da        通过 DA SHUTDOWN 命令直接重启
///   xml       通过 XML DA SET-BOOT-MODE 重启（新平台）
///   preloader 通过 Preloader Pattern 协议（fastboot/meta，无需加载 DA）
///
/// system 重启说明（reboot / reboot system，默认路径）:
///   走硬件看门狗硬复位（write32(wdt+0x14, 0x1209)），不加载 DA、不写任何分区，
///   设备硬件复位后按默认 boot_mode(normal) 进入系统。brom 与 preloader 走同一
///   BROM WRITE32 原语，行为一致、最稳，且不受活 DA 会话是否存活影响。
///   --via 对该模式无效（即便指定 --via preloader 仍是看门狗，不再走 jump_bl）。
///
/// --via preloader 支持的模式:
///   fastboot  → Pattern FASTBOOT（BROM: 先 reset 到 Preloader，再 Pattern）
///   meta      → Pattern METAMETA（同上）
///   recovery/fastbootd → 不支持 Pattern，回退到 para
///
/// 模式与工作模式的关系:
///   --mode brom / --mode preloader（reboot system，默认）:
///     直接在 BROM/Preloader 握手态 trigger_meta_reboot（看门狗）→ 硬件复位进系统
///   --mode brom:
///     --via preloader: BROM write32 触发看门狗重启 → 等 Preloader → 握手 → trigger_meta_reboot → Pattern
///     --via para/misc/da/xml: 加载 DA → 写分区 → 重启
///   --mode preloader:
///     --via preloader: trigger_meta_reboot → Pattern
///     --via para/misc: 加载 DA → 写分区 → 重启
pub fn cmd_reboot(
    da: &mut DAXFlash,
    args: &[String],
    is_brom: bool,
    is_preloader: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // 解析参数：reboot [mode] [--via misc/para/da/xml/preloader]
    let mut mode = "system";
    let mut via = "para"; // 默认 para（最稳定）

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--via" => {
                via = args.get(i + 1).map(|s| s.as_str()).unwrap_or("para");
                i += 2;
            }
            "system" | "fastboot" | "recovery" | "fastbootd" | "meta" => {
                mode = args[i].as_str();
                i += 1;
            }
            _ => {
                return Err(format!(
                    "未知重启参数: {}。用法: reboot [system|fastboot|recovery|fastbootd|meta] [--via misc|para|da|xml|preloader]",
                    args[i]
                )
                .into());
            }
        }
    }

    // 未完成读取保护：若存在活跃读取续传（Ctrl+C 中断但未完成的分区读取），
    // reboot 会销毁 DA 会话，导致已读取进度无法续传，且复用会话下 DA SHUTDOWN
    // 常返回 0x00010007 而失败。此时保持会话，提示用户先完成分区读取，
    // 而非尝试 DA SHUTDOWN。
    let pending = crate::cmd::pending_read_resume_cwd();
    if !pending.is_empty() {
        warn!(
            "{}",
            "检测到未完成的读取任务，已跳过重启以保持 DA 会话"
                .yellow()
                .bold()
        );
        for p in &pending {
            let pct = if p.size > 0 {
                format!(
                    "（{:.1}% 已完成）",
                    p.written as f64 / p.size as f64 * 100.0
                )
            } else {
                String::new()
            };
            warn!(
                "  - {}：已读取 {}/{} 字节 {}",
                p.output, p.written, p.size, pct
            );
        }
        warn!(
            "{}",
            "请先完成分区读取（重新运行对应的 r <分区> <文件> 命令，会自动从断点续传）后再重启。"
                .yellow()
        );
        warn!(
            "{}",
            "若确定要放弃读取并强制重启，请先删除对应的 .img 与 .resume 文件后重试。".dimmed()
        );
        return Ok(());
    }

    // --via preloader: Pattern 协议路径（支持 fastboot 和 meta）
    if via == "preloader" {
        return cmd_reboot_via_preloader(da, mode, is_brom, is_preloader);
    }

    // 非 preloader 路径：需要加载 DA
    // Preloader 模式下 via 限制：仅支持 misc、para
    if is_preloader && !matches!(via, "misc" | "para") {
        warn!(
            "Preloader 模式下 via={} 不支持（仅支持 misc/para/preloader），已回退到 para",
            via
        );
        via = "para";
    }

    match mode {
        "system" => {
            // system 模式：通过硬件看门狗硬复位（write32(wdt+0x14, 0x1209)）。
            // 不依赖活 DA —— 本命令在 handle_command 中已提前 return（DA 未加载），
            // 设备仍处 BROM/Preloader 握手态，可直接下发 BROM WRITE32；设备硬件复位后
            // 按默认 boot_mode(normal) 进入系统。brom 与 preloader 走同一原语，行为一致。
            info!("通过看门狗硬复位重启到系统（无需加载 DA）...");
            // 兜底：brom 模式下连接可能未初始化 chip（trigger_meta_reboot 需要 chip）
            if da.preloader.chip.is_none() {
                if let Err(e) = da.preloader.get_hw_code() {
                    warn!("获取芯片信息失败（{}），无法触发看门狗", e);
                    crate::connection::reset_session();
                    info!("{}", "请手动重启设备（断开 USB 重新连接或按电源键）".yellow());
                    return Ok(());
                }
            }
            match da.preloader.trigger_meta_reboot() {
                Ok(_) => {
                    info!("{}", "看门狗已触发，设备正在硬件复位...".green());
                    crate::connection::reset_session();
                    info!("{}", "请保持或断开 USB，等待设备重启进入系统".cyan());
                }
                Err(e) => {
                    warn!("看门狗触发失败: {}", e);
                    crate::connection::reset_session();
                    info!("{}", "请手动重启设备（断开 USB 重新连接或按电源键）".yellow());
                }
            }
        }
        "fastboot" => {
            if is_brom {
                match via {
                    "xml" => {
                        info!("通过 XML DA 协议重启到 fastboot...");
                        da.da_xml_reboot_fastboot()
                            .map_err(|e| format!("XML DA fastboot 失败: {}", e))?;
                        info!("{}", "设备已通过 XML DA 重启到 fastboot".green());
                        crate::connection::reset_session();
                        return Ok(());
                    }
                    "para" => set_boot_mode_via_para(da, mode)?,
                    _ => set_bootloader_message(da, mode)?,
                }
                do_reboot(da, mode)?;
            } else {
                // Preloader 模式非 preloader via：需要加载 DA
                match via {
                    "para" => set_boot_mode_via_para(da, mode)?,
                    _ => set_bootloader_message(da, mode)?,
                }
                do_reboot(da, mode)?;
            }
        }
        "recovery" | "fastbootd" => {
            info!("准备重启到 {} 模式 (via {})...", mode, via);
            match via {
                "da" => {
                    set_bootloader_message(da, mode)?;
                }
                "para" => set_boot_mode_via_para(da, mode)?,
                "xml" => {
                    warn!("XML DA 不支持 {} 模式，回退到 misc", mode);
                    set_bootloader_message(da, mode)?;
                }
                _ => set_bootloader_message(da, mode)?,
            }
            do_reboot(da, mode)?;
        }
        "meta" => {
            info!("准备重启到 meta 模式 (via {})...", via);
            match via {
                "da" => {
                    info!("通过 DA SET_META_BOOT_MODE 重启到 meta...");
                    if let Err(e) = da.set_meta_boot_mode(1) {
                        warn!("SET_META_BOOT_MODE 失败: {}，回退到 misc", e);
                        set_bootloader_message(da, mode)?;
                    }
                }
                "xml" => {
                    da.da_xml_reboot_meta()
                        .map_err(|e| format!("XML DA meta 失败: {}", e))?;
                    info!("{}", "设备已通过 XML DA 重启到 meta".green());
                    crate::connection::reset_session();
                    return Ok(());
                }
                "para" => set_boot_mode_via_para(da, mode)?,
                _ => set_bootloader_message(da, mode)?,
            }
            do_reboot(da, mode)?;
        }
        _ => unreachable!(),
    }

    Ok(())
}

/// --via preloader: 通过 Pattern 协议重启设备
///
/// 支持的 mode: fastboot, meta
/// 不支持的 mode (recovery/fastbootd/system): 回退到 para
///
/// BROM 模式流程（DA 加载前拦截，设备仍在 BROM 握手状态）:
/// 1. trigger_meta_reboot: write32(wdt+0x14, 0x00001209) 触发看门狗重启
/// 2. 关闭串口 → 等待设备重枚举为 Preloader VCOM
/// 3. open_raw → 读 READY → 发送模式标识 → DISCONNECT
///
/// Preloader 模式流程（同上）
fn cmd_reboot_via_preloader(
    da: &mut DAXFlash,
    mode: &str,
    is_brom: bool,
    _is_preloader: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::cmd::preloader_boot_mode::{self, BootMode};

    // Pattern 协议支持的模式映射
    let boot_mode = match mode {
        "fastboot" => BootMode::Fastboot,
        "meta" => BootMode::Meta,
        _ => {
            // recovery/fastbootd/system 不支持 Pattern，回退到 para
            warn!(
                "{} 模式不支持 --via preloader（Pattern 协议无对应标识），回退到 --via para",
                mode
            );
            match mode {
                "system" => {
                    // system 模式：统一走硬件看门狗硬复位（与默认 reboot system 一致）
                    info!("通过看门狗硬复位重启到系统...");
                    if da.preloader.chip.is_none() {
                        if let Err(e) = da.preloader.get_hw_code() {
                            warn!("获取芯片信息失败（{}），无法触发看门狗", e);
                            crate::connection::reset_session();
                            info!("{}", "请手动重启设备".yellow());
                            return Ok(());
                        }
                    }
                    match da.preloader.trigger_meta_reboot() {
                        Ok(_) => {
                            info!("{}", "看门狗已触发，设备正在硬件复位...".green());
                            crate::connection::reset_session();
                        }
                        Err(e) => {
                            warn!("看门狗触发失败: {}", e);
                            crate::connection::reset_session();
                            info!("{}", "请手动重启设备".yellow());
                        }
                    }
                }
                "recovery" | "fastbootd" | "meta" => {
                    set_boot_mode_via_para(da, mode)?;
                    do_reboot(da, mode)?;
                }
                _ => {}
            }
            return Ok(());
        }
    };

    let mode_name = boot_mode.name();
    let tag = if is_brom { "BROM" } else { "Preloader" };
    info!("通过 Preloader Pattern 协议重启到 {}...", mode_name);

    // 步骤 1: trigger_meta_reboot 触发看门狗重启
    info!("  [{}] 步骤 1/2: 触发看门狗重启...", tag);
    match da.preloader.trigger_meta_reboot() {
        Ok(_) => info!("  看门狗已触发，设备正在重启..."),
        Err(e) => warn!("  看门狗触发失败: {}，继续等待设备重枚举", e),
    }
    let _ = da.preloader.device.close_device();
    crate::connection::reset_session();
    std::thread::sleep(Duration::from_millis(1000));

    // 步骤 2: 等待 Preloader VCOM → Pattern 协议
    info!(
        "  [{}] 步骤 2/2: 等待 Preloader VCOM → Pattern 协议...",
        tag
    );
    match preloader_boot_mode::try_preloader_pattern(boot_mode) {
        Ok(_) => {
            info!(
                "{}",
                format!("  设备已通过 Pattern 重启到 {}", mode_name)
                    .green()
                    .bold()
            );
            return Ok(());
        }
        Err(e) => {
            warn!("  Pattern 发送失败: {}，设备可能已进入目标模式", e);
            return Ok(());
        }
    }
}

/// 执行实际重启
/// 使用刷机匣协议（SYNC + CMD_SHUTDOWN），失败则提示用户手动重启
fn do_reboot(da: &mut DAXFlash, mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    match da.reset_device() {
        Ok(()) => {
            info!("{}", format!("设备已重启到 {} 模式", mode).green());
            info!("{}", "请断开 USB 连接，等待设备自动重启".cyan());
            crate::connection::reset_session();
            Ok(())
        }
        Err(e) => {
            warn!("重启失败: {}", e);
            crate::connection::reset_session();
            info!("{}", format!(
                "misc/para 分区已写入，请手动重启设备（断开 USB 重新连接或按电源键重启），设备将启动到 {} 模式",
                mode
            ).yellow());
            Ok(())
        }
    }
}

/// 设置 misc 分区的 bootloader_message 以重启到指定模式
fn set_bootloader_message(da: &mut DAXFlash, mode: &str) -> Result<(), String> {
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
fn set_boot_mode_via_para(da: &mut DAXFlash, mode: &str) -> Result<(), String> {
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

/// A/B 槽位管理
/// slot show — 显示当前槽位
/// slot a    — 切换到 A 槽
/// slot b    — 切换到 B 槽
pub fn cmd_slot(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient slot show/a/b".into());
    }

    let action = args[0].as_str();
    match action {
        "show" => show_slot(da)?,
        "a" => set_slot(da, 'a')?,
        "b" => set_slot(da, 'b')?,
        _ => {
            return Err(format!("用法: mtkclient slot show|a|b（未知槽位操作: {}）", action).into());
        }
    }

    Ok(())
}

/// 读取并显示当前 A/B 槽位
fn show_slot(da: &mut DAXFlash) -> Result<(), String> {
    // 尝试从 misc 分区读取槽位信息
    let slot = read_slot_from_misc(da)?;
    match slot {
        Some(s) => {
            info!("{}", format!("当前 A/B 槽位: {}", s).green().bold());
        }
        None => {
            info!("未检测到 A/B 槽位信息（设备可能不支持 A/B 分区）");
        }
    }
    Ok(())
}

/// 设置 A/B 槽位
fn set_slot(da: &mut DAXFlash, slot: char) -> Result<(), String> {
    info!("设置 A/B 槽位为 {}...", slot);

    // 读取 misc 分区到内存
    let (misc_addr, misc_size) = da
        .find_partition_addr("misc")
        .map_err(|e| format!("查找 misc 分区失败: {}", e))?;

    let mut new_misc = da
        .readflash_data(misc_addr, misc_size)
        .map_err(|e| format!("读取 misc 分区失败: {}", e))?;

    // 优先检查 bootctrl 结构（偏移 0x2000）
    let bootctrl_offset = 0x2000;
    if new_misc.len() >= bootctrl_offset + 12 {
        let magic = u32::from_le_bytes([
            new_misc[bootctrl_offset],
            new_misc[bootctrl_offset + 1],
            new_misc[bootctrl_offset + 2],
            new_misc[bootctrl_offset + 3],
        ]);
        if magic == 0x424F4F54 {
            // 已有 bootctrl 结构，直接修改 active_slot
            let new_slot_val = if slot == 'a' { 0u32 } else { 1u32 };
            new_misc[bootctrl_offset + 8..bootctrl_offset + 12]
                .copy_from_slice(&new_slot_val.to_le_bytes());
            info!("bootctrl 结构已更新 (active_slot={})", new_slot_val);
        } else {
            // 没有 bootctrl 结构，初始化一个
            warn!("misc 分区中无 bootctrl 结构，创建新的...");
            new_misc[bootctrl_offset..bootctrl_offset + 4]
                .copy_from_slice(&0x424F4F54u32.to_le_bytes()); // "BOOT"
            new_misc[bootctrl_offset + 4..bootctrl_offset + 8].copy_from_slice(&1u32.to_le_bytes()); // version
            new_misc[bootctrl_offset + 8..bootctrl_offset + 12]
                .copy_from_slice(&(if slot == 'a' { 0u32 } else { 1u32 }).to_le_bytes()); // active_slot
        }
    }

    // 直接从内存写入 misc 分区
    write_partition_from_memory(da, "misc", &new_misc)?;

    info!("{}", format!("A/B 槽位已设置为 {}", slot).green().bold());
    Ok(())
}

/// 从 misc 分区读取槽位信息
fn read_slot_from_misc(da: &mut DAXFlash) -> Result<Option<String>, String> {
    let (misc_addr, misc_size) = match da.find_partition_addr("misc") {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let misc_data = match da.readflash_data(misc_addr, misc_size) {
        Ok(data) => data,
        Err(_) => return Ok(None),
    };

    if misc_data.len() < 0x2010 {
        return Ok(None);
    }

    // 检查 bootctrl 结构（偏移 0x2000）
    let bootctrl_offset = 0x2000;
    let magic = u32::from_le_bytes([
        misc_data[bootctrl_offset],
        misc_data[bootctrl_offset + 1],
        misc_data[bootctrl_offset + 2],
        misc_data[bootctrl_offset + 3],
    ]);

    if magic == 0x424F4F54 {
        // "BOOT"
        let active_slot = u32::from_le_bytes([
            misc_data[bootctrl_offset + 8],
            misc_data[bootctrl_offset + 9],
            misc_data[bootctrl_offset + 10],
            misc_data[bootctrl_offset + 11],
        ]);
        let slot = if active_slot == 0 {
            "a".to_string()
        } else {
            "b".to_string()
        };
        return Ok(Some(slot));
    }

    // 尝试在数据中搜索槽位标记
    let data_str = String::from_utf8_lossy(&misc_data);
    if data_str.contains("_a") && !data_str.contains("_b") {
        return Ok(Some("a".to_string()));
    } else if data_str.contains("_b") && !data_str.contains("_a") {
        return Ok(Some("b".to_string()));
    }

    Ok(None)
}

/// 从内存直接写入分区数据（无需临时文件）
///
/// 将 data 写入到指定分区，自动查找分区地址并处理 512 字节对齐
fn write_partition_from_memory(
    da: &mut DAXFlash,
    part_name: &str,
    data: &[u8],
) -> Result<(), String> {
    let (addr, _size) = da
        .find_partition_addr(part_name)
        .map_err(|e| format!("查找分区 {} 失败: {}", part_name, e))?;

    da.write_flash_data(addr, data, 1, 8)
        .map_err(|e| format!("写入分区 {} 失败: {}", part_name, e))?;
    Ok(())
}

// =============================================================================
// peek / poke — 设备内存读写
// =============================================================================

/// 读取设备内存并输出 hex dump
///
/// 用法:
///   peek <addr>          — 读取 4 字节（默认）
///   peek <addr> <size>   — 读取指定字节数
///
/// 地址支持 0x 前缀或十进制格式
pub fn cmd_peek(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: peek <addr> [size]".into());
    }

    let addr = parse_addr(&args[0])?;
    let size: u32 = if args.len() >= 2 {
        parse_size(&args[1])?
    } else {
        4 // 默认 4 字节
    };

    if size == 0 {
        return Err("size 不能为 0".into());
    }
    if size > 0x100000 {
        return Err("size 不能超过 1MB（peek 适用于小范围内存读取）".into());
    }

    let data = da
        .cmd_peek(addr, size)
        .map_err(|e| format!("peek 失败: {}", e))?;

    println!(
        "{}",
        format!("[peek] 地址 0x{:08X}, 读取 {} 字节:", addr, data.len()).cyan()
    );
    println!("{}", crate::util::hex_dump(&data, addr));

    Ok(())
}

/// 写入数据到设备内存
///
/// 用法:
///   poke <addr> <hex_data>
///
/// 地址支持 0x 前缀或十进制格式
/// hex_data 支持: AABB、AA BB、AA:BB、0xAABB 等格式
pub fn cmd_poke(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err("用法: poke <addr> <hex_data>".into());
    }

    let addr = parse_addr(&args[0])?;
    let data = crate::util::parse_hex(&args[1]).map_err(|e| format!("hex 数据解析失败: {}", e))?;

    if data.is_empty() {
        return Err("hex 数据为空".into());
    }

    info!(
        "写入 {} 字节到地址 0x{:08X}: {}",
        data.len(),
        addr,
        crate::util::hex_str(&data)
    );

    da.cmd_poke(addr, &data)
        .map_err(|e| format!("poke 失败: {}", e))?;

    info!(
        "{}",
        format!("已成功写入 {} 字节到 0x{:08X}", data.len(), addr).green()
    );

    Ok(())
}

/// 解析地址参数（支持 0x 前缀的十六进制或十进制）
pub fn parse_addr(s: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
            .map_err(|e| format!("无效的十六进制地址 '{}': {}", s, e).into())
    } else {
        s.parse::<u64>()
            .map_err(|e| format!("无效的地址 '{}': {}", s, e).into())
    }
}

/// 解析 size 参数（支持 0x 前缀的十六进制或十进制）
pub fn parse_size(s: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
            .map_err(|e| format!("无效的十六进制 size '{}': {}", s, e).into())
    } else {
        s.parse::<u32>()
            .map_err(|e| format!("无效的 size '{}': {}", s, e).into())
    }
}

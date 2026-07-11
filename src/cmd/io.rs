//! 设备 IO 命令
//!
//! - `cmd_read`     — 读取分区数据到文件
//! - `cmd_write`    — 写入文件到分区（支持 verify）
//! - `cmd_erase`    — 擦除分区
//! - `cmd_reboot`   — 重启设备（支持 system/fastboot/recovery/fastbootd）
//! - `cmd_slot`     — 显示/切换 A/B 槽位

use colored::Colorize;
use log::{info, warn};

use crate::da::DAXFlash;
use crate::da::xflash::protocol::ShutdownBootMode;
use crate::preloader::transport::BromTransport;

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
        return Err("用法: mtkclient r <part> <file> 或 mtkclient r super --dp <logical_part> <file>".into());
    }

    // 解析 --dp 参数：r super --dp system output.img
    let dp_idx = args.iter().position(|s| s == "--dp");
    if let Some(idx) = dp_idx {
        // 动态分区模式
        if args[0].to_lowercase() != "super" {
            return Err("--dp 只能与 super 分区一起使用".into());
        }
        let logical_name = args.get(idx + 1)
            .ok_or("--dp 后缺少逻辑分区名")?;
        let output_file = args.get(idx + 2)
            .ok_or("缺少输出文件名")?;

        info!("读取 super 内的动态分区: {} → {}", logical_name, output_file);
        da.读取动态分区(logical_name, output_file)
            .map_err(|e| format!("读取动态分区失败: {}", e))?;
        info!("{}", format!("super[{}] -> {}", logical_name, output_file).green());
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

    warn!("{}", "═══════════════════════════════════════════════".yellow().bold());
    warn!("{}", "  即将执行恢复出厂设置！".yellow().bold());
    warn!("{}", "  将擦除: userdata, md_udc, metadata".yellow().bold());
    warn!("{}", "  metadata 擦除后，super_metadata 缓存将被清除".yellow().bold());
    warn!("{}", "  设备重启后 bootloader 会自动重建 metadata".yellow().bold());
    warn!("{}", "═══════════════════════════════════════════════".yellow().bold());

    for &part in &partitions {
        info!("正在擦除 {}...", part);
        da.擦除分区(part)
            .map_err(|e| format!("擦除 {} 失败: {}", part, e))?;
        info!("{}", format!("{} 已擦除", part).green());
    }

    // metadata 擦除后，清除 super_metadata 缓存，下次需要时重新读取
    da.super_metadata = None;
    info!("{}", "super_metadata 缓存已清除（下次需要时自动重新读取）".dimmed());

    info!("{}", "恢复出厂设置完成！请重启设备。".green().bold());
    info!("提示: 重启后执行 reboot 命令让设备进入系统");
    Ok(())
}

/// 重启设备
/// 用法:
///   reboot                          → 正常重启到系统
///   reboot fastboot                 → 重启到 Bootloader（lk fastboot）
///   reboot recovery                 → 重启到 Recovery
///   reboot fastbootd                → 重启到 fastbootd（userspace fastboot）
///   reboot fastboot --via para      → 通过 para 分区设置 boot_mode=5（DA模式）
///   reboot fastboot --via da        → 通过 XFlash DA SHUTDOWN bootmode=2 直接重启
///   reboot fastboot --via xml       → 通过 XML DA SET-BOOT-MODE 重启（新平台）
pub fn cmd_reboot(
    da: &mut DAXFlash,
    args: &[String],
    is_brom: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // 解析参数：reboot [mode] [--via misc/para/da/xml]
    let mut mode = "system";
    let mut via = "misc";

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--via" => {
                via = args.get(i + 1).map(|s| s.as_str()).unwrap_or("misc");
                i += 2;
            }
            "system" | "fastboot" | "recovery" | "fastbootd" | "meta" => {
                mode = args[i].as_str();
                i += 1;
            }
            _ => {
                return Err(format!(
                    "未知重启参数: {}。用法: reboot [system|fastboot|recovery|fastbootd|meta] [--via misc|para|da|xml]",
                    args[i]
                )
                .into());
            }
        }
    }

    match mode {
        "system" => {
            // 正常重启：通过 DA SHUTDOWN(bootmode=REBOOT)
            do_reboot(da, "system")?;
        }
        "fastboot" => {
            // BROM 模式下：先 jump_bl 重启到 bootloader，然后尝试 Preloader Pattern
            if is_brom {
                info!("BROM 模式下重启到 fastboot：执行 jump_bl...");
                match da.preloader.jump_bl() {
                    Ok(_) => {
                        info!("jump_bl 成功，设备正在重启到 Bootloader...");
                        crate::connection::reset_session();
                        // 等待设备重新枚举
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        // 尝试连接 Preloader 串口并发送 Pattern
                        match try_preloader_pattern_reboot() {
                            Ok(_) => {
                                info!("{}", "设备已通过 Preloader Pattern 重启到 Bootloader".green());
                                return Ok(());
                            }
                            Err(e) => {
                                warn!("Preloader Pattern 发送失败: {}，设备可能已进入 Bootloader", e);
                                return Ok(());
                            }
                        }
                    }
                    Err(e) => {
                        warn!("jump_bl 失败: {}，回退到 misc 分区方式", e);
                        set_bootloader_message(da, mode)?;
                        do_reboot(da, mode)?;
                    }
                }
            } else {
                // DA 模式下：根据 --via 选择重启方式
                match via {
                    "da" => {
                        // Layer 2: XFlash DA SHUTDOWN bootmode=2
                        info!("通过 DA SHUTDOWN 直接重启到 fastboot...");
                        da.da_reboot_fastboot()
                            .map_err(|e| format!("DA SHUTDOWN fastboot 失败: {}", e))?;
                        info!("{}", "设备已通过 DA SHUTDOWN 重启到 fastboot".green());
                        crate::connection::reset_session();
                        return Ok(());
                    }
                    "xml" => {
                        // Layer 3: XML DA SET-BOOT-MODE + REBOOT
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
            }
        }
        "recovery" | "fastbootd" => {
            info!("准备重启到 {} 模式 (via {})...", mode, via);
            match via {
                "da" => {
                    // DA 模式下 fastbootd/recovery 仍通过 misc 分区设置
                    // 因为 SHUTDOWN bootmode 只支持 0/1/2，没有 recovery/fastbootd
                    set_bootloader_message(da, mode)?;
                    do_reboot(da, mode)?;
                }
                "para" => set_boot_mode_via_para(da, mode)?,
                _ => set_bootloader_message(da, mode)?,
            }
            do_reboot(da, mode)?;
        }
        "meta" => {
            info!("准备重启到 {} 模式 (via {})...", mode, via);
            match via {
                "da" => {
                    // DA 模式下 meta 通过 SET_META_BOOT_MODE devctrl
                    info!("通过 DA SET_META_BOOT_MODE 重启到 meta...");
                    if let Err(e) = da.set_meta_boot_mode(1) {
                        warn!("SET_META_BOOT_MODE 失败: {}，回退到 misc", e);
                        set_bootloader_message(da, mode)?;
                    }
                    do_reboot(da, mode)?;
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

/// 尝试连接 Preloader 串口并发送 Fastboot Pattern
fn try_preloader_pattern_reboot() -> Result<(), String> {
    use std::time::Duration;
    use crate::preloader::SerialPortTransport;
    use crate::cmd::preloader_boot_mode;

    info!("等待 Preloader COM 口出现（最多 15 秒）...");

    // 最多等待 15 秒，每 200ms 扫描一次
    let max_wait_ms = 15000u64;
    let interval_ms = 200u64;
    let max_retries = max_wait_ms / interval_ms;

    for _retry in 0..max_retries {
        if let Some(port_result) = SerialPortTransport::find_brom_port_with_timeout(interval_ms) {
            match port_result {
                crate::preloader::BromPortResult::SerialPort(port_name) => {
                    info!("发现 Preloader COM 口: {}", port_name);
                    // 打开串口
                    let mut transport = SerialPortTransport::new(&port_name, 115200)
                        .map_err(|e| format!("打开串口失败: {}", e))?;
                    // 设置超时
                    transport.set_timeout(Duration::from_millis(2000));

                    // 尝试握手（部分 Preloader 需要）
                    info!("尝试 Preloader 握手...");
                    if let Err(e) = transport.do_handshake() {
                        warn!("握手失败（可能不需要）: {}", e);
                    }

                    // 发送 Fastboot Pattern
                    info!("发送 Fastboot Pattern...");
                    preloader_boot_mode::send_boot_pattern(
                        &mut transport,
                        preloader_boot_mode::BootMode::Fastboot,
                    )?;
                    return Ok(());
                }
                _ => {
                    // WinUsbDevice 或其他结果，不是串口
                    continue;
                }
            }
        }
    }

    Err("未在 15 秒内找到 Preloader COM 口".to_string())
}

/// 执行实际重启（使用 XFlash DA SHUTDOWN 协议）
fn do_reboot(da: &mut DAXFlash, mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    let bootmode = match mode {
        "system" => ShutdownBootMode::Reboot,
        _ => ShutdownBootMode::Reboot,
    };

    match da.da_shutdown(bootmode) {
        Ok(()) => {
            info!("{}", format!("设备已重启到 {} 模式 (DA SHUTDOWN)", mode).green());
            crate::connection::reset_session();
            Ok(())
        }
        Err(e) => {
            warn!("DA SHUTDOWN 失败: {}，回退到 jump_bl", e);
            da.close_device(true);
            crate::connection::reset_session();
            info!("{}", format!("设备已重启到 {} 模式", mode).green());
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
        "fastboot" => "bootloader",     // 进入 lk bootloader（adb reboot bootloader）
        "recovery" => "boot-recovery",
        "fastbootd" => "boot-fastboot", // 进入 fastbootd（用户空间 fastboot）
        _ => "",
    };

    if !cmd_str.is_empty() {
        let cmd_bytes = cmd_str.as_bytes();
        msg[..cmd_bytes.len()].copy_from_slice(cmd_bytes);
        info!("已设置 bootloader_message command='{}'", cmd_str);
    }

    // 读取 misc 分区，修改前 1088 字节，写回
    let misc_data = match da.读取分区("misc", "misc_tmp_read.bin") {
        Ok(_) => std::fs::read("misc_tmp_read.bin").unwrap_or_else(|_| vec![0u8; 0x1000]),
        Err(e) => {
            warn!("读取 misc 分区失败 ({})，尝试直接写入", e);
            vec![0u8; 0x1000] // 4KB 默认值
        }
    };

    let mut new_misc = misc_data;
    if new_misc.len() < msg.len() {
        new_misc.resize(msg.len(), 0);
    }
    new_misc[..msg.len()].copy_from_slice(&msg);

    // 写回 misc 分区
    std::fs::write("misc_reboot_tmp.bin", &new_misc).map_err(|e| e.to_string())?;
    da.写入分区("misc", "misc_reboot_tmp.bin")
        .map_err(|e| format!("写入 misc 分区失败: {}", e))?;

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
        "fastboot" => 5,  // BROM download mode，等同于 bootloader
        "recovery" => 1,
        "fastbootd" => 2,
        _ => return Err(format!("para 不支持的启动模式: {}", mode)),
    };

    // 读取 para 分区
    let para_data = match da.读取分区("para", "para_reboot_tmp_read.bin") {
        Ok(_) => std::fs::read("para_reboot_tmp_read.bin")
            .unwrap_or_else(|_| vec![0u8; 0x2000]),
        Err(e) => {
            warn!("读取 para 分区失败 ({})，回退到 misc", e);
            return set_bootloader_message(da, mode);
        }
    };

    let mut new_para = para_data;
    // para 分区通常至少 4KB，确保能容纳 boot_mode
    if new_para.len() < 4 {
        new_para.resize(4, 0);
    }

    let old_mode = u32::from_le_bytes([
        new_para[0], new_para[1], new_para[2], new_para[3],
    ]);
    new_para[0..4].copy_from_slice(&boot_mode.to_le_bytes());

    std::fs::write("para_reboot_tmp.bin", &new_para).map_err(|e| e.to_string())?;
    da.写入分区("para", "para_reboot_tmp.bin")
        .map_err(|e| format!("写入 para 分区失败: {}", e))?;

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
        _ => return Err(format!("未知槽位操作: {}。支持: show, a, b", action).into()),
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

    // 读取 misc 分区
    let misc_data = match da.读取分区("misc", "misc_tmp_read2.bin") {
        Ok(_) => std::fs::read("misc_tmp_read2.bin").map_err(|e| e.to_string())?,
        Err(e) => return Err(format!("读取 misc 分区失败: {}", e)),
    };

    let mut new_misc = misc_data.clone();

    // 尝试在 misc 分区中查找并修改槽位标记
    // A/B 槽位信息通常存储在 misc 分区的 bootctrl 区域（偏移 0x2000 或 0x4000）
    // 或者在 bootloader_message 的保留区域中
    // 这里尝试查找已知的槽位标记模式
    let slot_marker_a = b"_a";
    let slot_marker_b = b"_b";

    // 简单策略：查找并替换槽位标记
    let modified = if slot == 'a' {
        replace_slot_markers(&mut new_misc, slot_marker_b, slot_marker_a)
    } else {
        replace_slot_markers(&mut new_misc, slot_marker_a, slot_marker_b)
    };

    if modified == 0 {
        warn!("未在 misc 分区中找到槽位标记，尝试写入 bootctrl 结构...");
        // 如果找不到标记，尝试在偏移 0x2000 处写入槽位信息
        if new_misc.len() < 0x2010 {
            new_misc.resize(0x2010, 0);
        }
        // bootctrl 结构：magic(4) + version(4) + active_slot(4) + ...
        let bootctrl_offset = 0x2000;
        new_misc[bootctrl_offset..bootctrl_offset + 4]
            .copy_from_slice(&0x424F4F54u32.to_le_bytes()); // "BOOT"
        new_misc[bootctrl_offset + 4..bootctrl_offset + 8].copy_from_slice(&1u32.to_le_bytes()); // version
        new_misc[bootctrl_offset + 8..bootctrl_offset + 12]
            .copy_from_slice(&(if slot == 'a' { 0u32 } else { 1u32 }).to_le_bytes()); // active_slot
    }

    // 写回 misc 分区
    std::fs::write("misc_slot_tmp.bin", &new_misc).map_err(|e| e.to_string())?;
    da.写入分区("misc", "misc_slot_tmp.bin")
        .map_err(|e| format!("写入 misc 分区失败: {}", e))?;

    info!("{}", format!("A/B 槽位已设置为 {}", slot).green().bold());
    Ok(())
}

/// 从 misc 分区读取槽位信息
fn read_slot_from_misc(da: &mut DAXFlash) -> Result<Option<String>, String> {
    let misc_data = match da.读取分区("misc", "misc_tmp_read3.bin") {
        Ok(_) => match std::fs::read("misc_tmp_read3.bin") {
            Ok(data) => data,
            Err(_) => return Ok(None),
        },
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

/// 在字节数组中替换槽位标记
fn replace_slot_markers(data: &mut [u8], from: &[u8], to: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i + from.len() <= data.len() {
        if &data[i..i + from.len()] == from {
            data[i..i + to.len()].copy_from_slice(to);
            count += 1;
        }
        i += 1;
    }
    count
}

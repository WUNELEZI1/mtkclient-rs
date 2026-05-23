use colored::Colorize;
use log::{error, info, warn};
use std::time::SystemTime;

use crate::DeviceMode;
use crate::config::AppConfig;
use crate::da_xflash::{DAXFlash, generate_scatter_from_gpt};
use crate::usb::UsbContext;

pub fn print_help() {
    println!("用法:");
    println!("  mtkclient-rs.exe <命令> [参数]");
    println!("  mtkclient-rs.exe --batch \"<命令1>\" \"<命令2>\" ...");
    println!();
    println!("命令:");
    println!("  printgpt         打印 GPT 分区表");
    println!("  dump-preloader   提取 Preloader");
    println!("  dumpbrom         提取 BROM");
    println!("  r <分区> <文件>  读取分区");
    println!("  w <分区> <文件>  写入分区");
    println!("  e <分区>        擦除分区");
    println!("  vbmeta <模式>   修补 vbmeta (0/1/2/3)");
    println!("  reset            重启设备");
    println!("  unlock           解锁 Bootloader");
    println!("  lock             锁定 Bootloader");
    println!("  enable-adb-on-da 在 DA 模式下开启 ADB");
    println!();
    println!("诊断:");
    println!("  diagnose         USB 连接诊断（设备状态、驱动、模式）");
    println!("  list-usb         列出所有 USB 设备");
    println!("  check-driver     检查 WinUSB 驱动状态");
    println!();
    println!("驱动:");
    println!("  install-drivers  安装 WinUSB 驱动 (管理员)");
    println!();
    println!("离线模式:");
    println!("  unlock --no-device <seccfg文件>  离线解锁 seccfg");
    println!("  lock --no-device <seccfg文件>    离线锁定 seccfg");
    println!();
    println!("批量模式:");
    println!("  --batch \"printgpt\" \"r boot boot.img\" \"e userdata\"");
    println!();
    println!("选项:");
    println!("  --preloader <文件>  指定 preloader 文件");
    println!("  --verify            写入后校验");
    println!("  --check-driver      检查驱动状态");
    println!("  --debug-mode        输出调试日志");
    println!("  --batch             批量执行多个命令");
    println!("  --force             强制安装驱动");
}

pub fn handle_command(
    da: &mut DAXFlash,
    _mode: &DeviceMode,
    app_config: &AppConfig,
    debug_mode: bool,
    quiet_dump: bool,
    preloader_file: &str,
    context: &UsbContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let is_brom = !da.preloader.is_preloader_mode;
    let mut auto_dumped_file: Option<String> = None;

    if is_brom {
        let mut _security_bypassed = false;
        if let Ok(cfg) = da.preloader.get_target_config() {
            info!("{}", cfg.format_info());
            if cfg.needs_bypass() {
                da.preloader.bypass_security().map_err(|e| {
                    error!("{}", format!("安全绕过失败: {}", e).red());
                    e
                })?;
                _security_bypassed = true;
                info!("安全绕过完成");
            }
        }

        let cmd = app_config.command.as_deref().unwrap_or("");
        match cmd {
            "dumpbrom" => {
                cmd_dumpbrom(da, debug_mode)?;
                return Ok(());
            }
            "reset" => {
                cmd_reset(da);
                return Ok(());
            }
            _ => {}
        }

        if preloader_file.is_empty() {
            let (data, filename) = da
                .preloader
                .dump_preloader_payload(false, quiet_dump, context)
                .map_err(|e| format!("Kamakiri2 失败: {}", e))?;

            if !data.is_empty() {
                std::fs::write(&filename, &data)?;
                info!("Preloader 已提取: {} ({} 字节)", filename, data.len());
                auto_dumped_file = Some(filename);
            }

            // dump 后重新握手，恢复 BROM 通信状态
            if let Err(e) = da.preloader.device.do_handshake() {
                warn!("dump 后握手失败: {}，尝试重新初始化设备", e);
                da.preloader.device.reopen(context).map_err(|e| {
                    error!("重新连接设备失败: {}", e);
                    e
                })?;
                da.preloader.is_preloader_mode = false;
                da.preloader.chip = None;
            }
        }

        let cmd = app_config.command.as_deref().unwrap_or("");
        if cmd == "dump-preloader" {
            return Ok(());
        }
    }

    let cmd: &str = match &app_config.command {
        Some(c) => c,
        None => {
            error!("{}", "未指定命令".red());
            print_help();
            return Err("未指定命令".into());
        }
    };

    if cmd == "dump-preloader" {
        return Ok(());
    }

    let effective_file = auto_dumped_file.as_deref().unwrap_or(preloader_file);
    info!("加载 EMI 数据: {}", effective_file);
    if let Err(e) = da.load_preloader_emi(effective_file) {
        info!("Warning: EMI 加载失败: {}", e);
    }

    // Python 全程使用同一个 USB 句柄，不做 reopen
    da.upload_da()
        .map_err(|e| format!("DA 加载失败: {}", e))
        .and_then(|ok| {
            if ok {
                Ok(())
            } else {
                Err("DA 加载失败".to_string())
            }
        })?;

    if debug_mode {
        if let Some(data) = da.get_emi_data() {
            let _ = std::fs::write("emi_debug.bin", data);
        }
        if let Some(data) = da.get_extensions_data() {
            let _ = std::fs::write("extensions_debug.bin", &data);
        }
    }

    let args = &app_config.cmd_args;
    let verify = app_config.verify;

    if cmd == "enable-adb-on-da" {
        da.enable_adb_and_reboot()?;
        info!("{}", "ADB 已启用，设备正在重启进入系统".green());
        return Ok(());
    }

    execute_single_command(da, cmd, args, verify)?;

    // 单命令执行完毕，复位设备恢复 BROM 状态
    let da_commands = [
        "printgpt", "r", "read", "w", "write", "e", "erase", "vbmeta", "unlock", "lock",
    ];
    if da_commands.contains(&cmd) {
        let _ = da.preloader.jump_bl();
    }

    Ok(())
}

/// 批量执行多个命令，保持 DA 会话
/// commands 是 Vec<(命令名, 参数列表)>
#[allow(clippy::too_many_arguments)]
pub fn handle_commands(
    da: &mut DAXFlash,
    _mode: &DeviceMode,
    app_config: &AppConfig,
    debug_mode: bool,
    quiet_dump: bool,
    preloader_file: &str,
    commands: &[(String, Vec<String>)],
    context: &UsbContext,
) -> Result<(), Box<dyn std::error::Error>> {
    if commands.is_empty() {
        return Err("没有要执行的命令".into());
    }

    let is_brom = !da.preloader.is_preloader_mode;
    let mut auto_dumped_file: Option<String> = None;

    if is_brom {
        let mut _security_bypassed = false;
        if let Ok(cfg) = da.preloader.get_target_config() {
            info!("{}", cfg.format_info());
            if cfg.needs_bypass() {
                da.preloader.bypass_security().map_err(|e| {
                    error!("{}", format!("安全绕过失败: {}", e).red());
                    e
                })?;
                _security_bypassed = true;
                info!("安全绕过完成");
            }
        }

        if preloader_file.is_empty() {
            let (data, filename) = da
                .preloader
                .dump_preloader_payload(false, quiet_dump, context)
                .map_err(|e| format!("Kamakiri2 失败: {}", e))?;

            if !data.is_empty() {
                std::fs::write(&filename, &data)?;
                info!("Preloader 已提取: {} ({} 字节)", filename, data.len());
                auto_dumped_file = Some(filename);
            }

            // dump 后重新握手，恢复 BROM 通信状态
            if let Err(e) = da.preloader.device.do_handshake() {
                warn!("dump 后握手失败: {}，尝试重新初始化设备", e);
                da.preloader.device.reopen(context).map_err(|e| {
                    error!("重新连接设备失败: {}", e);
                    e
                })?;
                da.preloader.is_preloader_mode = false;
                da.preloader.chip = None;
            }
        }
    }

    let effective_file = auto_dumped_file.as_deref().unwrap_or(preloader_file);
    info!("加载 EMI 数据: {}", effective_file);
    if let Err(e) = da.load_preloader_emi(effective_file) {
        info!("Warning: EMI 加载失败: {}", e);
    }

    // Python 全程使用同一个 USB 句柄，不做 reopen
    da.upload_da()
        .map_err(|e| format!("DA 加载失败: {}", e))
        .and_then(|ok| {
            if ok {
                Ok(())
            } else {
                Err("DA 加载失败".to_string())
            }
        })?;

    if debug_mode {
        if let Some(data) = da.get_emi_data() {
            let _ = std::fs::write("emi_debug.bin", data);
        }
        if let Some(data) = da.get_extensions_data() {
            let _ = std::fs::write("extensions_debug.bin", &data);
        }
    }

    let verify = app_config.verify;

    for (i, (cmd, args)) in commands.iter().enumerate() {
        info!(
            ">>> 执行命令 {}/{}: {} {}",
            i + 1,
            commands.len(),
            cmd,
            args.join(" ")
        );
        if let Err(e) = execute_single_command(da, cmd, args, verify) {
            error!("命令执行失败: {}", e);
        }
    }

    // 所有命令执行完毕，复位设备恢复 BROM 状态
    let _ = da.preloader.jump_bl();

    Ok(())
}

/// 执行单个 DA 命令（不处理 Phase1/Phase2）
fn execute_single_command(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        "printgpt" => cmd_printgpt(da, false),
        "r" | "read" => cmd_read(da, args)?,
        "w" | "write" => cmd_write(da, args, verify)?,
        "e" | "erase" => cmd_erase(da, args)?,
        "vbmeta" => cmd_vbmeta(da, args)?,
        "reset" => cmd_reset(da),
        "unlock" => cmd_unlock(da)?,
        "lock" => cmd_lock(da)?,
        "enable-adb-on-da" => {
            da.enable_adb_and_reboot()?;
            info!("{}", "ADB 已启用，设备正在重启进入系统".green());
        }
        _ => {
            error!("{}", format!("未知命令: {}", cmd).red());
            print_help();
            return Err(format!("未知命令: {}", cmd).into());
        }
    }

    Ok(())
}

fn cmd_printgpt(da: &mut DAXFlash, debug_mode: bool) {
    match da.read_gpt() {
        Ok(_) => {
            // 获取 EMMC Boot1/Boot2 信息
            if let Ok(emmc_info) = da.get_emmc_info() {
                println!("\nEMMC 信息:");
                println!(
                    "  EMMC Boot1 Size: 0x{:06X} ({} MB)",
                    emmc_info.boot1_size,
                    emmc_info.boot1_size / 1024 / 1024
                );
                println!(
                    "  EMMC Boot2 Size: 0x{:06X} ({} MB)",
                    emmc_info.boot2_size,
                    emmc_info.boot2_size / 1024 / 1024
                );
            }

            info!("{}", "GPT 读取成功".green());
            if debug_mode {
                let ts = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let file = format!("gpt_debug_{}.bin", ts);
                if let Ok(data) = da.get_last_gpt_data() {
                    let _ = std::fs::write(&file, data);
                }
            }
            if let Ok(data) = da.get_last_gpt_data() {
                match generate_scatter_from_gpt(data, "scatter.txt") {
                    Ok(parts) => info!("  scatter.txt 已生成 ({} 分区)", parts.len()),
                    Err(e) => info!("  Warning: scatter 生成失败: {}", e),
                }
            }
        }
        Err(e) => error!("{}", format!("GPT 读取失败: {}", e).red()),
    }
}

fn cmd_dumpbrom(da: &mut DAXFlash, debug_mode: bool) -> Result<(), Box<dyn std::error::Error>> {
    da.preloader
        .run_dump_brom_payload("brom_dump.bin", debug_mode)
        .map_err(|e| format!("BROM 提取失败: {}", e))?;
    info!("{}", "BROM 已提取: brom_dump.bin".green());
    Ok(())
}

fn cmd_read(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err("用法: mtkclient read <分区> <文件>".into());
    }
    da.read_partition(&args[0], &args[1])
        .map_err(|e| format!("读取失败: {}", e))?;
    info!("{}", format!("{} -> {}", args[0], args[1]).green());
    Ok(())
}

fn cmd_write(
    da: &mut DAXFlash,
    args: &[String],
    verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err("用法: mtkclient write <分区> <文件>".into());
    }
    let result = if verify {
        da.write_partition_with_verify(&args[0], &args[1])
    } else {
        da.write_partition(&args[0], &args[1])
    };
    result.map_err(|e| format!("写入失败: {}", e))?;
    info!("{}", format!("{} <- {}", args[0], args[1]).green());
    Ok(())
}

fn cmd_erase(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient erase <分区>".into());
    }
    da.erase_partition(&args[0])
        .map_err(|e| format!("擦除失败: {}", e))?;
    info!("{}", format!("{} 已擦除", args[0]).green());
    Ok(())
}

fn cmd_vbmeta(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient vbmeta <模式> (0/1/2/3)".into());
    }
    let mode = args[0].parse::<u32>().map_err(|_| "无效模式")?;
    da.patch_vbmeta(mode)
        .map_err(|e| format!("修补失败: {}", e))?;
    info!("{}", "vbmeta 已修补".green());
    Ok(())
}

fn cmd_reset(da: &mut DAXFlash) {
    info!("已使用脑电波控制设备重启");
    da.close_device(true);
    info!("{}", "设备已重启".green());
}

fn cmd_unlock(da: &mut DAXFlash) -> Result<(), Box<dyn std::error::Error>> {
    da.unlock_bootloader()
        .map_err(|e| format!("解锁失败: {}", e))?;
    info!("{}", "Bootloader 已解锁".green());
    Ok(())
}

fn cmd_lock(da: &mut DAXFlash) -> Result<(), Box<dyn std::error::Error>> {
    da.lock_bootloader()
        .map_err(|e| format!("锁定失败: {}", e))?;
    info!("{}", "Bootloader 已锁定".green());
    Ok(())
}

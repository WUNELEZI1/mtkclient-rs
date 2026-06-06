use colored::Colorize;
use log::{error, info, warn};
use std::time::SystemTime;

use crate::connection::DeviceMode;
use crate::config::AppConfig;
use crate::da_partition::{generate_scatter_from_gpt, generate_scatter_shoujixia};
use crate::da_xflash::DAXFlash;
use crate::frp;
use crate::usb::UsbContext;

pub fn print_help() {
    println!("用法:");
    println!("  mtkclient-rs.exe <命令> [参数]");
    println!();
    println!("命令:");
    println!("  printgpt          打印 GPT 分区表");
    println!("  dump-preloader    提取 Preloader");
    println!("  dumpbrom          提取 BROM");
    println!("  r <分区> <文件>   读取分区");
    println!("  r gpt <目录>      保存 GPT 原始数据到目录");
    println!("  rl <目录>         读取全部分区到目录");
    println!("  w <分区> <文件>   写入分区");
    println!("  e <分区>          擦除分区");
    println!("  vbmeta <模式>     修补 vbmeta (0/1/2/3)");
    println!("  reset             重启设备");
    println!("  unlock            解锁 Bootloader");
    println!("  lock              锁定 Bootloader");
    println!("  frp               FRP OEM 解锁");
    println!("  print-scatter     打印 scatter 到屏幕并保存文件");
    println!("  enable-adb-on-da  在 DA 模式下开启 ADB");
    println!();
    println!("选项:");
    println!("  --preloader <文件>  指定 preloader 文件");
    println!("  --verify            写入后校验");
    println!("  --log <级别>        日志级别：1=INFO，2=DEBUG，3=TRACE");
    println!("  --patch-da          是否 patch DA（默认开启）");
}

/// 单命令执行入口
pub fn handle_command(
    da: &mut DAXFlash,
    _mode: &DeviceMode,
    app_config: &AppConfig,
    log_level: u8,
    _quiet_dump: bool,
    preloader_file: &str,
    _context: &UsbContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let is_brom = !da.preloader.is_preloader_mode;
    let mut auto_dumped_file: Option<String> = None;

    if is_brom {
        let cmd = app_config.command.as_deref().unwrap_or("");
        match cmd {
            "dumpbrom" => {
                cmd_dumpbrom(da, log_level)?;
                return Ok(());
            }
            "reset" => {
                cmd_reset(da)?;
                return Ok(());
            }
            _ => {}
        }

        if preloader_file.is_empty() {
            match da.preloader.get_target_config() {
                Ok(cfg) => info!("{}", cfg.format_info()),
                Err(e) => warn!("获取 target config 失败: {}", e),
            }

            da.preloader
                .bypass_security()
                .map_err(|e| format!("bypass_security 失败: {}", e))?;

            let data = da
                .preloader
                .dump_preloader_from_ram(false)
                .map_err(|e| format!("dump_preloader_ram 失败: {}", e))?;

            if !data.is_empty() {
                let filename = if let Some(info_idx) =
                    data.windows(16).position(|w| w == b"MTK_BLOADER_INFO")
                {
                    let filename_start = info_idx + 0x1B;
                    let filename_end = std::cmp::min(filename_start + 0x30, data.len());
                    let filename_bytes = &data[filename_start..filename_end];
                    let filename_len = filename_bytes
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(filename_bytes.len());
                    String::from_utf8_lossy(&filename_bytes[..filename_len]).to_string()
                } else {
                    "preloader_dumped.bin".to_string()
                };
                if !filename.is_empty() {
                    auto_dumped_file = Some(filename);
                    info!(
                        "Preloader 已提取: {} ({} 字节)",
                        auto_dumped_file.as_ref().unwrap(),
                        data.len()
                    );
                }
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

    // DA 已加载则跳过完整 upload_da，只做 reinit 复用会话
    if da.daext {
        info!("DA 已加载，复用会话");
        da.reinit().map_err(|e| format!("DA reinit 失败: {}", e))?;
    } else {
        da.upload_da()
            .map_err(|e| format!("DA 加载失败: {}", e))?;
    }

    if log_level >= 2 {
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

    execute_single_command(da, cmd, args, verify, log_level)?;

    Ok(())
}

/// 执行单个 DA 命令（不处理 Phase1/Phase2）
fn execute_single_command(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    verify: bool,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        "printgpt" => cmd_printgpt(da, log_level),
        "r" | "read" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let dir = args.get(1).ok_or("用法: mtkclient r gpt <目录>")?;
                cmd_read_gpt(da, dir, log_level)?;
            } else {
                cmd_read(da, args)?;
            }
        }
        "rl" | "readall" => {
            let dir = args.first().ok_or("用法: mtkclient rl <目录>")?;
            cmd_read_all(da, dir)?;
        }
        "w" | "write" => cmd_write(da, args, verify)?,
        "e" | "erase" => cmd_erase(da, args)?,
        "vbmeta" => cmd_vbmeta(da, args)?,
        "frp" => frp::frp_unlock(da)?,
        "reset" => cmd_reset(da)?,
        "unlock" => cmd_unlock(da)?,
        "lock" => cmd_lock(da)?,
        "print-scatter" => cmd_print_scatter(da, log_level)?,
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

fn cmd_printgpt(da: &mut DAXFlash, log_level: u8) {
    match da.read_gpt() {
        Ok(_) => {
            if let Ok(emmc_info) = da.get_emmc_info() {
                println!();
                println!("{}", " EMMC 信息 ".on_green().black());
                println!(
                    "  EMMC Boot1 Size: {}  {} MB",
                    format!("0x{:06X}", emmc_info.boot1_size).green(),
                    format!("({} MB)", emmc_info.boot1_size / 1024 / 1024).dimmed()
                );
                println!(
                    "  EMMC Boot2 Size: {}  {} MB",
                    format!("0x{:06X}", emmc_info.boot2_size).green(),
                    format!("({} MB)", emmc_info.boot2_size / 1024 / 1024).dimmed()
                );
            }

            if let Ok(data) = da.get_last_gpt_data() {
                print_gpt_table(data);
            }

            info!("{}", "GPT 读取成功".green());
            if log_level >= 3 {
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

fn print_gpt_table(data: &[u8]) {
    let gpt_info = match crate::da_partition::GptInfo::parse(data) {
        Ok(info) => info,
        Err(_) => return,
    };

    let revision = gpt_info.revision;
    let num_part_entries = gpt_info.num_part_entries;
    let part_entry_size = gpt_info.part_entry_size;

    println!();
    println!("{}", " GPT 分区表 ".on_green().black());
    println!("  修订版本:     {}", format!("0x{:08X}", revision).green());
    println!("  头部大小:     {} 字节", gpt_info.header_size);
    println!("  分区数量:     {}", format!("{}", num_part_entries).green());
    println!("  分区项大小:   {} 字节", part_entry_size);

    println!();
    println!(
        "{:<4} {:<20} {:<20} {:<20}",
        "序号".cyan(),
        "分区名称".cyan(),
        "起始地址".cyan(),
        "大小".cyan()
    );
    println!("{}", "─".repeat(66).dimmed());

    let partitions = gpt_info.partitions();
    for (count, entry) in partitions.iter().enumerate() {
        println!(
            "{:<4} {:<20} {:<20} {:<20}",
            format!("#{}", count + 1).dimmed(),
            entry.name.green(),
            format!("0x{:014X}", entry.start_addr).yellow(),
            format!("0x{:014X}", entry.size).yellow(),
        );
    }

    println!("{}", "─".repeat(66).dimmed());
    println!("  共 {} 个分区", format!("{}", partitions.len()).green());
    println!();
}

fn cmd_read_gpt(
    da: &mut DAXFlash,
    dir: &str,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;
    let output = format!("{}/gpt.bin", dir);

    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    let gpt_data = da.get_last_gpt_data()?;
    std::fs::write(&output, gpt_data).map_err(|e| format!("写入失败: {}", e))?;
    info!("{}", format!("GPT 已保存: {} ({} 字节)", output, gpt_data.len()).green());

    if log_level >= 2 {
        print_gpt_table(gpt_data);
    }

    Ok(())
}

fn cmd_read_all(da: &mut DAXFlash, dir: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;

    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    let gpt_data = da.get_last_gpt_data()?.clone();
    let gpt_info = crate::da_partition::GptInfo::parse(&gpt_data)?;

    for entry in gpt_info.iter_partitions() {
        let output = format!("{}/{}.img", dir, entry.name);
        info!(
            "  读取分区 {} (0x{:X} @ 0x{:X})",
            entry.name, entry.size, entry.start_addr
        );
        let data = da.readflash_data(entry.start_addr, entry.size)
            .map_err(|e| format!("读取 {} 失败: {}", entry.name, e))?;
        std::fs::write(&output, &data).map_err(|e| format!("写入失败: {}", e))?;
        info!("{}", format!("  {} -> {}", entry.name, output).green());
    }

    info!("{}", format!("全部分区已读取到: {}", dir).green());
    Ok(())
}

fn cmd_print_scatter(
    da: &mut DAXFlash,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    let gpt_data = da.get_last_gpt_data()?;
    let gpt_info = crate::da_partition::GptInfo::parse(gpt_data)?;

    println!();
    println!("{}", " Scatter 文件 (SP Flash Tool 格式) ".on_green().black());
    println!();

    let header = crate::da_partition::generate_scatter_header();
    for line in header.lines() {
        println!("{}", line);
    }

    for entry in gpt_info.iter_partitions() {
        println!("{} 0x{:X}", entry.name.to_uppercase().green(), entry.start_addr);
        println!("{{");
        println!("  is_upgradeable: 1");
        println!("  is_download: 1");
        println!("  is_reserved: 0");
        println!("  reserve: 0");
        println!("  operation: UPDATE");
        println!("  partition_size: 0x{:X}", entry.size);
        println!("}}");
        println!();
    }

    let scatter_file = "MT6768_Android_scatter.txt";
    generate_scatter_from_gpt(gpt_data, scatter_file)
        .map_err(|e| format!("scatter 生成失败: {}", e))?;
    info!("{}", format!("Scatter 已保存: {}", scatter_file).green());

    let shoujixia_file = "scatter_shoujixia.txt";
    if let Err(e) = generate_scatter_shoujixia(gpt_data, shoujixia_file, "MT6768") {
        info!("Warning: 刷机匣格式 scatter 生成失败: {}", e);
    } else {
        info!("{}", format!("刷机匣格式 scatter 已保存: {}", shoujixia_file).green());
    }

    if log_level >= 3 {
        let debug_file = format!(
            "scatter_debug_{}.txt",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        );
        generate_scatter_from_gpt(gpt_data, &debug_file)
            .map_err(|e| format!("调试 scatter 生成失败: {}", e))?;
    }

    Ok(())
}

fn cmd_dumpbrom(da: &mut DAXFlash, log_level: u8) -> Result<(), Box<dyn std::error::Error>> {
    da.preloader
        .run_dump_brom_payload("brom_dump.bin", log_level >= 2)
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

fn cmd_reset(da: &mut DAXFlash) -> Result<(), Box<dyn std::error::Error>> {
    match da.reset_device() {
        Ok(()) => {
            info!("{}", "设备已通过 DA 重启".green());
            Ok(())
        }
        Err(e) => {
            warn!("DA 重启失败，回退到 BROM jump_bl: {}", e);
            da.close_device(true);
            info!("{}", "设备已重启".green());
            Ok(())
        }
    }
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

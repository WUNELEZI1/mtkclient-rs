//! 用户命令入口
//!
//! 子模块：
//! - `gpt`   — printgpt / read_gpt / read_all / print_scatter 等分区表相关
//! - `io`    — read / write / erase / vbmeta / unlock / lock / reset 等 IO 命令
//! - `dump`  — dumpbrom / dumppreloader 等镜像提取
//!
//! 顶层入口：
//! - `print_help`       — 打印帮助信息
//! - `handle_command`   — 单命令执行入口（处理 Phase1 dump/bypass + Phase2 DA 命令）

pub mod dump;
pub mod gpt;
pub mod io;

use colored::Colorize;
use log::{error, info, warn};

use crate::config::AppConfig;
use crate::da_xflash::DAXFlash;
use crate::usb::UsbContext;

pub fn print_help() {
    println!("用法:");
    println!("  mtkclient-rs.exe <命令> [参数]");
    println!();
    println!("命令:");
    println!("  printgpt          打印 GPT 分区表");
    println!("  dumppreloader     提取 Preloader (Exploit)");
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
                dump::cmd_dumpbrom(da, log_level)?;
                return Ok(());
            }
            "dumppreloader" => {
                dump::cmd_dumppreloader(da, _context)?;
                return Ok(());
            }
            "reset" => {
                io::cmd_reset(da)?;
                return Ok(());
            }
            _ => {}
        }

        if preloader_file.is_empty() {
            // 关键顺序：先 dump_preloader_payload，再 bypass_security
            // 原因：
            // 1. mtkclient 的 run_dump_preloader 不调用 bypass_security，直接注入
            //    generic_preloader_dump_payload.bin，说明 dump_payload 内部已处理 bypass
            // 2. 如果先 bypass_security 注入 patcher，patcher 会改变 BROM 状态（ptr_send 位置含义变化、
            //    BROM 控制流被劫持），导致后续 dump_payload 注入后无法收到 ack（5秒超时）
            // 3. dump_payload 执行后设备状态恢复，可以正常进行 bypass_security 注入 patcher
            //
            // 注意：是否需要 bypass 由 needs_bypass 决定，但 dump_preloader 必须在 bypass 之前
            let (data, filename) = da
                .preloader
                .dump_preloader_payload(false, false, _context)
                .map_err(|e| format!("dump_preloader_payload 失败: {}", e))?;

            if !data.is_empty() {
                auto_dumped_file = Some(filename);
                info!(
                    "Preloader 已提取: {} ({} 字节)",
                    auto_dumped_file.as_ref().unwrap(),
                    data.len()
                );
            }

            // dump_preloader_payload 完成后，再判断是否需要 bypass_security 注入 patcher
            // 后续 upload_da 需要 patcher 来 bypass security
            let needs_bypass = match da.preloader.get_target_config() {
                Ok(cfg) => {
                    info!("{}", cfg.format_info());
                    if cfg.sbc || cfg.sla || cfg.daa {
                        info!("设备有安全保护，执行 Kamakiri2 bypass...");
                        true
                    } else {
                        info!(
                            "设备无安全保护（SBC/SLA/DAA 全关），跳过 Kamakiri2，直接进入 DA 模式"
                        );
                        false
                    }
                }
                Err(e) => {
                    warn!("获取 target config 失败: {}", e);
                    // 无法获取 config 时，保守执行 bypass
                    true
                }
            };

            if needs_bypass {
                da.preloader
                    .bypass_security(_context)
                    .map_err(|e| format!("bypass_security 失败: {}", e))?;
            }
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

    let effective_file = auto_dumped_file.as_deref().unwrap_or(preloader_file);
    info!("加载 EMI 数据: {}", effective_file);
    if let Err(e) = da.load_preloader_emi(effective_file) {
        // EMI 加载失败 = 致命错误（无 EMI 数据后续 upload_da 会失败），直接返回
        return Err(format!("EMI 加载失败: {}", e).into());
    }

    // DA 已加载则跳过完整 upload_da，只做 reinit 复用会话
    if da.daext {
        info!("DA 已加载，复用会话");
        da.reinit().map_err(|e| format!("DA reinit 失败: {}", e))?;
    } else {
        da.upload_da().map_err(|e| format!("DA 加载失败: {}", e))?;
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
pub fn execute_single_command(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    verify: bool,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        "printgpt" => gpt::cmd_printgpt(da, log_level),
        "r" | "read" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let dir = args.get(1).ok_or("用法: mtkclient r gpt <目录>")?;
                gpt::cmd_read_gpt(da, dir, log_level)?;
            } else {
                io::cmd_read(da, args)?;
            }
        }
        "rl" | "readall" => {
            let dir = args.first().ok_or("用法: mtkclient rl <目录>")?;
            gpt::cmd_read_all(da, dir)?;
        }
        "w" | "write" => io::cmd_write(da, args, verify)?,
        "e" | "erase" => io::cmd_erase(da, args)?,
        "vbmeta" => io::cmd_vbmeta(da, args)?,
        "frp" => crate::security::frp::frp_unlock(da)?,
        "reset" => io::cmd_reset(da)?,
        "unlock" => io::cmd_unlock(da)?,
        "lock" => io::cmd_lock(da)?,
        "print-scatter" => gpt::cmd_print_scatter(da, log_level)?,
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

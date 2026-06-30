//! 用户命令入口
//!
//! 子模块：
//! - `分区表` — printgpt / read_gpt / read_all / print_scatter 等分区表相关
//! - `输入输出` — read / write / erase / vbmeta / unlock / lock / reset 等 IO 命令
//! - `转储`  — dumppreloader 等镜像提取
//!
//! 顶层入口：
//! - `print_help`       — 打印帮助信息
//! - `handle_command`   — 单命令执行入口（处理 Phase1 dump/bypass + Phase2 DA 命令）

#[path = "转储.rs"]
pub mod 转储;
#[path = "分区表.rs"]
pub mod 分区表;
#[path = "输入输出.rs"]
pub mod 输入输出;

use colored::Colorize;
use log::{error, info, warn};

use crate::config::AppConfig;
use crate::DA扩展::DAXFlash;
use crate::USB通信::USB上下文;

pub fn print_help() {
    println!("用法:");
    println!("  mtkclient-rs.exe <命令> [参数]");
    println!();
    println!("命令:");
    println!("  printgpt            打印 GPT 分区表 + EMMC 信息 + 生成 scatter");
    println!("  dump                从 RAM 提取 Preloader (Exploit)");
    println!("  r <part> <file>     读取分区到文件");
    println!("  r gpt <dir>          保存 GPT 原始数据到目录");
    println!("  rl <dir>             读取全部分区到目录 (支持 --skip)");
    println!("  w <part> <file>     写入文件到分区");
    println!("  wl <dir>             从目录恢复全部分区 (.bin/.img)");
    println!("  e <part>             擦除分区");
    println!("  vbmeta <mode>        修补 vbmeta (0/1/2/3)");
    println!("  reset                重启设备");
    println!("  unlock               解锁 Bootloader");
    println!("  lock                 锁定 Bootloader");
    println!("  frp                  FRP OEM 解锁");
    println!("  scatter              打印 scatter 到屏幕并保存文件");
    println!("  adb                  在 DA 模式下开启 ADB");
    println!();
    println!("选项:");
    println!("  --preloader <file>  指定 preloader 文件");
    println!("  --verify            写入后校验");
    println!("  --log <level>       日志级别：1=INFO，2=DEBUG，3=TRACE");
    println!("  --patch-da          是否 patch DA（默认 true）");
    println!("  --mode <mode>       工作模式：brom（默认）/ preloader / auto");
    println!("  --da-x-speed <1-3>  DA 加载速度级别");
    println!("  --skip <parts>      rl 跳过的分区（逗号分隔）");
    println!("  --quiet             静默模式");
    println!("  --quiet-dump        静默 dump（不打印进度条）");
    println!("  --usb-log           启用 USB 通信追踪日志");
}

/// 单命令执行入口
pub fn handle_command(
    da: &mut DAXFlash,
    app_config: &AppConfig,
    log_level: u8,
    _quiet_dump: bool,
    preloader_file: &str,
    _context: &USB上下文,
) -> Result<(), Box<dyn std::error::Error>> {
    let is_brom = !da.preloader.is_preloader_mode;
    let mut auto_dumped_file: Option<String> = None;

    if is_brom {
        let cmd = app_config.command.as_deref().unwrap_or("");
        match cmd {
            "dump" => {
                转储::cmd_dumppreloader(da, _context)?;
                return Ok(());
            }
            "reset" => {
                输入输出::cmd_reset(da)?;
                return Ok(());
            }
            _ => {}
        }

        // 1. 获取 target config 判断是否需要 bypass
        let needs_bypass = match da.preloader.get_target_config() {
            Ok(cfg) => {
                info!("{}", cfg.format_info());
                if cfg.needs_bypass() {
                    info!("设备有安全保护，执行 Kamakiri2 bypass...");
                    true
                } else {
                    info!(
                        "设备无安全保护（SBC/SLA/DAA/MemRead 全关），跳过 Kamakiri2，直接进入 DA 模式"
                    );
                    false
                }
            }
            Err(e) => {
                warn!("获取 target config 失败: {}", e);
                warn!("假设 bypass 已处理，直接继续...");
                false
            }
        };

        if needs_bypass {
            da.preloader
                .bypass_security(_context)
                .map_err(|e| format!("bypass_security 失败: {}", e))?;
        }

        // 2. 如果没有指定 preloader 文件，通过非破坏性 read32 从 RAM 提取
        if preloader_file.is_empty() {
            info!("未指定 --preloader，自动从 RAM 提取 preloader...");
            match da.preloader.dump_preloader_via_brom_read() {
                Ok((data, filename)) => {
                    auto_dumped_file = Some(filename);
                    info!(
                        "Preloader 已自动提取: {} ({} 字节)",
                        auto_dumped_file.as_ref().unwrap(),
                        data.len()
                    );
                }
                Err(e) => {
                    return Err(format!(
                        "自动提取 preloader 失败: {}。请手动运行 'dump' 命令获取文件，\n\
                         然后使用 --preloader <文件> 参数。",
                        e
                    )
                    .into());
                }
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

    // Preloader 模式下 DRAM 已由 preloader 初始化，EMI 数据可选
    if !preloader_file.is_empty() {
        info!("加载 EMI 数据: {}", preloader_file);
        if let Err(e) = da.load_preloader_emi(preloader_file) {
            return Err(format!("EMI 加载失败: {}", e).into());
        }
    } else if let Some(ref f) = auto_dumped_file {
        info!("加载 EMI 数据: {}", f);
        if let Err(e) = da.load_preloader_emi(f) {
            return Err(format!("EMI 加载失败: {}", e).into());
        }
    } else if da.preloader.is_preloader_mode {
        info!("Preloader 模式：跳过 EMI 加载（DRAM 已由 preloader 初始化）");
    } else {
        return Err("未找到 preloader 文件，且自动提取失败".into());
    }

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

    if cmd == "adb" {
        da.enable_adb_and_reboot()?;
        info!("{}", "ADB 已启用，设备正在重启进入系统".green());
        return Ok(());
    }

    execute_single_command(da, cmd, args, verify, log_level, app_config)?;

    Ok(())
}

/// 执行单个 DA 命令（不处理 Phase1/Phase2）
pub fn execute_single_command(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    verify: bool,
    log_level: u8,
    app_config: &AppConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        "printgpt" => 分区表::cmd_printgpt(da, log_level),
        "r" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let dir = args.get(1).ok_or("用法: mtkclient r gpt <dir>")?;
                分区表::cmd_read_gpt(da, dir, log_level)?;
            } else {
                输入输出::cmd_read(da, args)?;
            }
        }
        "rl" => {
            let dir = args.first().ok_or("用法: mtkclient rl <dir>")?;
            分区表::cmd_read_all(da, dir, app_config)?;
        }
        "wl" => {
            let dir = args.first().ok_or("用法: mtkclient wl <dir>")?;
            分区表::cmd_write_all(da, dir, verify)?;
        }
        "w" => 输入输出::cmd_write(da, args, verify)?,
        "e" => 输入输出::cmd_erase(da, args)?,
        "vbmeta" => 输入输出::cmd_vbmeta(da, args)?,
        "frp" => crate::安全::frp::frp_unlock(da)?,
        "reset" => 输入输出::cmd_reset(da)?,
        "unlock" => 输入输出::cmd_unlock(da)?,
        "lock" => 输入输出::cmd_lock(da)?,
        "scatter" => 分区表::cmd_print_scatter(da, log_level)?,
        "adb" => {
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

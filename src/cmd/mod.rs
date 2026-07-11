//! 用户命令入口
//!
//! 子模块：
//! - `cli`            — 命令行参数解析
//! - `dump`           — dumppreloader 等镜像提取
//! - `io`             — r/w/e/reboot/slot 等 IO 命令
//! - `partition_table` — printgpt / rl / wl 等分区表相关
//!
//! 顶层入口：
//! - `print_help`       — 打印帮助信息
//! - `handle_command`   — 单命令执行入口（处理 Phase1 dump/bypass + Phase2 DA 命令）

pub mod cli;
pub mod buildprop;
pub mod detect;
pub mod dump;
pub mod preloader_boot_mode;
pub mod io;
pub mod multi;
pub mod partition_table;

use colored::Colorize;
use log::{error, info, warn};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;
use crate::usb::USB上下文;

pub fn print_help() {
    use colored::Colorize;

    println!("{}", "mtkclient-rs — MTK 设备底层刷机工具".bold());
    println!("用法:");
    println!("  mtkclient-rs.exe [全局选项] <命令> [参数...]");
    println!();
    println!("{}", "全局选项:".bold());
    println!("  --mode <mode>         工作模式: brom（默认，完整BROM流程+Kamakiri2 bypass）");
    println!("                                preloader（直接连接Preloader VCOM，跳过bypass）");
    println!("  --preloader <file>    指定 preloader 文件（默认从设备dump）");
    println!("  --da <file>           指定 DA loader 文件");
    println!("  --da2 <file>          指定 DA2 文件");
    println!("  --da_x_speed <1-3>    DA 加载速度: 1=完整 2=快速 3=极速+USB高速重连");
    println!("  --log <level>         日志级别: 1=INFO 2=DEBUG 3=TRACE（含原始hex dump）");
    println!("  --quiet               静默模式（跳过info输出）");
    println!("  --quiet_dump          静默dump（不打印进度条）");
    println!("  --usb_log             启用 USB 通信追踪日志（输出到 usb_debug.log）");
    println!();
    println!("{}", "分区操作:".bold());
    println!("  printgpt                     打印 GPT 分区表 + eMMC 信息 + super动态分区树");
    println!("  r <分区名> <文件>             读取分区到文件");
    println!("       r boot boot.img          读取 boot 分区");
    println!("       r boot1 boot1.bin        读取 eMMC boot1（特殊分区）");
    println!("       r super --dp system_b system_b.img   读取 super 内动态分区");
    println!("       r gpt <目录>              读取 GPT 原始数据到目录");
    println!("  rl <目录> [--skip <分区,...>]  读取全部分区到目录");
    println!("  w <分区名> <文件>             写入文件到分区");
    println!("  wl <目录>                     从目录恢复全部分区（匹配 .bin/.img）");
    println!("  e <分区名>                    擦除分区");
    println!();
    println!("{}", "信息查询:".bold());
println!("  zyb get_build_prop [分区名]   读取 build.prop（默认 system 分区）");
println!("  slot show/a/b                 显示/切换 A/B 槽位");
println!();
    println!("{}", "安全与解锁:".bold());
    println!("  zyb seccfg unlock             解锁 Bootloader（seccfg V3/V4）");
    println!("  zyb seccfg lock               锁定 Bootloader（seccfg V3/V4）");
    println!("  zyb oem unlock                FRP OEM 解锁（修改 frp 分区标志位）");
    println!("  zyb oem lock                  FRP OEM 回锁（修改 frp 分区标志位）");
    println!("  zyb vbmeta <mode>             修补 vbmeta（0=关闭验证 1=启用 2/3=保留）");
    println!("  zyb erase_data                清除用户数据 + metadata（恢复出厂设置）");
    println!("  frp                           FRP OEM 解锁（zyb oem unlock 的别名）");
    println!();
    println!("{}", "重启控制:".bold());
    println!("  reboot                        正常重启到系统");
    println!("  reboot fastboot               重启到 Bootloader（lk fastboot）");
    println!("  reboot recovery               重启到 Recovery");
    println!("  reboot fastbootd              重启到 fastbootd（userspace fastboot）");
    println!("  reboot meta                   重启到 META 模式");
    println!("  reboot fastboot --via para    通过 para 分区设置 boot_mode=5（DA模式）");
    println!("  reboot fastboot --via da      通过 DA SHUTDOWN bootmode=2 直接重启");
    println!("  reboot fastboot --via xml     通过 XML DA SET-BOOT-MODE 重启（新平台）");
    println!();
    println!("{}", "其他:".bold());
    println!("  dumppreloader                 从 RAM 提取 Preloader（BROM Exploit）");
    println!("  adb                           在 DA 模式下开启 ADB 并重启");
    println!("  fs_shell                      交互式浏览 super 分区文件系统");
    println!("  multi \"<cmd1>;<cmd2>;...\"    一次 DA 会话执行多个命令");
    println!();
    println!("{}", "常用示例:".dimmed());
    println!("  cargo run -- --mode brom printgpt");
    println!("  cargo run -- --mode brom r boot_a boot_a.img");
    println!("  cargo run -- --mode brom r super --dp system_b system_b.img");
    println!("  cargo run -- --mode brom zyb get_build_prop");
    println!("  cargo run -- --mode brom zyb erase_data");
    println!("  cargo run -- --mode brom w boot_a boot_patched.img");
    println!("  cargo run -- --mode brom zyb seccfg unlock");
    println!("  cargo run -- --mode preloader reboot fastboot");
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

    let cmd = app_config.command.as_deref().unwrap_or("");
    let has_active_read_resume = cmd == "r" && active_read_resume_exists(&app_config.cmd_args);

    // DA 会话复用时跳过 preloader dump / bypass / EMI 加载（DA 仍在运行）
    if !da.daext && is_brom {
        match cmd {
            "dumppreloader" => {
                dump::cmd_dumppreloader(da, _context)?;
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
                        "自动提取 preloader 失败: {}。请手动运行 'dumppreloader' 命令获取文件，\n\
                         然后使用 --preloader <文件> 参数。",
                        e
                    )
                    .into());
                }
            }
        }
    }

    // Preloader 模式下 reboot fastboot: 直接通过串口发送 Pattern 协议
    // 对齐 MABT (MTK Auth Bypass Tool) 的 BootAsFASTBOOT 行为
    // 不需要 DA 加载，直接走 Preloader COM 口通信
    if !is_brom && cmd == "reboot" {
        if app_config.cmd_args.first().map(|s| s.as_str()) == Some("fastboot") {
            info!("Preloader 模式直接发送 BootAsFASTBOOT Pattern...");
            match preloader_boot_mode::send_boot_pattern(
                &mut *da.preloader.device,
                preloader_boot_mode::BootMode::Fastboot,
            ) {
                Ok(_) => {
                    info!("设备已通过 Preloader 模式重启到 Bootloader");
                    crate::connection::reset_session();
                    return Ok(());
                }
                Err(e) => {
                    warn!("Preloader BootAsFASTBOOT 失败: {}，回退到 DA 流程", e);
                }
            }
        }
    }

    // Preloader 模式下 DRAM 已由 preloader 初始化，EMI 数据可选
    let effective_preloader = if !preloader_file.is_empty() {
        Some(preloader_file.to_string())
    } else {
        auto_dumped_file.clone()
    };

    if let Some(ref f) = effective_preloader {
        info!("加载 EMI 数据: {}", f);
        if let Err(e) = da.load_preloader_emi(f) {
            return Err(format!("EMI 加载失败: {}", e).into());
        }
        da.preloader_path = Some(f.clone());
    } else if da.preloader.is_preloader_mode {
        info!("Preloader 模式：跳过 EMI 加载（DRAM 已由 preloader 初始化）");
    } else {
        return Err("未找到 preloader 文件，且自动提取失败".into());
    }

    // DA 会话检测与恢复（对齐刷机匣"初始化DA模式"机制）
    if da.daext {
        info!("DA 已加载，检测会话有效性...");
        if has_active_read_resume {
            info!("[DA_SESSION] 检测到活跃读取续传，跳过 heartbeat/reinit，直接续接数据流");
        } else if da.check_da_session() {
            info!("DA 会话有效，执行 reinit...");
            da.reinit().map_err(|e| format!("DA reinit 失败: {}", e))?;
        } else {
            return Err(
                "DA 会话已失效，已清除 .state。请让设备重新进入 BROM 后重新执行命令，避免在半状态下继续重载 DA。"
                    .into(),
            );
        }
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

    // multi 和 adb 都在 DA 会话已建立后执行，不需要走 execute_single_command
    if cmd == "adb" {
        da.enable_adb_and_reboot()?;
        info!("{}", "ADB 已启用，设备正在重启进入系统".green());
        return Ok(());
    }

    if cmd == "multi" {
        multi::cmd_multi(da, args, app_config, log_level)?;
        return Ok(());
    }

    execute_single_command(da, cmd, args, verify, log_level, app_config).map_err(|e| {
        // 命令执行失败时重置 .state，避免下次复用已失效的会话
        // 但用户主动取消时不重置（Ctrl+C 场景，DA 仍存活）
        if !crate::cancel::requested() && !crate::cancel::force_requested() {
            warn!("[DA_SESSION] 命令执行失败，重置会话状态");
            crate::connection::reset_session();
            // 清理可能残留的 resume 文件，避免下次启动死循环
            if cmd == "r" {
                if let Some(output) = app_config.cmd_args.get(1) {
                    let resume_path = format!("{}.resume", output);
                    let _ = std::fs::remove_file(&resume_path);
                }
            }
        }
        e
    })?;

    Ok(())
}

fn active_read_resume_exists(args: &[String]) -> bool {
    let Some(output) = args.get(1) else {
        return false;
    };
    let path = format!("{}.resume", output);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    if !content.lines().any(|line| line == "active_read=true") {
        return false;
    }
    let Some(written) = content
        .lines()
        .find_map(|line| line.strip_prefix("written="))
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return false;
    };
    match std::fs::metadata(output) {
        Ok(metadata) => {
            let file_size = metadata.len();
            // BufWriter 64MB 缓冲：Ctrl+C 时文件可能比 resume 记录小
            // 允许 file_size <= written（文件已刷盘的数据 <= writer 线程已接收的数据）
            // 最大容差 128MB（2×BufWriter 缓冲 + 安全余量）
            file_size <= written && written.saturating_sub(file_size) < 128 * 1024 * 1024
        }
        Err(_) => false,
    }
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
        "printgpt" => partition_table::cmd_printgpt(da, log_level),
        "dumppreloader" => {
            info!("dumppreloader 命令请在 BROM 模式下直接执行，无需进入 DA 模式");
        }
        "r" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let dir = args.get(1).ok_or("用法: mtkclient r gpt <dir>")?;
                partition_table::cmd_read_gpt(da, dir, log_level)?;
            } else {
                io::cmd_read(da, args)?;
            }
        }
        "rl" => {
            let dir = args.first().ok_or("用法: mtkclient rl <dir>")?;
            partition_table::cmd_read_all(da, dir, app_config)?;
        }
        "wl" => {
            let dir = args.first().ok_or("用法: mtkclient wl <dir>")?;
            partition_table::cmd_write_all(da, dir, verify)?;
        }
        "w" => io::cmd_write(da, args, verify)?,
        "e" => io::cmd_erase(da, args)?,
        "zyb" => handle_zyb_command(da, args)?,
        "frp" => crate::security::frp::frp_unlock(da)?,
        "reboot" => io::cmd_reboot(da, args, !da.preloader.is_preloader_mode)?,
        "slot" => io::cmd_slot(da, args)?,
        "adb" => {
            da.enable_adb_and_reboot()?;
            info!("{}", "ADB 已启用，设备正在重启进入系统".green());
        }
        "fs_shell" => {
            // 读 GPT 获取 super 分区地址
            da.read_gpt().map_err(|e| format!("读取 GPT 失败: {}", e))?;
            let gpt_data = da.last_gpt_data.as_ref()
                .ok_or("GPT 数据不可用")?;
            let gpt_info = crate::partition::gpt::GptInfo::parse(gpt_data)
                .map_err(|e| format!("解析 GPT 失败: {}", e))?;

            let super_entry = gpt_info.find_partition("super")
                .or_else(|| gpt_info.find_partition("super_b"))
                .ok_or("GPT 中未找到 super 分区")?;
            let super_addr = super_entry.start_addr;

            info!("super 分区地址: 0x{:08X}", super_addr);

            // 构造 read_fn 闭包
            let da_ref = &mut *da;
            let mut read_fn = |offset: u64, len: u64| -> Result<Vec<u8>, String> {
                da_ref.readflash_data_ex(super_addr + offset, len, 8)
            };

            crate::partition::explorer::run_explorer(&mut read_fn)?;
        }
        _ => {
            error!("{}", format!("未知命令: {}", cmd).red());
            print_help();
            return Err(format!("未知命令: {}", cmd).into());
        }
    }

    Ok(())
}

/// 处理 zyb 子命令
fn handle_zyb_command(
    da: &mut DAXFlash,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient zyb <subcmd> [args]".into());
    }

    let subcmd = args[0].as_str();
    let sub_args = &args[1..];

    match subcmd {
        "vbmeta" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb vbmeta <mode> (0/1/2/3)".into());
            }
            let mode = sub_args[0].parse::<u32>().map_err(|_| "无效模式")?;
            crate::security::vbmeta::vbmeta_disable(da, mode)
                .map_err(|e| format!("修补失败: {}", e))?;
            info!("{}", "vbmeta 已修补".green());
        }
        "seccfg" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb seccfg unlock/lock".into());
            }
            match sub_args[0].as_str() {
                "unlock" => {
                    da.unlock_bootloader()
                        .map_err(|e| format!("解锁失败: {}", e))?;
                    info!("{}", "Bootloader 已解锁".green());
                }
                "lock" => {
                    da.lock_bootloader()
                        .map_err(|e| format!("锁定失败: {}", e))?;
                    info!("{}", "Bootloader 已锁定".green());
                }
                _ => return Err("用法: mtkclient zyb seccfg unlock/lock".into()),
            }
        }
        "oem" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb oem unlock/lock".into());
            }
            match sub_args[0].as_str() {
                "unlock" => {
                    crate::security::frp::frp_unlock(da)
                        .map_err(|e| format!("FRP OEM 解锁失败: {}", e))?;
                    info!("{}", "FRP OEM 已解锁".green());
                }
                "lock" => {
                    crate::security::frp::frp_lock(da)
                        .map_err(|e| format!("FRP OEM 锁定失败: {}", e))?;
                    info!("{}", "FRP OEM 已锁定".green());
                }
                _ => return Err("用法: mtkclient zyb oem unlock/lock".into()),
            }
        }
        "get_build_prop" => {
            buildprop::cmd_buildprop(da, sub_args)?;
        }
        "erase_data" => {
            io::cmd_erase_data(da)?;
        }
        _ => return Err(format!("未知 zyb 子命令: {}", subcmd).into()),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_read_resume_detects_sidecar_file() {
        let output =
            std::env::temp_dir().join(format!("cmd_active_resume_{}.img", std::process::id()));
        let output = output.to_string_lossy().to_string();

        // written=4096, file=4096 → 精确匹配，应该检测到
        std::fs::write(
            format!("{}.resume", output),
            "active_read=true\nwritten=4096\n",
        )
        .unwrap();
        std::fs::write(&output, vec![0u8; 4096]).unwrap();
        assert!(active_read_resume_exists(&[
            "boot_b".to_string(),
            output.clone()
        ]));

        // written=8192, file=4096 → BufWriter 未刷完，应该检测到（放宽校验）
        std::fs::write(
            format!("{}.resume", output),
            "active_read=true\nwritten=8192\n",
        )
        .unwrap();
        assert!(active_read_resume_exists(&[
            "boot_b".to_string(),
            output.clone()
        ]));

        // written=200MB, file=0 → 差距 200MB > 128MB 容差，应该不检测
        std::fs::write(
            format!("{}.resume", output),
            format!("active_read=true\nwritten={}\n", 200 * 1024 * 1024),
        )
        .unwrap();
        std::fs::write(&output, vec![0u8; 0]).unwrap();
        assert!(!active_read_resume_exists(&[
            "boot_b".to_string(),
            output.clone()
        ]));

        let _ = std::fs::remove_file(&output);
        let _ = std::fs::remove_file(format!("{}.resume", output));
    }
}

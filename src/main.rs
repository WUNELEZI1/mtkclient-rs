#![allow(dead_code)]

use clap::Parser;
use colored::Colorize;
use log::{error, info, trace, warn};

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
    fn SetConsoleCP(wCodePageID: u32) -> i32;
}

mod cli;
#[path = "命令/模块.rs"]
mod 命令;
mod config;
#[path = "连接管理/模块.rs"]
mod 连接管理;
#[path = "DA分区/模块.rs"]
mod DA分区;
#[path = "DA扩展/模块.rs"]
mod DA扩展;
#[path = "DA扩展命令/模块.rs"]
mod DA扩展命令;
#[path = "DA加载/模块.rs"]
mod DA加载;
#[path = "漏洞利用/模块.rs"]
mod 漏洞利用;
mod paths;
#[path = "预加载器/模块.rs"]
mod 预加载器;
#[path = "安全/模块.rs"]
mod 安全;
#[path = "USB通信/模块.rs"]
mod USB通信;

use 连接管理::ConnectionManager;
use USB通信::USB上下文;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "windows")]
    unsafe {
        SetConsoleCP(65001);
        SetConsoleOutputCP(65001);
    }

    // 输出版本号（每次代码更新 cargo.toml version +0.0.1）
    println!(
        "{} {}",
        "mtkclient-rs".cyan().bold(),
        format!("v{}", env!("CARGO_PKG_VERSION")).yellow().bold()
    );
    println!(
        "{}",
        "Copyright (c) wunelezi & trae | Licensed under GPL-3.0"
            .dimmed()
    );
    println!();

    let raw_args: Vec<String> = std::env::args().collect();
    let is_help = raw_args.iter().any(|a| a == "-h" || a == "--help");
    if is_help {
        命令::print_help();
        return Ok(());
    }

    let cli = cli::Cli::parse();

    // Windows: 驱动安装 (pnputil / wdi-rs) 必须管理员，提前提权
    // 用环境变量 MTKCLIENT_ELEVATED 标记避免子进程重复提权造成死循环
    #[cfg(target_os = "windows")]
    {
        if !cli.no_elevate
            && !连接管理::driver::is_admin()
            && std::env::var_os("MTKCLIENT_ELEVATED").is_none()
        {
            // 提示用户（UAC 弹窗会覆盖这个）
            eprintln!("[MAIN] 需要管理员权限以安装 WinUSB 驱动，正在请求提权...");
            if let Err(e) = 连接管理::driver::restart_as_admin() {
                eprintln!("[MAIN] 提权失败: {}", e);
                eprintln!(
                    "[MAIN] 请右键以管理员身份运行本程序，或加 --检测管理员权限 跳过（将无法切换 WinUSB）"
                );
                return Err(e.into());
            }
            // restart_as_admin 内部 std::process::exit(0)，不会回到这里
        }
    }

    let app_config = config::AppConfig::from_cli(&cli);

    USB通信::设置USB日志开关(cli.usb_log);

    // --quiet-dump: 抑制 USB 读取日志和进度条
    if cli.quiet_dump {
        USB通信::设置USB读取静默(true);
    }

    let log_level = if cli.quiet {
        log::LevelFilter::Error // --quiet: 只输出 ERROR
    } else {
        app_config.log_level
    };

    env_logger::builder()
        .filter_level(log_level)
        .filter_module("mtkclient_rs", log_level) // 明确指定本 crate 的日志级别
        .format(|buf, record| {
            use std::io::Write;
            let level = match record.level() {
                log::Level::Error => "ERROR",
                log::Level::Warn => "WARN ",
                log::Level::Info => "INFO ",
                log::Level::Debug => "DEBUG",
                log::Level::Trace => "TRACE",
            };
            // 使用 Windows API 获取本地时间
            #[cfg(target_os = "windows")]
            let timestamp = {
                #[repr(C)]
                struct SystemTime {
                    w_year: u16,
                    w_month: u16,
                    w_day_of_week: u16,
                    w_day: u16,
                    w_hour: u16,
                    w_minute: u16,
                    w_second: u16,
                    w_milliseconds: u16,
                }
                unsafe extern "system" {
                    fn GetLocalTime(lpSystemTime: *mut SystemTime);
                }
                let mut st = SystemTime {
                    w_year: 0,
                    w_month: 0,
                    w_day_of_week: 0,
                    w_day: 0,
                    w_hour: 0,
                    w_minute: 0,
                    w_second: 0,
                    w_milliseconds: 0,
                };
                unsafe {
                    GetLocalTime(&mut st);
                }
                format!(
                    "{:04}/{:02}/{:02} {:02}:{:02}:{:02}.{:03}",
                    st.w_year,
                    st.w_month,
                    st.w_day,
                    st.w_hour,
                    st.w_minute,
                    st.w_second,
                    st.w_milliseconds
                )
            };
            #[cfg(not(target_os = "windows"))]
            let timestamp = {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                let secs = now.as_secs();
                let millis = now.subsec_millis();
                let hours = (secs % 86400) / 3600;
                let minutes = (secs % 3600) / 60;
                let seconds = secs % 60;
                format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
            };
            writeln!(buf, "[{}] [{}] {}", timestamp, level, record.args())
        })
        .init();

    let cmd = app_config.command.as_deref().unwrap_or("");

    if cmd.is_empty() {
        命令::print_help();
        return Ok(());
    }

    let usb_context = USB上下文::新建().inspect_err(|e| {
        error!("{}", e);
    })?;

    // COM 口前置握手 + libusb 后续通信
    let mut conn_mgr = ConnectionManager::new();

    // 工作模式：brom / preloader / auto
    let 工作模式 = app_config.工作模式;
    info!("[MAIN] 工作模式: {:?}", 工作模式);

    // === DA 会话复用检查 ===
    // 如果 .state 存在且设备已经处于 DA 模式（PID=0x2000），
    // 可以跳过 BROM→DA 流程，直接连接 DA 模式设备。
    // 流程：
    //   1. libusb 枚举当前 USB 设备，找到第一个 MediaTek 设备
    //   2. 读 .state 文件，检查 da_loaded 标志和 VID/PID 匹配
    //   3. 如果复用条件满足 → connect_to_da_mode（直接连接 PID=0x2000）
    //   4. 否则 → 走正常的 smart_init 流程
    let da_session_reused =
        if let Some((current_vid, current_pid, _dev_type)) = USB通信::获取第一个联发科VIDPID() {
            if current_pid == 0x0003 {
                // 核心修复：如果当前设备是 BROM (0003)，说明设备已重启，必须重置 DA 会话
                trace!("[session] 检测到 BROM 设备，强制重置旧的 DA 会话状态");
                crate::连接管理::reset_session();
                false
            } else if crate::连接管理::try_reuse_da_session(current_vid, current_pid) {
                info!(
                    "{}",
                    "[DA_SESSION] 检测到现有 DA 会话，尝试复用..."
                        .green()
                        .bold()
                );
                true
            } else {
                false
            }
        } else {
            false
        };

    let (mut preloader, _mode) = if da_session_reused {
        match conn_mgr.connect_to_da_mode(&usb_context) {
            Ok(pair) => pair,
            Err(e) => {
                warn!("[DA_SESSION] DA 会话复用失败: {}，回退到正常流程", e);
                crate::连接管理::reset_session();
                conn_mgr.smart_init(&usb_context, 工作模式)?
            }
        }
    } else {
        conn_mgr.smart_init(&usb_context, 工作模式)?
    };

    match 工作模式 {
        crate::config::工作模式::Preloader => {
            info!("{}", "连接成功 (Preloader 模式)".green().bold());
        }
        _ => {
            info!("{}", "连接成功 (BROM 模式)".green().bold());
        }
    }

    // preloader_path 决定后续策略：
    //   - Some(path)：使用用户指定的文件作为 EMI 数据源，跳过 dump + bypass
    //   - None：强制从设备 dump preloader 一次（覆盖同名文件），然后按需 bypass
    //
    // 注意：dump + load + bypass + upload_da 全部下放到 命令::handle_command 统一处理，
    // 避免在 main.rs 与 handle_command 双重执行（之前会 dump 两次）。
    let final_preloader_path = app_config.preloader_path.clone().unwrap_or_default();
    if !final_preloader_path.is_empty() {
        info!("使用指定的 preloader 文件: {}", final_preloader_path);
    } else {
        info!(
            "{}",
            "未指定 --preloader，将强制从设备 dump 并覆盖同名文件".yellow()
        );
    }

    let mut da = DA扩展::DAXFlash::new(&mut preloader);
    da.patch_da = cli.patch_da;
    da.da_x_speed = app_config.da_x_speed;

    // dump + load + bypass + upload_da 全部由 handle_command 内部完成
    // 这里不再调用 dump_preloader_payload / load_preloader_emi / upload_da
    命令::handle_command(
        &mut da,
        &app_config,
        cli.log_level,
        cli.quiet_dump,
        &final_preloader_path,
        &usb_context,
    )?;

    Ok(())
}

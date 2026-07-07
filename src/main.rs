#![allow(dead_code)]

use clap::Parser;
use colored::Colorize;
use log::{error, info, warn};
use std::io::Write;
use std::sync::Mutex;

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
    fn SetConsoleCP(wCodePageID: u32) -> i32;
}

/// 获取本地时间戳字符串 [YYYY/MM/DD HH:MM:SS.mmm]
fn get_local_timestamp() -> String {
    #[cfg(target_os = "windows")]
    {
        #[repr(C)]
        struct SystemTime {
            w_year: u16, w_month: u16, w_day_of_week: u16,
            w_day: u16, w_hour: u16, w_minute: u16,
            w_second: u16, w_milliseconds: u16,
        }
        unsafe extern "system" {
            fn GetLocalTime(lpSystemTime: *mut SystemTime);
        }
        let mut st = SystemTime {
            w_year: 0, w_month: 0, w_day_of_week: 0,
            w_day: 0, w_hour: 0, w_minute: 0,
            w_second: 0, w_milliseconds: 0,
        };
        unsafe { GetLocalTime(&mut st); }
        format!(
            "{:04}/{:02}/{:02} {:02}:{:02}:{:02}.{:03}",
            st.w_year, st.w_month, st.w_day,
            st.w_hour, st.w_minute, st.w_second, st.w_milliseconds
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();
        let millis = now.subsec_millis();
        let hours = (secs % 86400) / 3600;
        let minutes = (secs % 3600) / 60;
        let seconds = secs % 60;
        format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
    }
}

/// TeeLogger：同时输出到终端（env_logger）和文件（usb_debug.log）
struct TeeLogger {
    terminal: env_logger::Logger,
    file: Mutex<std::fs::File>,
}

impl log::Log for TeeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.terminal.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        // 1. 终端输出（保持 env_logger 原有格式）
        self.terminal.log(record);

        // 2. 文件输出（简化格式）
        let line = format!(
            "[{}] [{}] [{}] {}\n",
            get_local_timestamp(),
            record.level(),
            record.target(),
            record.args()
        );
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        self.terminal.flush();
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

mod cancel;
#[path = "cmd/mod.rs"]
mod cmd;
#[path = "connection/mod.rs"]
mod connection;
#[path = "da/mod.rs"]
mod da;
#[path = "exploit/mod.rs"]
mod exploit;
#[path = "partition/mod.rs"]
mod partition;
#[path = "preloader/mod.rs"]
mod preloader;
#[path = "security/mod.rs"]
mod security;
mod system;
#[path = "usb/mod.rs"]
mod usb;

use connection::ConnectionManager;
use usb::USB上下文;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    colored::control::set_override(true);
    cancel::install_ctrlc_handler();

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
        "Copyright (c) wunelezi & trae | Licensed under GPL-3.0".dimmed()
    );
    println!();

    let raw_args: Vec<String> = std::env::args().collect();
    let is_help = raw_args.iter().any(|a| a == "-h" || a == "--help");
    if is_help {
        cmd::print_help();
        return Ok(());
    }

    let cli = cmd::cli::Cli::parse();

    // Windows: 驱动安装 (pnputil / wdi-rs) 必须管理员，提前提权
    // 用环境变量 MTKCLIENT_ELEVATED 标记避免子进程重复提权造成死循环
    #[cfg(target_os = "windows")]
    {
        if !cli.no_elevate
            && !connection::driver::is_admin()
            && std::env::var_os("MTKCLIENT_ELEVATED").is_none()
        {
            // 提示用户（UAC 弹窗会覆盖这个）
            eprintln!("[MAIN] 需要管理员权限以安装 WinUSB 驱动，正在请求提权...");
            if let Err(e) = connection::driver::restart_as_admin() {
                eprintln!("[MAIN] 提权失败: {}", e);
                eprintln!(
                    "[MAIN] 请右键以管理员身份运行本程序，或加 --检测管理员权限 跳过（将无法切换 WinUSB）"
                );
                return Err(e.into());
            }
            // restart_as_admin 内部 std::process::exit(0)，不会回到这里
        }
    }

    let app_config = system::config::AppConfig::from_cli(&cli);

    usb::设置USB日志开关(cli.usb_log);

    // --quiet-dump: 抑制 USB 读取日志和进度条
    if cli.quiet_dump {
        usb::设置USB读取静默(true);
    }

    let log_level = if cli.quiet {
        log::LevelFilter::Error // --quiet: 只输出 ERROR
    } else {
        app_config.log_level
    };

    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(log_level)
        .filter_module("mtkclient_rs", log_level) // 明确指定本 crate 的日志级别
        .filter_module("nusb", log::LevelFilter::Warn)
        .format(|buf, record| {
            use std::io::Write;
            let level = match record.level() {
                log::Level::Error => "ERROR",
                log::Level::Warn => "WARN ",
                log::Level::Info => "INFO ",
                log::Level::Debug => "DEBUG",
                log::Level::Trace => "TRACE",
            };
            let timestamp = get_local_timestamp();
            writeln!(buf, "[{}] [{}] {}", timestamp, level, record.args())
        });

    if cli.usb_log {
        // TeeLogger：终端 + usb_debug.log（覆盖模式）
        let terminal_logger = builder.build();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("usb_debug.log")
            .map_err(|e| format!("无法创建 usb_debug.log: {}", e))?;

        let tee_logger = TeeLogger {
            terminal: terminal_logger,
            file: Mutex::new(file),
        };
        log::set_boxed_logger(Box::new(tee_logger))
            .map_err(|e| format!("设置全局 logger 失败: {}", e))?;
        log::set_max_level(log_level);
    } else {
        builder.init();
    }

    let cmd = app_config.command.as_deref().unwrap_or("");

    if cmd.is_empty() {
        cmd::print_help();
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
        if let Some((current_vid, current_pid, _dev_type)) = usb::获取第一个联发科VIDPID() {
            if crate::connection::try_reuse_da_session(current_vid, current_pid) {
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
                crate::connection::reset_session();
                conn_mgr.smart_init(&usb_context, 工作模式)?
            }
        }
    } else {
        conn_mgr.smart_init(&usb_context, 工作模式)?
    };

    // 从 .state 恢复 preloader 路径（如果存在且用户未指定）
    let restored_preloader_path = if da_session_reused {
        if let Some(state) = crate::connection::SessionState::load() {
            if let Some(ref path) = state.preloader_path {
                if std::path::Path::new(path).exists() {
                    info!("[DA_SESSION] 从 .state 恢复 preloader 路径: {}", path);
                    Some(path.clone())
                } else {
                    warn!("[DA_SESSION] .state 中的 preloader 路径不存在: {}", path);
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    match 工作模式 {
        crate::system::config::工作模式::Preloader => {
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
    // 注意：dump + load + bypass + upload_da 全部下放到 cmd::handle_command 统一处理，
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

    let mut da = da::DAXFlash::new(&mut preloader);
    da.patch_da = cli.patch_da;
    da.da_x_speed = app_config.da_x_speed;

    // 复用 DA 会话时标记 DA 已加载，避免 handle_command 中重复 upload_da
    if da_session_reused {
        da.daext = true;
        info!("[DA_SESSION] DA 已标记为加载状态 (daext=true)");
    }

    // preloader 路径优先级：用户指定 > .state 恢复 > 从设备 dump
    let effective_preloader_path = if !final_preloader_path.is_empty() {
        final_preloader_path.clone()
    } else if let Some(ref path) = restored_preloader_path {
        path.clone()
    } else {
        String::new()
    };

    // dump + load + bypass + upload_da 全部由 handle_command 内部完成
    // 这里不再调用 dump_preloader_payload / load_preloader_emi / upload_da
    cmd::handle_command(
        &mut da,
        &app_config,
        cli.log_level,
        cli.quiet_dump,
        &effective_preloader_path,
        &usb_context,
    )?;

    Ok(())
}

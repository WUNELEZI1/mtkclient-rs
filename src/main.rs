// #![allow(dead_code)] — removed during dead-code sweep; re-add only if needed

// 纯美化：压制 clippy 风格类 lint（不改变任何运行时行为 / 安全性，仅清理告警）。
// 覆盖项均为 warn 级纯风格提示；如需收紧代码风格可移除本段后逐项手工重构。
#![allow(clippy::collapsible_if)]
#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::upper_case_acronyms)]
#![allow(clippy::let_and_return)]
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::manual_range_contains)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::needless_return)]
#![allow(clippy::single_match)]
#![allow(clippy::redundant_pattern_matching)]
#![allow(clippy::question_mark)]
#![allow(clippy::print_literal)]
#![allow(clippy::op_ref)]
#![allow(clippy::only_used_in_recursion)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::manual_strip)]
#![allow(clippy::format_in_format_args)]
#![allow(clippy::useless_format)]
#![allow(clippy::useless_borrows_in_formatting)]
#![allow(clippy::useless_asref)]
#![allow(clippy::unnecessary_unwrap)]
#![allow(clippy::unnecessary_find_map)]

use clap::Parser;
use crate::color::Colorize;
use log::{debug, error, info, warn};
use std::io::Write;
use std::sync::Mutex;

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn SetConsoleOutputCP(wCodePage: u32) -> i32;
    fn SetConsoleCP(wCodePage: u32) -> i32;
}

/// 获取本地时间戳字符串 [YYYY/MM/DD HH:MM:SS.mmm]
fn get_local_timestamp() -> String {
    #[cfg(target_os = "windows")]
    {
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
            st.w_year, st.w_month, st.w_day, st.w_hour, st.w_minute, st.w_second, st.w_milliseconds
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

/// 自研终端日志器，替代 env_logger::Logger。
/// 复刻原 env_logger 终端格式：[时间戳] [LEVEL] 消息，并保留按模块过滤（nusb 限定至 Warn）。
struct TerminalLogger {
    /// 全局默认日志级别（mtkclient_rs 及其它模块）
    level: log::LevelFilter,
}

impl TerminalLogger {
    /// 返回指定 target 的实际生效级别：nusb 强制不超过 Warn，其余用全局默认。
    fn level_for(&self, target: &str) -> log::LevelFilter {
        if target == "nusb" {
            log::LevelFilter::Warn
        } else {
            self.level
        }
    }
}

impl log::Log for TerminalLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level_for(metadata.target())
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            log::Level::Error => "ERROR",
            log::Level::Warn => "WARN ",
            log::Level::Info => "INFO ",
            log::Level::Debug => "DEBUG",
            log::Level::Trace => "TRACE",
        };
        let timestamp = get_local_timestamp();
        let _ = writeln!(std::io::stderr(), "[{}] [{}] {}", timestamp, level, record.args());
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// TeeLogger：同时输出到终端（TerminalLogger）和文件（usb_debug.log）
struct TeeLogger {
    terminal: TerminalLogger,
    file: Mutex<std::fs::File>,
}

impl log::Log for TeeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.terminal.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        // 1. 终端输出（保持原有格式）
        self.terminal.log(record);

        // 2. 文件输出（简化格式，含 target 便于排查）
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
mod error;
#[path = "exploit/mod.rs"]
mod exploit;
#[path = "partition/mod.rs"]
mod partition;
#[path = "preloader/mod.rs"]
mod preloader;
mod resume;
#[path = "security/mod.rs"]
mod security;
mod system;
#[path = "usb/mod.rs"]
mod usb;
mod util;
mod color;
mod progress;
mod sha;

use connection::ConnectionManager;
use usb::UsbContext;

/// 检查 Windows 版本，要求 Windows 10 或更高
#[cfg(target_os = "windows")]
fn check_windows_version() -> Result<(), String> {
    #[repr(C)]
    struct OSVERSIONINFOEXW {
        dw_os_version_info_size: u32,
        dw_major_version: u32,
        dw_minor_version: u32,
        dw_build_number: u32,
        dw_platform_id: u32,
        sz_csd_version: [u16; 128],
        w_service_pack_major: u16,
        w_service_pack_minor: u16,
        w_suite_mask: u16,
        w_product_type: u8,
        w_reserved: u8,
    }

    unsafe extern "system" {
        fn RtlGetVersion(lp_version_information: *mut OSVERSIONINFOEXW) -> i32;
    }

    let mut info = OSVERSIONINFOEXW {
        dw_os_version_info_size: std::mem::size_of::<OSVERSIONINFOEXW>() as u32,
        dw_major_version: 0,
        dw_minor_version: 0,
        dw_build_number: 0,
        dw_platform_id: 0,
        sz_csd_version: [0; 128],
        w_service_pack_major: 0,
        w_service_pack_minor: 0,
        w_suite_mask: 0,
        w_product_type: 0,
        w_reserved: 0,
    };

    unsafe {
        RtlGetVersion(&mut info);
    }

    // Windows 10 = Major 10, Build >= 10240
    // Windows 11 = Major 10, Build >= 22000
    if info.dw_major_version < 10 {
        return Err(format!(
            "不支持的操作系统: Windows {}.{} (Build {})。本程序需要 Windows 10 或 Windows 11 (64-bit)。",
            info.dw_major_version, info.dw_minor_version, info.dw_build_number
        ));
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 编译时限制：仅支持 x86_64 架构
    #[cfg(not(target_arch = "x86_64"))]
    {
        eprintln!("错误: 本程序仅支持 64-bit (x86_64) 架构。");
        std::process::exit(1);
    }

    #[cfg(target_os = "windows")]
    {
        // 1. 运行时检查 Windows 版本（要求 Windows 10+）
        if let Err(e) = check_windows_version() {
            eprintln!("{}", e);
            std::process::exit(1);
        }

        // 2. 强制设置控制台代码页为 UTF-8（CP_UTF8 = 65001）
        //    必须在任何输出之前调用，确保中文字符正确显示
        unsafe {
            SetConsoleCP(65001);
            SetConsoleOutputCP(65001);
        }
        // 3. 启用 Windows ANSI 转义序列支持（CMD.exe 默认关闭）
        crate::color::enable_virtual_terminal();
    }

    #[cfg(not(target_os = "windows"))]
    {
        eprintln!("警告: 本程序仅在 Windows 10/11 上经过充分测试。");
    }

    cancel::install_ctrlc_handler();

    // 输出版本号（每次代码更新 cargo.toml version +0.0.1）
    println!(
        "{} {}",
        "mtkclient-rs".cyan().bold(),
        format!("v{}", env!("CARGO_PKG_VERSION")).yellow().bold()
    );
    println!(
        "{}",
        "Copyright (c) 无能乐子(wunelezi) | Licensed under Apache-2.0".dimmed()
    );
    println!(
        "{} {}  {} {}",
        "获取更新:".dimmed(),
        "https://gitee.com/WUNELEZI1/mtkclient-rs/releases"
            .blue()
            .underline(),
        "QQ:".dimmed(),
        "3535571067".green()
    );
    println!(
        "{}",
        "本工具完全免费，请勿被骗！禁止任何形式的倒卖、破解或去除作者信息。违者必究。"
            .red()
            .dimmed()
    );
    println!();

    let raw_args: Vec<String> = std::env::args().collect();
    let is_help = raw_args.iter().any(|a| a == "-h" || a == "--help");
    if is_help {
        cmd::print_help();
        return Ok(());
    }

    let cli = cmd::cli::Cli::parse();

    // === --data-dir 覆盖（GUI 传入的解压数据目录）===
    if let Some(ref data_dir) = cli.data_dir {
        let data_path = std::path::PathBuf::from(data_dir);
        if data_path.is_dir() {
            debug!("使用 --data-dir: {}", data_path.display());
            system::paths::set_data_dir(data_path);
        } else {
            warn!("--data-dir 路径不存在或不是目录: {}", data_dir);
        }
    }

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

    usb::set_usb_log_switch(cli.usb_log);

    // --quiet-dump: 抑制 USB 读取日志和进度条
    if cli.quiet_dump {
        usb::set_usb_read_quiet(true);
    }

    let log_level = if cli.quiet {
        log::LevelFilter::Error // --quiet: 只输出 ERROR
    } else {
        app_config.log_level
    };

    let terminal_logger = TerminalLogger { level: log_level };

    if cli.usb_log {
        // TeeLogger：终端 + tmp/usb_debug.log（覆盖模式）
        let usb_log_path = crate::system::paths::get_tmp_path("usb_debug.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&usb_log_path)
            .map_err(|e| format!("无法创建 {}: {}", usb_log_path.display(), e))?;

        let tee_logger = TeeLogger {
            terminal: terminal_logger,
            file: Mutex::new(file),
        };
        log::set_boxed_logger(Box::new(tee_logger))
            .map_err(|e| format!("设置全局 logger 失败: {}", e))?;
    } else {
        log::set_boxed_logger(Box::new(terminal_logger))
            .map_err(|e| format!("设置全局 logger 失败: {}", e))?;
    }
    // 全局最大级别取各模块过滤指令的最大值（nusb=Warn 可能高于默认）
    log::set_max_level(std::cmp::max(log_level, log::LevelFilter::Warn));

    let cmd = app_config.command.as_deref().unwrap_or("");

    if cmd.is_empty() {
        cmd::print_help();
        return Ok(());
    }

    // zyb detect 不需要设备连接，直接进入 Preloader 串口探测
    if cmd == "zyb" && app_config.cmd_args.first().map(|s| s.as_str()) == Some("detect") {
        return cmd::detect::cmd_detect().map_err(|e| e.to_string().into());
    }

    let usb_context = UsbContext::new().inspect_err(|e| {
        error!("{}", e);
    })?;

    // COM 口前置握手 + libusb 后续通信
    let mut conn_mgr = ConnectionManager::new();

    // 工作模式：brom / preloader / auto
    let work_mode = app_config.work_mode;
    debug!("[MAIN] 工作模式: {:?}", work_mode);

    // === DA 会话复用检查 ===
    // 如果 .state 存在且设备已经处于 DA 模式（PID=0x2000），
    // 可以跳过 BROM→DA 流程，直接连接 DA 模式设备。
    // 流程：
    //   1. libusb 枚举当前 USB 设备，找到第一个 MediaTek 设备
    //   2. 读 .state 文件，检查 da_loaded 标志和 VID/PID 匹配
    //   3. 如果复用条件满足 → connect_to_da_mode（直接连接 PID=0x2000）
    //   4. 否则 → 走正常的 smart_init 流程
    let mut da_session_reused =
        if let Some((current_vid, current_pid, _dev_type)) = usb::get_first_mtk_vid_pid() {
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
                let pair = conn_mgr.smart_init(&usb_context, work_mode)?;
                da_session_reused = conn_mgr.da_session_reused_in_init;
                pair
            }
        }
    } else {
        let pair = conn_mgr.smart_init(&usb_context, work_mode)?;
        if conn_mgr.da_session_reused_in_init {
            da_session_reused = true;
        }
        pair
    };

    // 从 .state 恢复 preloader 路径（如果存在且用户未指定）
    let restored_preloader_path = if da_session_reused {
        if let Some(state) = crate::connection::SessionState::load() {
            if let Some(ref path) = state.preloader_path {
                if std::path::Path::new(path).exists() {
                    debug!("[DA_SESSION] 从 .state 恢复 preloader 路径: {}", path);
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

    if da_session_reused {
        // 实际复用既有 DA 会话（设备停留在 DA 模式），与命令行 --mode 未必一致，
        // 如实显示，避免"--mode preloader 却打印 BROM 模式"的误导。
        info!("{}", "连接成功 (复用既有 DA 会话)".green().bold());
    } else {
        match work_mode {
            crate::system::config::WorkMode::Preloader => {
                info!("{}", "连接成功 (Preloader 模式)".green().bold());
            }
            _ => {
                info!("{}", "连接成功 (BROM 模式)".green().bold());
            }
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
        debug!("使用指定的 preloader 文件: {}", final_preloader_path);
    } else {
        debug!(
            "{}",
            "未指定 --preloader，将强制从设备 dump 并覆盖同名文件".yellow()
        );
    }

    // 传递用户指定的 DA 文件路径给 preloader（供后台线程预解析和 upload_da 使用）
    preloader.da_path = app_config.da_path.clone().unwrap_or_default();

    let mut da = da::DAXFlash::new(&mut preloader);
    da.patch_da = cli.patch_da;
    da.da_x_speed = app_config.da_x_speed;

    // 复用 DA 会话时：循环排空 IN 残留（不 clear_halt），直接标记 daext
    // 不做心跳验证、不发 xflash_sync、不 reconnect：
    //   刷机匣日志证实"重新初始化DA模式"只做 drain + devctrl 查询后直接操作，
    //   DA 在 DRAM 中持续运行，USB 重新打开后状态机保持完整。
    //   心跳验证用的 send_devctrl(0x010106) 在 USB 刚重开后可能超时（设备还在
    //   处理 USB 枚举），导致误判 DA 失效 → reconnect → xflash_sync → 状态破坏 →
    //   fallback BROM 加载 → 设备实际还在 DA 模式 → send_da 超时。
    //   drain_pipes 循环排空 IN 残留，但不 clear_halt（会重置 data toggle
    //   导致后续读取错位，如 0xEF400400 错误）。
    if da_session_reused {
        da.preloader.device.drain_pipes();
        da.daext = true;
        let init_mode = crate::connection::SessionState::load().map(|s| s.init_mode);
        info!(
            "[DA_SESSION] DA 会话复用 (init_mode={:?})，跳过验证直接使用",
            init_mode
        );
    }

    if !da_session_reused {
        // 如果复用验证失败，回退到正常加载流程
        // handle_command 中检测到 daext=false 会自动 upload_da
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

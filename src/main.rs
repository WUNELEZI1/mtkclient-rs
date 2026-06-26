#![allow(dead_code)]

use clap::Parser;
use colored::Colorize;
use log::{debug, error, info, warn};
use usb::UsbContext;

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
    fn SetConsoleCP(wCodePageID: u32) -> i32;
}

mod cli;
mod commands;
mod config;
mod connection;
mod da_extension;
mod da_partition;
mod da_xflash;
mod driver;
mod frp;
mod kamakiri2;
mod paths;
mod preloader;
mod seccfg;
mod sej;
mod session;
mod usb;
mod vbmeta;

use connection::ConnectionManager;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "windows")]
    unsafe {
        SetConsoleCP(65001);
        SetConsoleOutputCP(65001);
    }

    let raw_args: Vec<String> = std::env::args().collect();
    let is_help = raw_args.iter().any(|a| a == "-h" || a == "--help");
    if is_help {
        commands::print_help();
        return Ok(());
    }

    let cli = cli::Cli::parse();

    // Windows: 驱动安装 (pnputil / wdi-rs) 必须管理员，提前提权
    // 用环境变量 MTKCLIENT_ELEVATED 标记避免子进程重复提权造成死循环
    #[cfg(target_os = "windows")]
    {
        if !cli.no_elevate
            && !driver::is_admin()
            && std::env::var_os("MTKCLIENT_ELEVATED").is_none()
        {
            // 提示用户（UAC 弹窗会覆盖这个）
            eprintln!("[MAIN] 需要管理员权限以安装 WinUSB 驱动，正在请求提权...");
            if let Err(e) = driver::restart_as_admin() {
                eprintln!("[MAIN] 提权失败: {}", e);
                eprintln!(
                    "[MAIN] 请右键以管理员身份运行本程序，或加 --no-elevate 跳过（将无法切换 WinUSB）"
                );
                return Err(e.into());
            }
            // restart_as_admin 内部 std::process::exit(0)，不会回到这里
        }
    }

    let app_config = config::AppConfig::from_cli(&cli);

    usb::set_usb_log_enabled(cli.usb_log);

    // --quiet-dump: 抑制 USB 读取日志和进度条
    if cli.quiet_dump {
        usb::set_quiet_usb_read(true);
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
        commands::print_help();
        return Ok(());
    }

    let usb_context = UsbContext::new().inspect_err(|e| {
        error!("{}", e);
    })?;

    // COM 口前置握手 + libusb 后续通信
    // 注意：filter 卸载和重新安装在 smart_init 内部处理
    let mut conn_mgr = ConnectionManager::new();

    // === DA 会话复用检查 ===
    // 如果 .state 存在且设备已经处于 DA 模式（PID=0x2000），
    // 可以跳过 BROM→DA 流程，直接连接 DA 模式设备。
    // 流程：
    //   1. libusb 枚举当前 USB 设备，找到第一个 MediaTek 设备
    //   2. 读 .state 文件，检查 da_loaded 标志和 VID/PID 匹配
    //   3. 如果复用条件满足 → connect_to_da_mode（直接连接 PID=0x2000）
    //   4. 否则 → 走正常的 smart_init 流程
    let da_session_reused =
        if let Some((current_vid, current_pid, _dev_type)) = usb::get_first_mediatek_vid_pid() {
            if current_pid == 0x0003 {
                // 核心修复：如果当前设备是 BROM (0003)，说明设备已重启，必须重置 DA 会话
                debug!("[session] 检测到 BROM 设备，强制重置旧的 DA 会话状态");
                crate::session::reset_session();
                false
            } else if crate::session::try_reuse_da_session(current_vid, current_pid) {
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
                crate::session::reset_session();
                conn_mgr.smart_init(&usb_context)?
            }
        }
    } else {
        conn_mgr.smart_init(&usb_context)?
    };

    info!("{}", "连接成功 (BROM 模式)".green().bold());

    let final_preloader_path = if let Some(ref path) = app_config.preloader_path {
        info!("使用指定的 preloader 文件: {}", path);
        path.clone()
    } else {
        String::new()
    };

    let mut da = da_xflash::DAXFlash::new(&mut preloader);
    da.patch_da = cli.patch_da;

    if final_preloader_path.is_empty() {
        match da.preloader.get_target_config() {
            Ok(cfg) => info!("{}", cfg.format_info()),
            Err(e) => warn!("获取 target config 失败: {}", e),
        }
        da.preloader
            .bypass_security(&usb_context)
            .map_err(|e| format!("bypass_security 失败: {}", e))?;
        let data = da
            .preloader
            .dump_preloader_from_ram(false)
            .map_err(|e| format!("dump_preloader_ram 失败: {}", e))?;
        if !data.is_empty() {
            let filename =
                if let Some(info_idx) = data.windows(16).position(|w| w == b"MTK_BLOADER_INFO") {
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
                info!("Preloader 已提取: {} ({} 字节)", filename, data.len());
            }
        }
    }

    info!("加载 EMI 数据: {}", final_preloader_path);
    if let Err(e) = da.load_preloader_emi(&final_preloader_path) {
        info!("Warning: EMI 加载失败: {}", e);
    }

    da.upload_da()
        .map_err(|e| format!("DA 加载失败: {}", e))
        .and_then(|ok| {
            if ok {
                Ok(())
            } else {
                Err("DA 加载失败".to_string())
            }
        })?;

    if cli.log_level >= 3 {
        if let Some(data) = da.get_emi_data() {
            let _ = std::fs::write("emi_debug.bin", data);
        }
        if let Some(data) = da.get_extensions_data() {
            let _ = std::fs::write("extensions_debug.bin", &data);
        }
    }

    commands::handle_command(
        &mut da,
        &app_config,
        cli.log_level,
        cli.quiet_dump,
        &final_preloader_path,
        &usb_context,
    )?;

    Ok(())
}

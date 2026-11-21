// #![allow(dead_code)] — removed during dead-code sweep; re-add only if needed

// Purely cosmetic: suppress clippy style lints (no runtime behavior / safety changes, only warning cleanup).
// All covered lints are warn-level style hints; remove this block and refactor manually if stricter style is needed.
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

use crate::color::Colorize;
use clap::Parser;
use log::{debug, error, info, warn};
use std::io::Write;
use std::sync::Mutex;

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn SetConsoleOutputCP(wCodePage: u32) -> i32;
    fn SetConsoleCP(wCodePage: u32) -> i32;
}

/// Get a local timestamp string [YYYY/MM/DD HH:MM:SS.mmm]
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

/// In-house terminal logger, replacing env_logger::Logger.
/// Mirrors the original env_logger terminal format: `timestamp` `LEVEL` message, keeping per-module filtering (nusb capped at Warn).
struct TerminalLogger {
    /// Global default log level (mtkclient_rs and other modules)
    level: log::LevelFilter,
}

impl TerminalLogger {
    /// Return the effective level for a target: nusb is forced to at most Warn, others use the global default.
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
        let _ = writeln!(
            std::io::stderr(),
            "[{}] [{}] {}",
            timestamp,
            level,
            record.args()
        );
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// TeeLogger: write to both the terminal (TerminalLogger) and a file (usb_debug.log)
struct TeeLogger {
    terminal: TerminalLogger,
    file: Mutex<std::fs::File>,
}

impl log::Log for TeeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.terminal.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        // 1. Terminal output (keeps the original format)
        self.terminal.log(record);

        // 2. File output (simplified format including target for troubleshooting)
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
mod color;
// Note: this was once mistakenly written as `#[path = "connection/mod.rs"] mod confirm;`,
// making `crate::confirm` actually point to the connection module while src/confirm/mod.rs was never compiled
// (effectively disabling the danger-operation confirmer). The two modules must be declared independently.
mod confirm;
mod connection;

mod core;
#[path = "da/mod.rs"]
mod da;
mod error;
mod exec;
#[path = "exploit/mod.rs"]
mod exploit;
mod fs;
mod i18n;
mod ops;
#[path = "partition/mod.rs"]
mod partition;
#[path = "preloader/mod.rs"]
mod preloader;
mod progress;
mod resume;
#[path = "security/mod.rs"]
mod security;
mod session;
mod sha;
mod slot;
mod system;
mod transport;
#[path = "usb/mod.rs"]
mod usb;
mod util;

use connection::ConnectionManager;
use usb::UsbContext;

/// Check the Windows version; Windows 10 or later is required
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
        return Err(i18n::tr(
            "os_unsupported",
            &[
                &info.dw_major_version.to_string(),
                &info.dw_minor_version.to_string(),
                &info.dw_build_number.to_string(),
            ],
        ));
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Language selection takes priority over all output: `--language <LANG>` may appear anywhere
    // (e.g. `mtkclient-rs help --language zh-CN`); falls back to the environment variable when unspecified.
    i18n::detect_env();
    let argv: Vec<String> = i18n::extract_from_args(std::env::args().collect());

    // Compile-time restriction: only the x86_64 architecture is supported
    #[cfg(not(target_arch = "x86_64"))]
    {
        eprintln!("{}", i18n::t("arch_unsupported"));
        std::process::exit(1);
    }

    #[cfg(target_os = "windows")]
    {
        // 1. Runtime check of the Windows version (Windows 10+ required)
        if let Err(e) = check_windows_version() {
            eprintln!("{}", e);
            std::process::exit(1);
        }

        // 2. Force the console code page to UTF-8 (CP_UTF8 = 65001)
        //    Must be called before any output so Chinese characters display correctly
        unsafe {
            SetConsoleCP(65001);
            SetConsoleOutputCP(65001);
        }
        // 3. Enable Windows ANSI escape sequence support (disabled by default in CMD.exe)
        crate::color::enable_virtual_terminal();
    }

    #[cfg(not(target_os = "windows"))]
    {
        eprintln!("{}", i18n::t("os_warning"));
    }

    cancel::install_ctrlc_handler();

    // Print the version (bump cargo.toml version +0.0.1 on every code update)
    println!(
        "{} {}",
        "mtkclient-rs".cyan().bold(),
        format!("v{}", env!("CARGO_PKG_VERSION")).yellow().bold()
    );
    println!(
        "{}",
        "Copyright (c) 无能乐子(wunelezi) | Licensed under GPL-3.0-or-later".dimmed()
    );
    println!(
        "{} {}",
        i18n::t("get_updates").dimmed(),
        "https://github.com/WUNELEZI1/mtkclient-rs/releases"
            .blue()
            .underline()
    );
    println!("{}", i18n::t("free_tool_notice").red().dimmed());
    println!();

    let is_help = argv.iter().skip(1).any(|a| a == "-h" || a == "--help")
        || argv.get(1).map(|a| a == "help").unwrap_or(false);
    if is_help {
        print!("{}", i18n::help());
        return Ok(());
    }

    let cli = cmd::cli::Cli::parse_from(argv);

    // === --data-dir override (extracted data directory passed in by the GUI) ===
    if let Some(ref data_dir) = cli.data_dir {
        let data_path = std::path::PathBuf::from(data_dir);
        if data_path.is_dir() {
            debug!("使用 --data-dir: {}", data_path.display());
            system::paths::set_data_dir(data_path);
        } else {
            warn!("{}", i18n::tr("data_dir_missing", &[data_dir]));
        }
    }

    // Windows: driver installation (pnputil / wdi-rs) requires admin, so elevate early
    // Use the MTKCLIENT_ELEVATED env var to prevent child processes from re-elevating in an infinite loop
    #[cfg(target_os = "windows")]
    {
        if !cli.no_elevate
            && !connection::driver::is_admin()
            && std::env::var_os("MTKCLIENT_ELEVATED").is_none()
        {
            // Notify the user (the UAC dialog will cover this)
            eprintln!("{}", i18n::t("elevate_request"));
            if let Err(e) = connection::driver::restart_as_admin() {
                eprintln!("{}", i18n::tr("elevate_failed", &[&e.to_string()]));
                eprintln!("{}", i18n::t("elevate_hint"));
                return Err(e.into());
            }
            // restart_as_admin calls std::process::exit(0) internally and never returns here
        }
    }

    let app_config = system::config::AppConfig::from_cli(&cli);

    // === Anti-reversing: debugger detection ===
    // Called unconditionally to keep the symbol reachable (satisfying zero dead_code); whether diagnostic output is reduced
    // depends on the build type, avoiding interference with step debugging during development. In release, a detected debugger
    // disables extra diagnostic output without changing any flashing functionality, only reducing internal details exposed to analysts.
    let debugger_attached = security::obfuscate::debugger_present();
    let anti_analysis = debugger_attached && !cfg!(debug_assertions);
    if anti_analysis {
        eprintln!("[MAIN] 检测到调试器会话：已关闭详细诊断输出。");
    }
    let effective_usb_log = cli.usb_log && !anti_analysis;

    usb::set_usb_log_switch(effective_usb_log);

    // --quiet-dump: suppress USB read logs and the progress bar
    if cli.quiet_dump || anti_analysis {
        usb::set_usb_read_quiet(true);
    }

    let log_level = if cli.quiet {
        log::LevelFilter::Error // --quiet: emit ERROR only
    } else {
        app_config.log_level
    };

    let terminal_logger = TerminalLogger { level: log_level };

    if effective_usb_log || cli.log_file.is_some() {
        // TeeLogger: terminal + file. --usb_log writes tmp/usb_debug.log (truncate);
        // --log_file <path> writes the user-specified file (append, for cross-session archiving).
        let (path, append) = if let Some(ref p) = cli.log_file {
            (std::path::PathBuf::from(p), true)
        } else {
            (crate::system::paths::get_tmp_path("usb_debug.log"), false)
        };
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true);
        if append {
            opts.append(true);
        } else {
            opts.truncate(true);
        }
        let file = opts
            .open(&path)
            .map_err(|e| format!("无法创建 {}: {}", path.display(), e))?;

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
    // The global max level takes the maximum of the per-module filter directives (nusb=Warn may exceed the default)
    log::set_max_level(std::cmp::max(log_level, log::LevelFilter::Warn));

    let cmd = app_config.command.as_deref().unwrap_or("");

    if cmd.is_empty() {
        print!("{}", i18n::help());
        return Ok(());
    }

    // zyb detect needs no device connection; go straight to Preloader serial probing
    if cmd == "zyb" && app_config.cmd_args.first().map(|s| s.as_str()) == Some("detect") {
        return cmd::detect::cmd_detect().map_err(|e| e.to_string().into());
    }

    // boot / proinfo are offline image parsing commands that need no device connection; go straight to file parsing
    if cmd == "boot" {
        return cmd::bootimg::cmd_boot(&app_config.cmd_args).map_err(|e| e.to_string().into());
    }
    if cmd == "proinfo" {
        return cmd::proinfo::cmd_proinfo(&app_config.cmd_args).map_err(|e| e.to_string().into());
    }

    // zyb seccfg info `<file>`: offline seccfg image parsing (no device needed)
    if cmd == "zyb"
        && app_config.cmd_args.first().map(|s| s.as_str()) == Some("seccfg")
        && app_config.cmd_args.get(1).map(|s| s.as_str()) == Some("info")
        && let Some(path) = app_config.cmd_args.get(2)
    {
        return crate::security::seccfg::seccfg_info_file(path).map_err(|e| e.to_string().into());
    }

    // avb offline command: parse / disable vbmeta image files (in-house avbtool, no device needed)
    // plan: offline view of planning results (no device connection)
    if cmd == "plan" {
        let plan_mode = match app_config.work_mode {
            crate::system::config::WorkMode::Brom => "brom",
            crate::system::config::WorkMode::Preloader => "preloader",
            crate::system::config::WorkMode::Auto => "auto",
        };
        return cmd::plan::cmd_plan(
            &app_config.cmd_args,
            app_config.da_path.as_deref(),
            plan_mode,
        )
        .map_err(|e| e.to_string().into());
    }

    if cmd == "avb" {
        return cmd_avb_offline(&app_config.cmd_args);
    }

    // Offline simulation (--sim / --port mock://...): no USB, goes through Executor → Orchestrator
    if let Some(spec) = cli.sim.clone() {
        if let Err(e) = cmd::sim::run_sim(&app_config, &spec, cli.log_level) {
            eprintln!();
            eprintln!("{}", format!("[sim] 失败: {}", e).red().bold());
            std::process::exit(1);
        }
        return Ok(());
    }

    let usb_context = UsbContext::new().inspect_err(|e| {
        error!("{}", e);
    })?;

    // COM port pre-handshake + libusb for subsequent communication
    let mut conn_mgr = ConnectionManager::new();

    // Work mode: brom / preloader / auto
    let work_mode = app_config.work_mode;
    debug!("[MAIN] 工作模式: {:?}", work_mode);

    // === DA session reuse is fully disabled ===
    // MTK device DA sessions do not survive across processes: whether BROM (PID=0x0003) or Preloader (PID=0x2000),
    // process exit resets the device and kills the DA; but the PID stays the same (BROM always 0x0003, Preloader always
    // 0x2000). The old logic used .state(da_loaded) + device_online() comparing only PID strings to decide reuse,
    // inevitably hitting a dead DA session and sending the next command to a device without a DA -> failure/hang.
    // So all modes uniformly go through smart_init re-handshake + DA reload; .state is kept only as cache metadata
    // (gpt_cache / optional_query_failures / preloader_path) and no longer reuses DA session connections.
    // See the smart_init_preloader comment in reconnect.rs and the device_online implementation in session.rs.
    // DA session reuse is fully disabled (see the comment above); all modes use smart_init re-handshake + DA reload.
    let (mut preloader, _mode) = conn_mgr.smart_init(&usb_context, work_mode)?;

    // Restore the preloader path cache from .state (unrelated to DA session reuse; only a default data source when the user specifies none).
    let restored_preloader_path = match crate::connection::SessionState::load() {
        Some(state) => match state.preloader_path {
            Some(ref path) if std::path::Path::new(path).exists() => {
                debug!("[MAIN] 从 .state 恢复 preloader 路径: {}", path);
                Some(path.clone())
            }
            Some(ref path) => {
                warn!("[MAIN] .state 中的 preloader 路径不存在: {}", path);
                None
            }
            None => None,
        },
        None => None,
    };

    match work_mode {
        crate::system::config::WorkMode::Preloader => {
            info!("{}", i18n::t("connected_preloader").green().bold());
        }
        _ => {
            info!("{}", i18n::t("connected_brom").green().bold());
        }
    }

    // preloader_path determines the subsequent strategy:
    //   - Some(path): use the user-specified file as the EMI data source, skipping dump + bypass
    //   - None: force one preloader dump from the device (overwriting same-named files), then bypass as needed
    //
    // Note: dump + load + bypass + upload_da are all delegated to cmd::handle_command for unified handling,
    // avoiding double execution in main.rs and handle_command (previously it dumped twice).
    let final_preloader_path = app_config.preloader_path.clone().unwrap_or_default();
    if !final_preloader_path.is_empty() {
        debug!("使用指定的 preloader 文件: {}", final_preloader_path);
    } else {
        debug!("{}", i18n::t("no_preloader_forced_dump").yellow());
    }

    // Pass the user-specified DA file path to preloader (for background-thread pre-parsing and upload_da)
    preloader.da_path = app_config.da_path.clone().unwrap_or_default();

    let mut da = da::DAXFlash::new(&mut preloader);
    da.patch_da = cli.patch_da;
    da.da_x_speed = app_config.da_x_speed;

    // DA session reuse is disabled (see the comment above): upload_da inside handle_command always uses a fresh DA path.

    // preloader path priority: user-specified > restored from .state > dump from device
    let effective_preloader_path = if !final_preloader_path.is_empty() {
        final_preloader_path.clone()
    } else if let Some(ref path) = restored_preloader_path {
        path.clone()
    } else {
        String::new()
    };

    // dump + load + bypass + upload_da are all handled inside handle_command
    // dump_preloader_payload / load_preloader_emi / upload_da are no longer called here
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

/// `mtkclient-rs avb <info|disable> <file> [out]`
///
/// Offline mode of the in-house avbtool:
/// - `avb info <vbmeta.img>`: output in the same format as `avbtool info-image`;
/// - `avb disable <vbmeta.img> [out]`: write out with flags=3 (hashtree+verification disabled)
///   (overwrites the original file by default; writes `<file>.disabled` when out is omitted).
fn cmd_avb_offline(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use crate::color::Colorize;
    use crate::security::avb::{FLAG_HASHTREE_DISABLED, FLAG_VERIFICATION_DISABLED, VbmetaImage};

    let usage = "用法: mtkclient-rs avb <info|disable> <vbmeta.img> [out]";
    let action = args.first().ok_or(usage)?;
    let file = args.get(1).ok_or(usage)?;
    let data = std::fs::read(file).map_err(|e| format!("读取 {file} 失败: {e}"))?;
    let vb = VbmetaImage::parse(&data).map_err(|e| format!("解析 vbmeta 失败: {e}"))?;

    match action.as_str() {
        "info" => {
            print!("{}", vb.info());
            Ok(())
        }
        "disable" => {
            let out_path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| format!("{file}.disabled"));
            let flags = FLAG_HASHTREE_DISABLED | FLAG_VERIFICATION_DISABLED;
            let patched = vb.disable_verification(flags);
            std::fs::write(&out_path, &patched)
                .map_err(|e| format!("写出 {out_path} 失败: {e}"))?;
            println!(
                "{}",
                format!("vbmeta 已禁用校验 (flags=0x{flags:02X}) -> {out_path}").green()
            );
            Ok(())
        }
        _ => Err(usage.into()),
    }
}

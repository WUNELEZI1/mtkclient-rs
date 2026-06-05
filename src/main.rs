use clap::Parser;
use colored::Colorize;
use log::{error, info, warn};
use std::process;
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
mod sej;
mod seccfg;
mod vbmeta;
mod preloader;
mod usb;

use connection::{ConnectionManager, DeviceMode};

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

    let app_config = config::AppConfig::from_cli(&cli);

    usb::set_usb_log_enabled(cli.usb_log);

    let log_level = if cli.quiet {
        log::LevelFilter::Warn
    } else {
        app_config.log_level
    };

    env_logger::builder()
        .filter_level(log_level)
        .parse_default_env()
        .format(|buf, record| {
            use std::io::Write;
            let level = match record.level() {
                log::Level::Error => "ERROR",
                log::Level::Warn => "WARN ",
                log::Level::Info => "INFO ",
                log::Level::Debug => "DEBUG",
                log::Level::Trace => "TRACE",
            };
            writeln!(buf, "[{}] {}", level, record.args())
        })
        .init();

    let cmd = app_config.command.as_deref().unwrap_or("");

    if cmd.is_empty() {
        commands::print_help();
        return Ok(());
    }

    if cmd == "reset" {
        let usb_context = UsbContext::new().inspect_err(|e| {
            error!("{}", e);
        })?;
        let mut conn_mgr = ConnectionManager::new();
        let (mut preloader, _mode) = conn_mgr.smart_init(&usb_context).inspect_err(|e| {
            error!("{}", e);
        })?;
        let _ = preloader.jump_bl();
        return Ok(());
    }

    if cmd == "dump-preloader" {
        let usb_context = UsbContext::new().inspect_err(|e| {
            error!("{}", e);
        })?;

        let mut conn_mgr = ConnectionManager::new();
        let (mut preloader, _mode) = conn_mgr.smart_init(&usb_context).inspect_err(|e| {
            error!("{}", e);
        })?;

        match preloader.dump_preloader_payload(false, false, &usb_context) {
            Ok((data, filename)) => {
                if data.is_empty() {
                    error!("dump_preloader_payload 返回空数据");
                    process::exit(1);
                }
                std::fs::write(&filename, &data).expect("保存 preloader 失败");
                info!("Preloader 已提取: {} ({} 字节)", filename, data.len());
            }
            Err(e) => {
                error!("提取 Preloader 失败: {}", e);
                process::exit(1);
            }
        }

        return Ok(());
    }

    let usb_context = UsbContext::new().inspect_err(|e| {
        error!("{}", e);
    })?;

    let mut conn_mgr = ConnectionManager::new();
    let (mut preloader, mode) = conn_mgr.smart_init(&usb_context).inspect_err(|e| {
        error!("{}", e);
    })?;

    info!("{}", format!("连接模式: {:?}", mode).green().bold());

    let final_preloader_path = if let Some(ref path) = app_config.preloader_path {
        info!("使用指定的 preloader 文件: {}", path);
        path.clone()
    } else if mode == DeviceMode::Brom {
        String::new()
    } else {
        "preloader_k69v1_64_k419.bin".to_string()
    };

    let mut da = da_xflash::DAXFlash::new(&mut preloader);
    da.patch_da = cli.patch_da;

    if mode == DeviceMode::Brom && final_preloader_path.is_empty() {
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
        &mode,
        &app_config,
        cli.log_level,
        cli.quiet_dump,
        &final_preloader_path,
        &usb_context,
    )?;

    Ok(())
}

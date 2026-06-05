use clap::Parser;
use colored::Colorize;
use log::{debug, error, info, warn};
use std::process;
use std::time::Duration;
use usb::UsbContext;

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
    fn SetConsoleCP(wCodePageID: u32) -> i32;
}

mod cli;
mod commands;
mod config;
mod da_extension;
mod da_partition;
mod da_xflash;
mod frp;
mod kamakiri2;
mod libusb_filter;
mod paths;
mod sej;
mod seccfg;
mod vbmeta;
mod preloader;
mod usb;

#[derive(Debug, PartialEq, Clone)]
enum DeviceMode {
    Brom,
    Preloader,
    Unknown,
}

enum DeviceTransport {
    Usb,
    Serial(String),
}

fn detect_mode(vid: u16, pid: u16) -> DeviceMode {
    match config::DeviceType::from_vid_pid(vid, pid) {
        config::DeviceType::Brom => DeviceMode::Brom,
        config::DeviceType::Preloader | config::DeviceType::PreloaderVariant => {
            DeviceMode::Preloader
        }
        _ => DeviceMode::Unknown,
    }
}

/// 统一设备初始化入口 — 优先尝试串口 (Preloader)，失败后回退到 USB (BROM)
/// 
/// 刷机匣流程：串口握手 → 关闭看门狗 → 安装 libusb filter → 释放串口 → libusb 接管
fn smart_init(context: &UsbContext) -> Result<(preloader::Preloader, DeviceMode), String> {
    // 先尝试串口检测 (Preloader 模式) — 轮询等待 COM 端口出现
    info!("{}", "等待设备连接 (Preloader: 直接连接 / BROM: Vol+ + Vol- + Power)".yellow());

    let mut serial_attempt = 0;
    const MAX_SERIAL_ATTEMPTS: usize = 30;

    while serial_attempt < MAX_SERIAL_ATTEMPTS {
        serial_attempt += 1;

        // 快速扫描 COM 端口
        if let Some(port_name) = preloader::detect_serial_preloader() {
            info!("{}", format!("发现 Preloader 串口设备: {}", port_name).yellow());
            info!("{}", "正在通过串口连接设备...".yellow());
            let serial_transport = preloader::SerialPortTransport::new(&port_name, 115200)
                .map_err(|e| format!("打开串口失败: {}", e))?;
            let serial_device: Box<dyn preloader::BromTransport> = Box::new(serial_transport);
            let mut preloader_instance = preloader::Preloader::new(serial_device);
            if preloader_instance.init().unwrap_or(false) {
                info!("{}", "串口设备握手成功".green().bold());

                // 串口 init 完成后，安装 libusb filter 并切换到 USB 模式
                info!("正在安装 libusb-win32 filter...");
                match libusb_filter::install_libusb_filter() {
                    Ok(()) => {
                        info!("{}", "libusb filter 安装成功，切换到 USB 模式".green().bold());

                        // 释放串口连接
                        drop(preloader_instance);
                        debug!("串口已释放");

                        // 等待设备重枚举（释放串口后设备需要时间重新 enumerate）
                        info!("等待设备重枚举...");
                        std::thread::sleep(Duration::from_millis(500));

                        // 循环检测 libusb 设备（最多 10 秒）
                        const LIBUSB_WAIT_MS: u64 = 10_000;
                        const LIBUSB_RETRY_INTERVAL_MS: u64 = 200;
                        let max_retries = (LIBUSB_WAIT_MS / LIBUSB_RETRY_INTERVAL_MS) as usize;
                        let mut retry = 0;

                        while retry < max_retries {
                            retry += 1;

                            // 尝试打开设备（内部自动 detach + claim + endpoint 扫描）
                            match usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x0003) {
                                Ok(usb_device) => {
                                    info!(
                                        "  VID: {:04x}, PID: {:04x}, EP_OUT=0x{:02X}, EP_IN=0x{:02X}",
                                        usb_device.vid, usb_device.pid,
                                        usb_device.ep_out, usb_device.ep_in
                                    );
                                    info!("{}", "已切换到 libusb 模式".green().bold());
                                    return Ok((
                                        preloader::Preloader::new(Box::new(usb_device)),
                                        DeviceMode::Brom,
                                    ));
                                }
                                Err(e) => {
                                    if retry % 5 == 1 {
                                        debug!("libusb 连接尝试 {}/{}: {}", retry, max_retries, e);
                                    }
                                    std::thread::sleep(Duration::from_millis(LIBUSB_RETRY_INTERVAL_MS));
                                }
                            }
                        }

                        // filter 安装成功后不再回退串口（设备状态已变化，串口无法恢复）
                        return Err("libusb 接管失败：filter 安装成功但设备未枚举为 libusb".to_string());
                    }
                    Err(e) => {
                        warn!("libusb filter 安装失败: {}，继续使用串口", e);
                        // filter 失败，继续使用串口
                        info!("{}", "Preloader 模式连接成功".green().bold());
                        return Ok((preloader_instance, DeviceMode::Preloader));
                    }
                }
            }
            info!("串口握手失败，继续轮询...");
        }

        // 等待 1 秒后重试
        std::thread::sleep(Duration::from_secs(1));
    }

    info!("串口检测超时，尝试 USB 模式...");

    let mut no_device_count = 0;
    const MAX_NO_DEVICE_LOOPS: usize = 30;

    let usb_device = loop {
        match usb::UsbDevice::new(context) {
            Ok(d) => {
                break d;
            }
            Err(_) => {
                no_device_count += 1;
                if no_device_count == MAX_NO_DEVICE_LOOPS {
                    error!("{}", "设备未连接超时".red());
                    info!("请按住 音量+ + 音量- 插入 USB (BROM模式)");
                    info!("或直接插入 USB (Preloader模式)");
                    return Err("设备未连接，请重试".to_string());
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    };

    info!("  VID: {:04x}, PID: {:04x}", usb_device.vid, usb_device.pid);

    let mode = detect_mode(usb_device.vid, usb_device.pid);
    info!("  模式: {:?}", mode);

    info!("{}", "正在打开设备...".yellow());

    info!("{}", "连接成功".green().bold());
    let preloader = preloader::Preloader::new(Box::new(usb_device));
    Ok((preloader, mode))
}

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
        let (mut preloader, _mode) = smart_init(&usb_context).inspect_err(|e| {
            error!("{}", e);
        })?;
        let _ = preloader.jump_bl();
        return Ok(());
    }

    if cmd == "dump-preloader" {
        let usb_context = UsbContext::new().inspect_err(|e| {
            error!("{}", e);
        })?;

        let (mut preloader, _mode) = smart_init(&usb_context).inspect_err(|e| {
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

    let (mut preloader, mode) = smart_init(&usb_context).inspect_err(|e| {
        error!("{}", e);
    })?;

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

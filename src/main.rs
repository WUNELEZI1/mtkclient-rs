use clap::Parser;
use colored::Colorize;
use log::{error, info, warn};
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
mod da_xflash;
mod da_extension;
mod da_partition;
mod driver;
mod kamakiri2;
mod paths;
mod preloader;
mod usb;
mod usb_diag;

#[derive(Debug, PartialEq, Clone)]
enum DeviceMode {
    Brom,
    Preloader,
    Unknown,
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

fn smart_init(context: &UsbContext) -> Result<(usb::UsbDevice, DeviceMode), String> {
    use usb_diag::{diagnose_connection, print_connection_hint, UsbDiagState};

    info!("{}", "等待设备连接 (BROM: Vol+ + Vol- + Power)".yellow());

    let mut no_device_count = 0;
    let mut handshake_fail_count = 0;
    const MAX_NO_DEVICE_LOOPS: usize = 15;
    const MAX_HANDSHAKE_LOOPS: usize = 10;

    let usb_device = loop {
        // 诊断连接状态
        let diag = diagnose_connection();

        match usb::UsbDevice::new(context) {
            Ok(d) => {
                break d;
            }
            Err(_e) => {
                match diag {
                    UsbDiagState::NoDevice => {
                        no_device_count += 1;
                        if no_device_count == MAX_NO_DEVICE_LOOPS {
                            warn!("{}", "设备未连接，请重新插入".red());
                            print_connection_hint();
                        }
                        if no_device_count >= MAX_NO_DEVICE_LOOPS + 15 {
                            return Err("设备未连接超时，请检查 USB 线缆和设备状态".to_string());
                        }
                    }
                    UsbDiagState::WrongDriver => {
                        warn!("{}", "检测到设备但驱动异常 (可能需要安装 WinUSB)".red());
                        warn!("运行以下命令安装驱动: mtkclient install-drivers");
                        print_connection_hint();
                        std::thread::sleep(Duration::from_secs(2));
                    }
                    UsbDiagState::Preloader => {
                        warn!("{}", "检测到 Preloader 模式，需要 BROM 模式".red());
                        warn!("请按住 音量+ + 音量- 插入 USB 进入 BROM 模式");
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    UsbDiagState::EndpointError | UsbDiagState::HandshakeFailed => {
                        handshake_fail_count += 1;
                        if handshake_fail_count == MAX_HANDSHAKE_LOOPS {
                            warn!("{}", "设备通信异常，libusb 上下文可能已损坏".red());
                            warn!("请断开设备，重新运行程序");
                            print_connection_hint();
                        }
                        if handshake_fail_count >= MAX_HANDSHAKE_LOOPS + 5 {
                            return Err("设备握手失败超时，请重新运行程序".to_string());
                        }
                    }
                    UsbDiagState::Brom => {
                        // BROM 模式但连接失败，可能是临时问题，继续重试
                    }
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
    Ok((usb_device, mode))
}

fn handle_install_drivers(debug: bool, force: bool) {
    match driver::install_winusb_driver(debug, force) {
        Ok(_) => {}
        Err(e) => {
            error!("安装失败: {}", e);
        }
    }
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

    // 初始化 USB trace 日志（在 env_logger 之前，确保日志可用）
    usb::set_usb_log_enabled(cli.usb_log);

    env_logger::builder()
        .filter_level(app_config.log_level)
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

    if cli.check_driver {
        if driver::check_driver() {
            info!("WinUSB 驱动已就绪");
        } else {
            info!("未检测到 WinUSB 驱动，运行 install-drivers 安装");
        }
        return Ok(());
    }

    if cmd.is_empty() {
        commands::print_help();
        return Ok(());
    }

    if cmd == "install-drivers" {
        handle_install_drivers(cli.debug_mode, cli.force);
        return Ok(());
    }

    if cmd == "diagnose" {
        usb_diag::diagnose_and_report();
        return Ok(());
    }

    if cmd == "list-usb" {
        usb_diag::enumerate_usb_devices();
        return Ok(());
    }

    if let Some(ref input_file) = cli.no_device {
        if cmd == "unlock"
            || (cmd == "da"
                && cli.args.first().map(|s| s.as_str()) == Some("seccfg")
                && cli.args.get(1).map(|s| s.as_str()) == Some("unlock"))
        {
            match da_xflash::seccfg_unlock_offline(input_file) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    error!("离线解锁失败: {}", e);
                    process::exit(1);
                }
            }
        }
        if cmd == "lock"
            || (cmd == "da"
                && cli.args.first().map(|s| s.as_str()) == Some("seccfg")
                && cli.args.get(1).map(|s| s.as_str()) == Some("lock"))
        {
            match da_xflash::seccfg_lock_offline(input_file) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    error!("离线锁定失败: {}", e);
                    process::exit(1);
                }
            }
        }
        return Err(format!("不支持的离线命令: {}", cmd).into());
    }

    if cmd == "dump-preloader" {
        let usb_context = UsbContext::new().inspect_err(|e| {
            error!("{}", e);
        })?;

        let (usb_device, _mode) = smart_init(&usb_context).inspect_err(|e| {
            error!("{}", e);
        })?;

        let mut preloader = preloader::Preloader::new(usb_device);

        if !preloader.init().unwrap_or(false) {
            return Err("设备初始化失败".into());
        }

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

    let (usb_device, mode) = smart_init(&usb_context).inspect_err(|e| {
        error!("{}", e);
    })?;

    let mut preloader = preloader::Preloader::new(usb_device);

    if !preloader.init().unwrap_or(false) {
        return Err("设备初始化失败".into());
    }

    let final_preloader_path = if let Some(ref path) = app_config.preloader_path {
        info!("使用指定的 preloader 文件: {}", path);
        path.clone()
    } else if mode == DeviceMode::Brom {
        String::new()
    } else {
        "preloader_k69v1_64_k419.bin".to_string()
    };

    let mut da = da_xflash::DAXFlash::new(&mut preloader);

    let sub_commands = parse_sub_commands(cmd, &cli.args);
    if sub_commands.len() > 1 {
        commands::handle_commands(
            &mut da,
            &mode,
            &app_config,
            cli.debug_mode,
            cli.quiet_dump,
            &final_preloader_path,
            &sub_commands,
            &usb_context,
        )?;
    } else {
        commands::handle_command(
            &mut da,
            &mode,
            &app_config,
            cli.debug_mode,
            cli.quiet_dump,
            &final_preloader_path,
            &usb_context,
        )?;
    }

    Ok(())
}

/// 解析位置参数，判断是否为批量模式
/// 例如: cargo run -- printgpt r boot boot.img e userdata
/// cmd = "printgpt", args = ["r", "boot", "boot.img", "e", "userdata"]
/// 识别: 当 args 中包含已知的命令关键词时，解析为批量命令列表
fn parse_sub_commands(first_cmd: &str, args: &[String]) -> Vec<(String, Vec<String>)> {
    let known_da_cmds = [
        "printgpt",
        "dumpbrom",
        "r",
        "read",
        "w",
        "write",
        "e",
        "erase",
        "vbmeta",
        "reset",
        "unlock",
        "lock",
        "da",
        "enable-adb-on-da",
    ];

    let mut result = Vec::new();
    let mut current_cmd = first_cmd.to_string();
    let mut current_args: Vec<String> = Vec::new();

    for arg in args {
        if known_da_cmds.contains(&arg.as_str()) {
            if !current_cmd.is_empty() {
                result.push((current_cmd.clone(), current_args.clone()));
            }
            current_cmd = arg.clone();
            current_args.clear();
        } else {
            current_args.push(arg.clone());
        }
    }

    if !current_cmd.is_empty() {
        result.push((current_cmd, current_args));
    }

    result
}

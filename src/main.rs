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
mod da_extension;
mod da_partition;
mod da_xflash;
mod driver;
mod kamakiri2;
mod paths;
mod preloader;
mod session;
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
    use usb_diag::{UsbDiagState, diagnose_connection, print_connection_hint};

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

    usb::set_usb_log_enabled(cli.usb_log);

    let log_level = if cli.quiet {
        log::LevelFilter::Warn
    } else if cli.debug_mode {
        log::LevelFilter::Debug
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

    // reset 命令：直接复位设备，清除 .state
    if cmd == "reset" {
        session::reset_session();
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
        let _ = preloader.jump_bl();
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

    let saved_vid = usb_device.vid;
    let saved_pid = usb_device.pid;

    let mut preloader = preloader::Preloader::new(usb_device);

    if !preloader.init().unwrap_or(false) {
        return Err("设备初始化失败".into());
    }

    // 检测是否可以复用 DA 会话
    let can_reuse = session::try_reuse_da_session(saved_vid, saved_pid);

    let final_preloader_path = if let Some(ref path) = app_config.preloader_path {
        info!("使用指定的 preloader 文件: {}", path);
        path.clone()
    } else if mode == DeviceMode::Brom {
        String::new()
    } else {
        "preloader_k69v1_64_k419.bin".to_string()
    };

    let mut da = da_xflash::DAXFlash::new(&mut preloader);

    if can_reuse {
        // 复用 DA 会话：跳过 BROM→DA 流程，直接执行命令
        info!("{}", "DA 会话已复用，跳过 BROM→DA 流程".yellow());
    } else {
        // 完整 BROM→DA 流程
        if mode == DeviceMode::Brom {
            if final_preloader_path.is_empty() {
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

        // 保存 DA 会话状态
        let hw_code = da.preloader.get_hw_code().unwrap_or(0x0707);
        let target_config = da.preloader.get_target_config().map(|c| c.raw).unwrap_or(0);
        session::save_da_session(saved_vid, saved_pid, hw_code, target_config);
    }

    if cli.debug_mode {
        if let Some(data) = da.get_emi_data() {
            let _ = std::fs::write("emi_debug.bin", data);
        }
        if let Some(data) = da.get_extensions_data() {
            let _ = std::fs::write("extensions_debug.bin", &data);
        }
    }

    let sub_commands = parse_sub_commands(cmd, &cli.args);
    if sub_commands.len() > 1 {
        // 批量模式需要完整的 handle_commands，这里简化处理
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

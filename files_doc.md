# MTKClient-RS 项目文件索引与代码文档

## 项目概述

MTKClient-RS 是 MediaTek 设备刷写工具的 Rust 实现，用于读写 MTK 设备的分区、解锁 bootloader 等操作。

---

## 项目结构

```
d:\test\ZybClient\
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── cli.rs
│   ├── commands.rs
│   ├── config.rs
│   ├── usb.rs
│   ├── preloader.rs
│   ├── da_xflash.rs
│   ├── da_extension.rs
│   ├── da_partition.rs
│   ├── usb_diag.rs
│   ├── driver.rs
│   ├── kamakiri2.rs
│   └── paths.rs
├── mtkclient-2.0.1/
├── mtkclient-2.1.4.1/
├── payloads/
├── usb_driver/
└── target/
```

---

## 文件索引

### 1. Cargo.toml
**路径**: `d:\test\ZybClient\Cargo.toml`

项目配置文件，定义了项目名称、版本、依赖和编译配置。

```toml
[package]
name = "mtkclient-rs"
version = "0.1.0"
edition = "2024"

[dependencies]
libusb1-sys = "0.7"
log = "0.4"
env_logger = "0.11"
colored = "2"
clap = { version = "4", features = ["derive"] }
serialport = "4.7"
sha2 = "0.10"
aes = "0.8"
cbc = "0.1"

[profile.dev]
opt-level = 0          # 不优化，编译最快
debug = true           # 保留调试信息
incremental = true     # 增量编译
codegen-units = 256    # 最大并行
lto = false            # 关闭链接时优化

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
```

---

### 2. src/main.rs
**路径**: `d:\test\ZybClient\src\main.rs`

程序主入口，处理命令行参数和设备初始化。

```rust
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

        match preloader.dump_preloader_payload(false) {
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
            &final_preloader_path,
            &sub_commands,
        )?;
    } else {
        commands::handle_command(
            &mut da,
            &mode,
            &app_config,
            cli.debug_mode,
            &final_preloader_path,
        )?;
    }

    Ok(())
}

fn parse_sub_commands(first_cmd: &str, args: &[String]) -> Vec<(String, Vec<String>)> {
    let known_da_cmds = [
        "printgpt", "dumpbrom", "r", "read", "w", "write", "e", "erase", "vbmeta", "reset",
        "unlock", "lock", "da", "enable-adb-on-da",
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
```

---

### 3. src/cli.rs
**路径**: `d:\test\ZybClient\src\cli.rs`

命令行参数定义，使用 clap 库。

```rust
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mtkclient")]
#[command(about = "MTKClient Rust 版本 - MTK 设备刷写工具", long_about = None)]
pub struct Cli {
    #[arg(long = "da2", help = "指定 DA2 文件路径")]
    pub da2_path: Option<String>,

    #[arg(long = "preloader", help = "指定 preloader 文件路径")]
    pub preloader_path: Option<String>,

    #[arg(long = "loader", help = "指定 DA loader 文件路径")]
    pub loader_path: Option<String>,

    #[arg(long = "parttype", help = "指定分区类型")]
    pub parttype: Option<String>,

    #[arg(long = "offset", help = "指定偏移地址")]
    pub offset: Option<u64>,

    #[arg(long = "length", help = "指定长度")]
    pub length: Option<u64>,

    #[arg(long = "sector", help = "指定扇区号")]
    pub sector: Option<u32>,

    #[arg(long = "sectors", help = "指定扇区数")]
    pub sectors: Option<u32>,

    #[arg(long = "verify", help = "写入后校验")]
    pub verify: bool,

    #[arg(
        long = "debug-mode",
        default_value_t = false,
        help = "启用调试模式（详细日志 + dump 文件）"
    )]
    pub debug_mode: bool,

    #[arg(long = "check-driver", action = clap::ArgAction::SetTrue, help = "检查 WinUSB 驱动状态")]
    pub check_driver: bool,

    #[arg(
        long = "force",
        default_value_t = false,
        help = "强制安装驱动（跳过驱动检查）"
    )]
    pub force: bool,

    #[arg(
        long = "no-device",
        help = "离线模式：不连接设备，直接处理 seccfg 文件"
    )]
    pub no_device: Option<String>,

    #[arg(
        help = "要执行的命令",
        long_help = "可用命令:\n\
          install-drivers - 自动安装 WinUSB 驱动（需要管理员权限）\n\
          printgpt        - 打印 GPT 分区表\n\
          dump-preloader  - 从 RAM 提取 Preloader\n\
          dumpbrom        - 提取 BROM 到文件\n\
          r <分区> <文件> - 读取分区到文件\n\
          w <分区> <文件> - 写入文件到分区\n\
          e <分区>       - 擦除分区\n\
          vbmeta <模式>  - 修补 vbmeta 分区\n\
          unlock          - 解锁 Bootloader\n\
          lock            - 锁定 Bootloader\n\
          reset           - 重置设备\n\
          enable-adb-on-da - 在 DA 模式下开启 ADB\n\
\n\
          离线模式:\n\
          unlock --no-device <seccfg文件> - 离线解锁 seccfg\n\
          lock --no-device <seccfg文件>   - 离线锁定 seccfg\n\
\n\
          批量模式（一次连接执行多个命令）:\n\
          mtkclient-rs printgpt r boot boot.img e userdata"
    )]
    pub command: Option<String>,

    #[arg(help = "命令参数（批量模式下每个子命令的额外参数）")]
    pub args: Vec<String>,
}
```

---

### 4. src/config.rs
**路径**: `d:\test\ZybClient\src\config.rs`

配置模块，包含设备类型、芯片配置、目标配置等常量和结构体。

```rust
pub const MEDIATEK_VID: u16 = 0x0E8D;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Brom,
    Preloader,
    PreloaderVariant,
    Unknown,
}

impl DeviceType {
    pub fn from_vid_pid(vid: u16, pid: u16) -> Self {
        if vid != MEDIATEK_VID {
            return DeviceType::Unknown;
        }
        match pid {
            0x0003 | 0xF200 | 0xD1E9 | 0xD1E2 | 0xD1EC | 0xD1DD => DeviceType::Brom,
            0x2000 => DeviceType::Preloader,
            0x2001 => DeviceType::PreloaderVariant,
            _ => DeviceType::Unknown,
        }
    }

    pub fn is_brom(&self) -> bool {
        matches!(self, DeviceType::Brom)
    }

    pub fn is_preloader(&self) -> bool {
        matches!(self, DeviceType::Preloader | DeviceType::PreloaderVariant)
    }
}

#[derive(Debug, Clone)]
pub struct DeviceConfig {
    pub vid: u16,
    pub pid: u16,
    pub device_type: DeviceType,
    pub name: &'static str,
    pub description: &'static str,
}

pub const SUPPORTED_DEVICES: &[DeviceConfig] = &[
    DeviceConfig {
        vid: MEDIATEK_VID,
        pid: 0x0003,
        device_type: DeviceType::Brom,
        name: "MTK BROM",
        description: "MediaTek Boot ROM mode",
    },
    DeviceConfig {
        vid: MEDIATEK_VID,
        pid: 0x2000,
        device_type: DeviceType::Preloader,
        name: "MTK Preloader",
        description: "MediaTek Preloader mode",
    },
    DeviceConfig {
        vid: MEDIATEK_VID,
        pid: 0x2001,
        device_type: DeviceType::PreloaderVariant,
        name: "MTK Preloader Variant",
        description: "MediaTek Preloader variant mode",
    },
];

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub log_level: log::LevelFilter,
    pub da2_path: Option<String>,
    pub preloader_path: Option<String>,
    pub loader_path: Option<String>,
    pub parttype: Option<String>,
    pub offset: Option<u64>,
    pub length: Option<u64>,
    pub sector: Option<u32>,
    pub sectors: Option<u32>,
    pub verify: bool,
    pub command: Option<String>,
    pub cmd_args: Vec<String>,
}

impl AppConfig {
    pub fn from_cli(cli: &crate::cli::Cli) -> Self {
        let log_level = if cli.debug_mode {
            log::LevelFilter::Trace
        } else {
            log::LevelFilter::Info
        };

        AppConfig {
            log_level,
            da2_path: cli.da2_path.clone(),
            preloader_path: cli.preloader_path.clone(),
            loader_path: cli.loader_path.clone(),
            parttype: cli.parttype.clone(),
            offset: cli.offset,
            length: cli.length,
            sector: cli.sector,
            sectors: cli.sectors,
            verify: cli.verify,
            command: cli.command.clone(),
            cmd_args: cli.args.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ChipConfig {
    pub hw_code: u16,
    pub name: &'static str,
    pub description: &'static str,
    pub loader: &'static str,
    pub var1: u8,
    pub watchdog: u32,
    pub uart: u32,
    pub brom_payload_addr: u32,
    pub da_payload_addr: u32,
    pub pl_payload_addr: u32,
    pub gcpu_base: u32,
    pub sej_base: u32,
    pub dxcc_base: u32,
    pub cqdma_base: u32,
    pub ap_dma_mem: u32,
    pub send_ptr: (u32, u32),
    pub ctrl_buffer: u32,
    pub cmd_handler: u32,
    pub brom_register_access: (u32, u32),
    pub meid_addr: u32,
    pub socid_addr: u32,
    pub prov_addr: u32,
    pub misc_lock: u32,
    pub efuse_addr: u32,
    pub blacklist: &'static [(u32, u32)],
    pub blacklist_count: u32,
    pub ptr_da_bra: Option<u32>,
    pub ptr_send_addr: Option<u32>,
}

pub static CHIP_CONFIGS: &[ChipConfig] = &[
    ChipConfig {
        hw_code: 0x0707,
        name: "MT6768/MT6769",
        description: "Helio P65/G85 k68v1",
        loader: "mt6768_payload.bin",
        var1: 0x25,
        watchdog: 0x10007000,
        uart: 0x11002000,
        brom_payload_addr: 0x100A00,
        da_payload_addr: 0x201000,
        pl_payload_addr: 0x40200000,
        gcpu_base: 0x10050000,
        sej_base: 0x1000A000,
        dxcc_base: 0x10210000,
        cqdma_base: 0x10212000,
        ap_dma_mem: 0x110001A0,
        send_ptr: (0x10286C, 0xC190),
        ctrl_buffer: 0x00102A28,
        cmd_handler: 0x0000CF15,
        brom_register_access: (0xC598, 0xC650),
        meid_addr: 0x102AF8,
        socid_addr: 0x102B08,
        prov_addr: 0x1054F4,
        misc_lock: 0x1001A100,
        efuse_addr: 0x11CE0000,
        blacklist: &[(0x10282C, 0x0), (0x00105994, 0)],
        blacklist_count: 0x0000000A,
        ptr_da_bra: Some(0xC650),
        ptr_send_addr: Some(0xC190),
    },
    ChipConfig {
        hw_code: 0x0788,
        name: "MT6771",
        description: "Helio P60/P70 k71v1",
        loader: "mt6771_payload.bin",
        var1: 0x0A,
        watchdog: 0x10007000,
        uart: 0x11002000,
        brom_payload_addr: 0x100A00,
        da_payload_addr: 0x201000,
        pl_payload_addr: 0x40200000,
        gcpu_base: 0x10050000,
        sej_base: 0x1000A000,
        dxcc_base: 0x10210000,
        cqdma_base: 0x10212000,
        ap_dma_mem: 0x11000158,
        send_ptr: (0x102878, 0xDEBC),
        ctrl_buffer: 0x00102A80,
        cmd_handler: 0x0000EBE9,
        brom_register_access: (0xE2D0, 0xE388),
        meid_addr: 0x102B38,
        socid_addr: 0x102B48,
        prov_addr: 0x1065C0,
        misc_lock: 0x1001A100,
        efuse_addr: 0x11F10000,
        blacklist: &[(0x102834, 0x0), (0x106A60, 0x0)],
        blacklist_count: 0x0000000A,
        ptr_da_bra: None,
        ptr_send_addr: None,
    },
];

pub fn get_chip_config(hw_code: u16) -> Option<&'static ChipConfig> {
    CHIP_CONFIGS.iter().find(|c| c.hw_code == hw_code)
}

#[derive(Debug, Clone, Copy)]
pub struct TargetConfig {
    pub raw: u32,
    pub sbc: bool,
    pub sla: bool,
    pub daa: bool,
    pub swjtag: bool,
    pub epp: bool,
    pub cert: bool,
    pub memread: bool,
    pub memwrite: bool,
    pub cmd_c8: bool,
}

impl TargetConfig {
    pub fn from_raw(raw: u32) -> Self {
        TargetConfig {
            raw,
            sbc: (raw & 0x01) != 0,
            sla: (raw & 0x02) != 0,
            daa: (raw & 0x04) != 0,
            swjtag: (raw & 0x06) != 0,
            epp: (raw & 0x08) != 0,
            cert: (raw & 0x10) != 0,
            memread: (raw & 0x20) != 0,
            memwrite: (raw & 0x40) != 0,
            cmd_c8: (raw & 0x80) != 0,
        }
    }

    pub fn needs_bypass(&self) -> bool {
        self.sbc || self.sla || self.daa
    }

    pub fn format_info(&self) -> String {
        format!(
            "设备信息: 0x{:02X}\n  SBC: {} / SLA: {} / DAA: {}\n  Mem Read Auth: {} / Mem Write Auth: {}\n  Cmd 0xC8 blocked: {}",
            self.raw,
            if self.sbc { "True" } else { "False" },
            if self.sla { "True" } else { "False" },
            if self.daa { "True" } else { "False" },
            if self.memread { "True" } else { "False" },
            if self.memwrite { "True" } else { "False" },
            if self.cmd_c8 { "True" } else { "False" },
        )
    }
}
```

---

### 5. src/usb.rs
**路径**: `d:\test\ZybClient\src\usb.rs`

USB 通信模块，使用 libusb1-sys 直接与设备通信。

```rust
use crate::config::{DeviceType, SUPPORTED_DEVICES};
use log::{debug, info};
use std::time::Duration;

const LIBUSB_ERROR_TIMEOUT: i32 = -7;

pub struct UsbContext {
    ctx: *mut libusb1_sys::libusb_context,
}

impl UsbContext {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
            if libusb1_sys::libusb_init(&mut ctx) != 0 {
                return Err("libusb_init 失败".to_string());
            }
            Ok(UsbContext { ctx })
        }
    }

    pub fn as_ptr(&self) -> *mut libusb1_sys::libusb_context {
        self.ctx
    }
}

impl Drop for UsbContext {
    fn drop(&mut self) {
        unsafe {
            if !self.ctx.is_null() {
                libusb1_sys::libusb_exit(self.ctx);
                self.ctx = std::ptr::null_mut();
            }
        }
    }
}

pub struct UsbDevice {
    handle: *mut libusb1_sys::libusb_device_handle,
    pub vid: u16,
    pub pid: u16,
    device_type: DeviceType,
    ep_out: u8,
    ep_in: u8,
    ep_out_max_packet_size: u16,
    timeout: Duration,
}

impl UsbDevice {
    pub fn new(context: &UsbContext) -> Result<Self, String> {
        let ctx = context.as_ptr();
        unsafe {
            let mut handle = std::ptr::null_mut();
            let mut found_device_type = DeviceType::Unknown;
            for dev_config in SUPPORTED_DEVICES {
                handle = libusb1_sys::libusb_open_device_with_vid_pid(
                    ctx,
                    dev_config.vid,
                    dev_config.pid,
                );
                if !handle.is_null() {
                    info!(
                        "[USB] 已连接: {} - {} (VID:0x{:04X} PID:0x{:04X})",
                        dev_config.name, dev_config.description, dev_config.vid, dev_config.pid
                    );
                    found_device_type = dev_config.device_type;
                    break;
                }
            }
            if handle.is_null() {
                return Err("未找到支持的设备".into());
            }

            libusb1_sys::libusb_detach_kernel_driver(handle, 1);
            if libusb1_sys::libusb_claim_interface(handle, 1) != 0 {
                return Err("claim_interface 1 failed".into());
            }
            libusb1_sys::libusb_detach_kernel_driver(handle, 0);
            let _ = libusb1_sys::libusb_claim_interface(handle, 0);

            let device = libusb1_sys::libusb_get_device(handle);
            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            libusb1_sys::libusb_get_device_descriptor(device, &mut desc);

            debug!("[USB] scanning endpoints...");
            let mut config_ptr: *const libusb1_sys::libusb_config_descriptor = std::ptr::null();
            let mut ep_out_addr: u8 = 0x01;
            let mut ep_in_addr: u8 = 0x81;
            let mut ep_out_max_pkt: u16 = 512;
            let ret = libusb1_sys::libusb_get_active_config_descriptor(device, &mut config_ptr);
            if ret != 0 || config_ptr.is_null() {
                info!(
                    "[USB] WARNING: get_active_config_descriptor failed (ret={}), trying known combos...",
                    ret
                );
            } else {
                let config = &*config_ptr;
                for i in 0..config.bNumInterfaces as isize {
                    let iface = &*config.interface.wrapping_add(i as usize);
                    for j in 0..iface.num_altsetting {
                        let alt = &*iface.altsetting.wrapping_add(j as usize);
                        debug!(
                            "[USB] interface {} altsetting {} num_endpoints={}",
                            i, j, alt.bNumEndpoints
                        );
                        for k in 0..alt.bNumEndpoints as isize {
                            let ep = &*alt.endpoint.wrapping_add(k as usize);
                            let addr = ep.bEndpointAddress;
                            let dir = if addr & 0x80 != 0 { "IN" } else { "OUT" };
                            let ep_type = match ep.bmAttributes & 0x03 {
                                0 => "Control",
                                1 => "Isochronous",
                                2 => "Bulk",
                                3 => "Interrupt",
                                _ => "Unknown",
                            };
                            debug!(
                                "[USB]   EP: 0x{:02X} dir={} type={} size={}",
                                addr, dir, ep_type, ep.wMaxPacketSize
                            );
                            if dir == "OUT" && ep_type == "Bulk" {
                                ep_out_addr = addr;
                                ep_out_max_pkt = ep.wMaxPacketSize;
                            }
                            if dir == "IN" && ep_type == "Bulk" {
                                ep_in_addr = addr;
                            }
                        }
                    }
                }
                libusb1_sys::libusb_free_config_descriptor(config_ptr);
            }

            info!(
                "[USB] EP_OUT=0x{:02X} wMaxPacketSize={} EP_IN=0x{:02X}",
                ep_out_addr, ep_out_max_pkt, ep_in_addr
            );

            Ok(UsbDevice {
                handle,
                vid: desc.idVendor,
                pid: desc.idProduct,
                device_type: found_device_type,
                ep_out: ep_out_addr,
                ep_in: ep_in_addr,
                ep_out_max_packet_size: ep_out_max_pkt,
                timeout: Duration::from_millis(1000),
            })
        }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        if data.is_empty() {
            unsafe {
                let mut transferred: i32 = 0;
                let ret = libusb1_sys::libusb_bulk_transfer(
                    self.handle,
                    self.ep_out,
                    std::ptr::null_mut(),
                    0,
                    &mut transferred,
                    self.timeout.as_millis() as u32,
                );
                if ret != 0 {
                    return Err(format!("write ZLP err {}", ret));
                }
            }
            return Ok(0);
        }

        unsafe {
            let mut transferred: i32 = 0;
            let ret = libusb1_sys::libusb_bulk_transfer(
                self.handle,
                self.ep_out,
                data.as_ptr() as *mut u8,
                data.len() as i32,
                &mut transferred,
                self.timeout.as_millis() as u32,
            );
            if ret != 0 {
                return Err(format!(
                    "write err {} (transferred={}/{})",
                    ret,
                    transferred,
                    data.len()
                ));
            }
            Ok(transferred as usize)
        }
    }

    pub fn ep_out_max_packet_size(&self) -> u16 {
        self.ep_out_max_packet_size
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let mut total = 0usize;
        let deadline = std::time::Instant::now() + self.timeout;
        debug!(
            "[USB READ] starting, buf_len={}, timeout={:?}ms",
            buf.len(),
            self.timeout.as_millis()
        );
        while total < buf.len() {
            let remaining = buf.len() - total;
            unsafe {
                let mut transferred: i32 = 0;
                let now = std::time::Instant::now();
                if now >= deadline {
                    debug!("[USB READ] deadline reached, breaking, total={}", total);
                    break;
                }
                let ms_left = match (deadline - now).checked_sub(Duration::ZERO) {
                    Some(d) => {
                        let ms = d.as_millis() as u32;
                        if ms == 0 { 1 } else { ms }
                    }
                    None => break,
                };
                debug!(
                    "[USB READ] calling bulk_transfer, remaining={}, timeout={}ms",
                    remaining, ms_left
                );
                let ret = libusb1_sys::libusb_bulk_transfer(
                    self.handle,
                    self.ep_in,
                    buf[total..].as_mut_ptr(),
                    remaining as i32,
                    &mut transferred,
                    ms_left,
                );
                debug!(
                    "[USB READ] bulk_transfer returned: ret={}, transferred={}",
                    ret, transferred
                );
                if ret != 0 && ret != LIBUSB_ERROR_TIMEOUT {
                    debug!("[USB READ] error, returning");
                    return Err(format!("read err {}", ret));
                }
                total += transferred as usize;
                if transferred == 0 {
                    if now < deadline {
                        debug!("[USB READ] transferred=0, retrying in 10ms");
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    } else {
                        debug!("[USB READ] transferred=0 at deadline, breaking");
                        break;
                    }
                }
            }
        }
        debug!("[USB READ] returning total={}", total);
        Ok(total)
    }

    pub fn ctrl_transfer_in(
        &mut self,
        rt: u8,
        r: u8,
        v: u16,
        i: u16,
        len: u16,
    ) -> Result<Vec<u8>, String> {
        debug!(
            "[CTRL] IN rt=0x{:02X} r=0x{:02X} v=0x{:04X} i=0x{:04X} len={}",
            rt, r, v, i, len
        );
        unsafe {
            let mut buf = vec![0u8; len as usize];
            let ret = libusb1_sys::libusb_control_transfer(
                self.handle, rt, r, v, i, buf.as_mut_ptr(), len,
                self.timeout.as_millis() as u32,
            );
            if ret < 0 {
                debug!("[CTRL] IN error: {}", ret);
                Err(format!("ctrl_in {}", ret))
            } else {
                buf.truncate(ret as usize);
                debug!(
                    "[CTRL] IN OK: {:02X?}",
                    &buf[..std::cmp::min(buf.len(), 16)]
                );
                Ok(buf)
            }
        }
    }

    pub fn clear_halt_in(&mut self) -> Result<(), String> {
        unsafe {
            let ret = libusb1_sys::libusb_clear_halt(self.handle, self.ep_in);
            if ret != 0 {
                Err(format!("clear_halt_in err {}", ret))
            } else {
                Ok(())
            }
        }
    }

    pub fn ctrl_transfer_out(
        &mut self,
        rt: u8,
        r: u8,
        v: u16,
        i: u16,
        data: &[u8],
    ) -> Result<(), String> {
        debug!(
            "[CTRL] OUT rt=0x{:02X} r=0x{:02X} v=0x{:04X} i=0x{:04X} data={:02X?}",
            rt, r, v, i, data
        );
        unsafe {
            let ret = libusb1_sys::libusb_control_transfer(
                self.handle, rt, r, v, i,
                data.as_ptr() as *mut u8, data.len() as u16,
                self.timeout.as_millis() as u32,
            );
            if ret < 0 {
                debug!("[CTRL] OUT error: {}", ret);
                Err(format!("ctrl_out {}", ret))
            } else {
                debug!("[CTRL] OUT OK: {} bytes sent", ret);
                Ok(())
            }
        }
    }

    pub fn set_timeout(&mut self, duration: Duration) {
        self.timeout = duration;
    }

    pub fn get_timeout(&self) -> Duration {
        self.timeout
    }

    pub fn do_handshake(&mut self) -> Result<bool, String> {
        let cmd = [0xA0u8, 0x0A, 0x50, 0x05];
        let maxinsize = 512u16;

        if !self.device_type.is_brom() {
            info!(
                "[USB] non-BROM PID (0x{:04X}), sending 0xA0 first",
                self.pid
            );
            let _ = self.write(&[0xA0]);
            std::thread::sleep(Duration::from_millis(10));
        }

        for attempt in 0..10 {
            if attempt > 0 {
                info!(
                    "[USB] handshake attempt {}/10, waiting 300ms...",
                    attempt + 1
                );
                std::thread::sleep(Duration::from_millis(300));
            }
            let orig_timeout = self.timeout;
            self.timeout = Duration::from_millis(50);
            let mut drain = [0u8; 64];
            loop {
                match self.read(&mut drain) {
                    Ok(n) if n > 0 => continue,
                    _ => break,
                }
            }
            self.timeout = orig_timeout;

            let mut ok = true;
            let mut i = 0;
            while i < 4 {
                if let Err(e) = self.write(&[cmd[i]]) {
                    info!("[USB] handshake write error at byte {}: {}", i, e);
                    ok = false;
                    break;
                }
                let mut r = vec![0u8; maxinsize as usize];
                match self.read(&mut r) {
                    Ok(n) if n > 0 => {
                        let last_byte = r[n - 1];
                        if last_byte == !cmd[i] {
                            i += 1;
                        } else {
                            info!(
                                "[USB] handshake mismatch at byte {}: got 0x{:02X}, expected 0x{:02X}",
                                i, last_byte, !cmd[i]
                            );
                            i = 0;
                        }
                    }
                    _ => {
                        info!("[USB] handshake read error at byte {}", i);
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                info!("Handshake OK");
                return Ok(true);
            }
        }
        Err("Handshake failed after 10 attempts".into())
    }

    pub fn close(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                libusb1_sys::libusb_release_interface(self.handle, 1);
                libusb1_sys::libusb_release_interface(self.handle, 0);
                libusb1_sys::libusb_close(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

impl Drop for UsbDevice {
    fn drop(&mut self) {
        self.close();
    }
}
```

---

### 6. src/preloader.rs
**路径**: `d:\test\ZybClient\src\preloader.rs`

Preloader 通信模块，处理与设备的低级通信协议。

```rust
use crate::config::{ChipConfig, get_chip_config};
use crate::usb::{UsbContext, UsbDevice};
use log::{debug, info};
use std::time::Duration;

pub struct Preloader {
    pub device: UsbDevice,
    pub is_preloader_mode: bool,
    pub hw_code: u16,
    pub chip: Option<&'static ChipConfig>,
}

impl Preloader {
    pub fn new(device: UsbDevice) -> Self {
        Preloader {
            device,
            is_preloader_mode: false,
            hw_code: 0,
            chip: None,
        }
    }

    fn ensure_chip(&self) -> Result<&'static ChipConfig, String> {
        self.chip.ok_or_else(|| "未识别的处理器型号".to_string())
    }

    fn setreg_disablewatchdogtimer(&mut self) -> Result<bool, String> {
        let chip = self.ensure_chip()?;
        if !self.echo(&[0xD4])? {
            return Ok(false);
        }
        if !self.echo(&chip.watchdog.to_be_bytes())? {
            return Ok(false);
        }
        if !self.echo(&1u32.to_be_bytes())? {
            return Ok(false);
        }
        let _ = self.rword()?;
        if !self.echo(&0x22000064u32.to_be_bytes())? {
            return Ok(false);
        }
        let _ = self.rword()?;
        Ok(true)
    }

    fn rword(&mut self) -> Result<u16, String> {
        let mut buf = [0u8; 2];
        self.device.read(&mut buf).map(|_| u16::from_be_bytes(buf))
    }

    fn rdword(&mut self) -> Result<u32, String> {
        let mut buf = [0u8; 4];
        self.device.read(&mut buf).map(|_| u32::from_be_bytes(buf))
    }

    fn rbyte(&mut self, len: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; len];
        let n = self.device.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn echo(&mut self, data: &[u8]) -> Result<bool, String> {
        let orig_timeout = self.device.get_timeout();
        self.device.set_timeout(Duration::from_millis(100));

        let result = match self.device.write(data) {
            Ok(_) => {
                let mut buf = vec![0u8; data.len()];
                match self.device.read(&mut buf) {
                    Ok(n) if n == data.len() => buf == data,
                    _ => false,
                }
            }
            Err(_) => false,
        };

        self.device.set_timeout(orig_timeout);
        Ok(result)
    }

    pub fn echo_debug(&mut self, data: &[u8], tag: &str) -> Result<bool, String> {
        debug!("[ECHO:{}] sending {} bytes: {:02X?}", tag, data.len(), data);
        let result = self.echo(data);
        match &result {
            Ok(true) => debug!("[ECHO:{}] OK, data={:02X?}", tag, data),
            Ok(false) => debug!("[ECHO:{}] MISMATCH or short read", tag),
            Err(e) => debug!("[ECHO:{}] ERR: {}", tag, e),
        }
        result
    }

    pub fn get_hw_code(&mut self) -> Result<u16, String> {
        if !self.echo(&[0xFD])? {
            return Err("HW Code echo failed".into());
        }
        let val = self.rdword()?;
        Ok(((val >> 16) & 0xFFFF) as u16)
    }

    #[allow(dead_code)]
    pub fn read32_brom(&mut self, addr: u32, dwords: usize) -> Result<Vec<u32>, String> {
        if !self.echo(&[0xD1])? {
            return Err("read32: echo CMD failed".to_string());
        }
        if !self.echo(&addr.to_be_bytes())? {
            return Err("read32: echo addr failed".to_string());
        }
        if !self.echo(&(dwords as u32).to_be_bytes())? {
            return Err("read32: echo dwords failed".to_string());
        }
        let _status = self.rword()?;

        const PACKET_SIZE: usize = 512;
        let total_bytes = dwords * 4;
        let mut all_data = Vec::with_capacity(total_bytes);
        let mut remaining = total_bytes;

        while remaining > 0 {
            let chunk_size = if remaining > PACKET_SIZE {
                PACKET_SIZE
            } else {
                remaining
            };
            let mut buf = vec![0u8; chunk_size];

            let orig_timeout = self.device.get_timeout();
            self.device
                .set_timeout(std::time::Duration::from_millis(1000));
            let bytes_read = self
                .device
                .read(&mut buf)
                .map_err(|e| format!("read32: bulk read 失败: {}", e))?;
            self.device.set_timeout(orig_timeout);

            if bytes_read == 0 {
                return Err("read32: 读取 0 字节，设备无响应".to_string());
            }

            all_data.extend_from_slice(&buf[..bytes_read]);
            remaining = remaining.saturating_sub(bytes_read);
        }

        let mut _status2 = [0u8; 2];
        self.device
            .read(&mut _status2)
            .map_err(|e| format!("read32: status2 读取失败: {}", e))?;

        if all_data.len() < total_bytes {
            return Err(format!(
                "read32: 期望 {} 字节，实际 {} 字节",
                total_bytes,
                all_data.len()
            ));
        }

        let result: Vec<u32> = all_data
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();

        Ok(result)
    }

    pub fn send_da(
        &mut self,
        address: u32,
        _size: u32,
        sig_len: u32,
        dadata: &[u8],
    ) -> Result<bool, String> {
        let data_len = dadata.len() - sig_len as usize;
        let data = &dadata[..data_len];
        if !self.echo(&[0xD7])? {
            return Err("SEND_DA failed".into());
        }
        if !self.echo(&address.to_be_bytes())? {
            return Err("addr failed".into());
        }
        if !self.echo(&(data_len as u32).to_be_bytes())? {
            return Err("size failed".into());
        }
        if !self.echo(&sig_len.to_be_bytes())? {
            return Err("sig_len failed".into());
        }
        let status = self.rword()?;
        if status <= 0xFF {
            self.upload_data(data)?;
            return Ok(true);
        }
        Err(format!("DA status: 0x{:04X}", status))
    }

    fn upload_data(&mut self, data: &[u8]) -> Result<(), String> {
        let max_pkt = self.device.ep_out_max_packet_size() as usize;
        let mut pos = 0;
        while pos < data.len() {
            let sz = std::cmp::min(max_pkt, data.len() - pos);
            self.device.write(&data[pos..pos + sz])?;
            pos += sz;
        }
        self.device.write(&[])?;
        std::thread::sleep(Duration::from_millis(35));
        let _ = self.rword()?;
        let status = self.rword()?;
        if status <= 0xFF {
            Ok(())
        } else {
            Err(format!("upload: 0x{:04X}", status))
        }
    }

    pub fn jump_da(&mut self, addr: u32) -> Result<bool, String> {
        if !self.echo(&[0xD5])? {
            return Ok(false);
        }
        self.device.write(&addr.to_be_bytes())?;
        if self.rdword()? == addr && self.rword()? == 0 {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn jump_bl(&mut self) -> Result<bool, String> {
        if !self.echo(&[0xD6])? {
            return Ok(false);
        }
        let status = self.rword()?;
        if status <= 0xFF {
            let _status2 = self.rword()?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn brom_register_access(
        &mut self,
        address: u32,
        length: u32,
        data: Option<&[u8]>,
        check_status: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        let mode: u32 = if data.is_some() { 1 } else { 0 };
        let mut retries = 3;
        let mut ok = false;
        while retries > 0 {
            if self.echo(&[0xDA])? {
                ok = true;
                break;
            }
            retries -= 1;
            if retries > 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if !ok {
            return Err("echo DA failed after retries".into());
        }
        if !self.echo(&mode.to_be_bytes())? {
            return Err("mode failed".into());
        }
        if !self.echo(&address.to_be_bytes())? {
            return Err("addr failed".into());
        }
        if !self.echo(&length.to_be_bytes())? {
            return Err("len failed".into());
        }
        let mut st = [0u8; 2];
        self.device.read(&mut st)?;
        debug!("brom_reg status1: {:02X?}", st);
        if st != [0, 0] {
            return Err("status err".into());
        }
        if let Some(wdata) = data {
            self.device.write(wdata)?;
            if check_status {
                let mut st2 = [0u8; 2];
                self.device.read(&mut st2)?;
                debug!("brom_reg status3: {:02X?}", st2);
                if st2 != [0, 0] {
                    return Err("status2 err".into());
                }
            }
        } else {
            let rdata = self.rbyte(length as usize)?;
            debug!("brom_reg read data: {:02X?}", rdata);
            let mut st2 = [0u8; 2];
            self.device.read(&mut st2)?;
            debug!("brom_reg status2: {:02X?}", st2);
            return Ok(Some(rdata));
        }
        Ok(None)
    }

    pub fn get_blver(&mut self) -> Result<u8, String> {
        self.device.write(&[0xFE])?;
        let mut res = [0u8; 1];
        self.device.read(&mut res)?;
        Ok(res[0])
    }

    #[allow(dead_code)]
    pub fn reopen_device(&mut self, context: &UsbContext) -> Result<(), String> {
        info!("等待设备重枚举...");
        self.device.close();
        std::thread::sleep(Duration::from_millis(500));

        let max_retries = 20;
        for i in 0..max_retries {
            match UsbDevice::new(context) {
                Ok(dev) => {
                    info!(
                        "设备重新连接成功 (VID: {:04X}, PID: {:04X})",
                        dev.vid, dev.pid
                    );
                    self.device = dev;
                    self.is_preloader_mode = self.device.pid == 0x2000;
                    if !self.device.do_handshake()? {
                        if i < max_retries - 1 {
                            std::thread::sleep(Duration::from_millis(200));
                            continue;
                        }
                        return Err("重枚举后握手失败".into());
                    }
                    self.hw_code = self.get_hw_code()?;
                    self.chip = get_chip_config(self.hw_code);
                    return Ok(());
                }
                Err(_) => {
                    if i < max_retries - 1 {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        }
        Err("设备重枚举超时".into())
    }

    pub fn init(&mut self) -> Result<bool, String> {
        if !self.device.do_handshake()? {
            return Err("Handshake failed".into());
        }
        self.hw_code = self.get_hw_code()?;
        self.chip = get_chip_config(self.hw_code);
        info!("HW Code: 0x{:04X}", self.hw_code);
        if let Some(chip) = self.chip {
            info!("处理器: {} ({})", chip.name, chip.description);
        }
        info!("Disabling watchdog...");
        self.setreg_disablewatchdogtimer()?;
        let blver = self.get_blver()?;
        self.is_preloader_mode = blver != 0xFE;
        info!("Init done, BROM={}", !self.is_preloader_mode);

        Ok(true)
    }

    pub fn get_target_config(&mut self) -> Result<crate::config::TargetConfig, String> {
        if !self.echo(&[0xD8])? {
            return Err("GET_TARGET_CONFIG echo 失败".into());
        }
        let data = self.rbyte(6)?;
        if data.len() < 6 {
            return Err(format!(
                "GET_TARGET_CONFIG 返回数据不足: {} 字节",
                data.len()
            ));
        }
        let raw = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let _status = u16::from_be_bytes([data[4], data[5]]);

        let cfg = crate::config::TargetConfig::from_raw(raw);
        Ok(cfg)
    }
}

#[path = "kamakiri2.rs"]
mod kamakiri2;
```

---

### 7. src/driver.rs
**路径**: `d:\test\ZybClient\src\driver.rs`

驱动安装模块，用于自动安装 WinUSB 驱动。

```rust
use crate::paths::exe_relative_path;
use log::{debug, info};
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

macro_rules! debug_log {
    ($debug:expr, $($arg:tt)*) => {
        if $debug {
            debug!($($arg)*);
        }
    };
}

pub fn install_winusb_driver(debug: bool, force: bool) -> Result<(), String> {
    debug_log!(debug, "[DRV] install_winusb_driver start");

    if !force && check_driver() {
        info!("WinUSB 驱动已就绪");
        info!("使用 --force 可重新安装");
        return Ok(());
    }

    if let Some(com_port) = find_mediatek_com_port(debug) {
        info!("找到 MediaTek USB Port: {}", com_port);
        info!("正在关闭 Watchdog 稳定端口...");
        disable_watchdog_serial(&com_port, debug)?;
        info!("Watchdog 已关闭，等待端口稳定...");
        sleep(Duration::from_secs(2));
    } else {
        info!("未检测到 MediaTek COM 端口，跳过 Watchdog 关闭");
    }

    info!("安装 MediaTek BROM WinUSB 驱动...");

    install_driver_inf(debug)?;
    info!("  驱动已安装");

    install_certificates(debug)?;
    info!("  证书已导入");

    info!("");
    info!("安装完成");
    info!("");
    info!("重新连接设备：");
    info!("  1. 关机");
    info!("  2. 按住音量加 + 音量减，插入 USB");
    info!("  3. 等待 BROM 设备识别");

    Ok(())
}

fn find_mediatek_com_port(debug: bool) -> Option<String> {
    debug_log!(debug, "[DRV] searching for MediaTek COM port");
    let ports = serialport::available_ports().ok()?;

    for port in &ports {
        debug_log!(debug, "[DRV] checking port: {}", port.port_name);
        if port.port_name.to_lowercase().contains("mediatek") {
            return Some(port.port_name.clone());
        }
    }

    info!("serialport 枚举未找到 MediaTek 端口，尝试 wmic...");
    if let Ok(output) = Command::new("wmic")
        .args(["path", "Win32_SerialPort", "get", "DeviceID,Name"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.to_lowercase().contains("mediatek") {
                if let Some(start) = line.find("COM") {
                    if let Some(end) = line[start..].find(|c: char| !c.is_alphanumeric()) {
                        return Some(line[start..start + end].to_string());
                    } else {
                        return Some(line[start..].to_string());
                    }
                }
            }
        }
    }

    None
}

fn disable_watchdog_serial(port_name: &str, debug: bool) -> Result<(), String> {
    debug_log!(
        debug,
        "[DRV] opening serial port {} to disable watchdog",
        port_name
    );

    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(1000))
        .open()
        .map_err(|e| format!("无法打开串口 {}: {}", port_name, e))?;

    let watchdog_disable_cmd: &[u8] = &[0xA0];
    port.write(watchdog_disable_cmd)
        .map_err(|e| format!("发送 Watchdog 关闭命令失败: {}", e))?;

    let mut buf = [0u8; 64];
    let _ = port.read(&mut buf);

    debug_log!(debug, "[DRV] watchdog disable command sent successfully");
    Ok(())
}

fn install_driver_inf(debug: bool) -> Result<(), String> {
    let inf_path = exe_relative_path("usb_driver/MediaTek_USB_Port.inf");

    if !inf_path.exists() {
        return Err(format!("INF file not found: {:?}", inf_path));
    }

    let inf_str = inf_path.to_str().unwrap().replace('/', "\\");
    debug_log!(debug, "[DRV] installing INF: {}", inf_str);

    let status = Command::new("pnputil")
        .args(["/add-driver", &inf_str, "/install"])
        .status()
        .map_err(|e| format!("pnputil failed: {}", e))?;

    if !status.success() {
        return Err("pnputil installation failed".to_string());
    }

    debug_log!(debug, "[DRV] pnputil success");
    Ok(())
}

fn install_certificates(debug: bool) -> Result<(), String> {
    let cat_path = exe_relative_path("usb_driver/MediaTek_USB_Port.cat");

    if !cat_path.exists() {
        return Err(format!("Certificate file not found: {:?}", cat_path));
    }

    let cat_str = cat_path.to_str().unwrap();
    debug_log!(debug, "[DRV] importing certificate: {}", cat_str);

    let _ = Command::new("certutil")
        .args(["-add-store", "Root", cat_str])
        .status();
    let _ = Command::new("certutil")
        .args(["-add-store", "TrustedPublisher", cat_str])
        .status();

    Ok(())
}

pub fn check_driver() -> bool {
    let output = Command::new("pnputil").args(["/enum-drivers"]).output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.contains("MediaTek") && (stdout.contains("WinUSB") || stdout.contains("oem"))
        }
        Err(_) => false,
    }
}
```

---

### 8. src/usb_diag.rs
**路径**: `d:\test\ZybClient\src\usb_diag.rs`

USB 诊断模块，用于检测设备和驱动状态。

```rust
use crate::config::DeviceType;
use log::{debug, info, warn};

/// USB 设备状态
#[derive(Debug, Clone, PartialEq)]
pub enum UsbDiagState {
    /// 未检测到任何 MediaTek 设备
    NoDevice,
    /// 检测到设备但驱动异常
    WrongDriver,
    /// 检测到 Preloader 模式
    Preloader,
    /// 检测到 BROM 模式
    Brom,
    /// 端点通信异常
    EndpointError,
    /// 设备连接但握手失败
    HandshakeFailed,
}

/// 扫描系统中的 MediaTek 设备
pub fn scan_mediatek_devices() -> Vec<(u16, u16, bool, String)> {
    let mut results = Vec::new();
    unsafe {
        let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut ctx) != 0 {
            warn!("[USB诊断] libusb_init 失败");
            return results;
        }

        let mut dev_list: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let dev_count = libusb1_sys::libusb_get_device_list(ctx, &mut dev_list);
        if dev_count < 0 {
            warn!("[USB诊断] get_device_list 失败: {}", dev_count);
            libusb1_sys::libusb_exit(ctx);
            return results;
        }

        for i in 0..dev_count as isize {
            let dev = *dev_list.wrapping_offset(i);
            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(dev, &mut desc) != 0 {
                continue;
            }

            let vid = desc.idVendor;
            let pid = desc.idProduct;

            // 只关心中 MediaTek 设备
            if vid != 0x0E8D {
                continue;
            }

            let dev_config = DeviceType::from_vid_pid(vid, pid);
            let is_brom = dev_config.is_brom();

            // 尝试打开设备获取驱动信息
            let mut handle: *mut libusb1_sys::libusb_device_handle = std::ptr::null_mut();
            let driver_info = if libusb1_sys::libusb_open(dev, &mut handle) == 0 {
                let claim_ret = libusb1_sys::libusb_claim_interface(handle, 1);
                let info = if claim_ret == 0 {
                    libusb1_sys::libusb_release_interface(handle, 1);
                    "WinUSB/libusb 正常".to_string()
                } else if claim_ret == -12 {
                    "接口未找到".to_string()
                } else {
                    format!("claim_interface 失败: {}", claim_ret)
                };
                libusb1_sys::libusb_close(handle);
                info
            } else {
                "设备被其他驱动占用".to_string()
            };

            results.push((vid, pid, is_brom, driver_info));
        }

        libusb1_sys::libusb_free_device_list(dev_list, 1);
        libusb1_sys::libusb_exit(ctx);
    }
    results
}

/// 诊断 USB 连接状态
pub fn diagnose_connection() -> UsbDiagState {
    let devices = scan_mediatek_devices();
    if devices.is_empty() {
        return UsbDiagState::NoDevice;
    }
    for (vid, pid, is_brom, driver_info) in &devices {
        debug!("[USB诊断] VID={:04X} PID={:04X} BROM={} 驱动={}", vid, pid, is_brom, driver_info);
        if driver_info.contains("被其他驱动占用") {
            return UsbDiagState::WrongDriver;
        }
        if !is_brom {
            return UsbDiagState::Preloader;
        }
        return UsbDiagState::Brom;
    }
    UsbDiagState::NoDevice
}

/// 打印连接提示
pub fn print_connection_hint() {
    info!("");
    info!("设备连接提示:");
    info!("  1. 确保设备已关机");
    info!("  2. BROM 模式: 按住 音量+ + 音量减，插入 USB");
    info!("  3. Preloader 模式: 不要按任何键，直接插入 USB");
    info!("  4. 如果已连接但无响应，按住电源键 10 秒重置");
    info!("  5. 运行 'check-driver' 确认 WinUSB 驱动已安装");
    info!("");
}

/// 枚举 USB 设备
pub fn enumerate_usb_devices() {
    info!("扫描 USB 设备...");
    unsafe {
        let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut ctx) != 0 {
            info!("libusb 初始化失败");
            return;
        }

        let mut dev_list: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let dev_count = libusb1_sys::libusb_get_device_list(ctx, &mut dev_list);
        if dev_count < 0 {
            info!("获取设备列表失败: {}", dev_count);
            libusb1_sys::libusb_exit(ctx);
            return;
        }

        info!("找到 {} 个 USB 设备", dev_count);

        for i in 0..dev_count as isize {
            let dev = *dev_list.wrapping_offset(i);
            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(dev, &mut desc) != 0 {
                continue;
            }

            let vid = desc.idVendor;
            let pid = desc.idProduct;

            let is_mtk = vid == 0x0E8D;
            let dev_config = DeviceType::from_vid_pid(vid, pid);

            let mode = if is_mtk {
                if dev_config.is_brom() {
                    "BROM"
                } else if dev_config.is_preloader() {
                    "Preloader"
                } else {
                    "未知模式"
                }
            } else {
                ""
            };

            if is_mtk {
                info!("  [MediaTek] Bus {} Device {}: VID={:04X} PID={:04X} 模式={}",
                    libusb1_sys::libusb_get_bus_number(dev),
                    libusb1_sys::libusb_get_device_address(dev),
                    vid, pid, mode);
            } else {
                debug!("  Bus {} Device {}: VID={:04X} PID={:04X}",
                    libusb1_sys::libusb_get_bus_number(dev),
                    libusb1_sys::libusb_get_device_address(dev),
                    vid, pid);
            }
        }

        libusb1_sys::libusb_free_device_list(dev_list, 1);
        libusb1_sys::libusb_exit(ctx);
    }
}

/// 综合诊断报告
pub fn diagnose_and_report() {
    info!("=== USB 连接诊断 ===");
    info!("");

    let devices = scan_mediatek_devices();
    if devices.is_empty() {
        warn!("未检测到任何 MediaTek USB 设备");
        info!("");
        info!("请检查:");
        info!("  1. USB 线缆是否连接良好");
        info!("  2. 设备是否已关机");
        info!("  3. 是否已进入 BROM 模式");
        return;
    }

    info!("检测到 {} 个 MediaTek 设备:", devices.len());
    for (vid, pid, is_brom, driver_info) in &devices {
        let mode = if *is_brom { "BROM" } else { "Preloader" };
        info!("  VID={:04X} PID={:04X} 模式={} 驱动={}", vid, pid, mode, driver_info);
    }
    info!("");

    let mut has_wrong_driver = false;
    let mut has_correct_driver = false;
    for (_, _, _, driver_info) in &devices {
        if driver_info.contains("WinUSB/libusb 正常") {
            has_correct_driver = true;
        } else {
            has_wrong_driver = true;
        }
    }

    if has_correct_driver && !has_wrong_driver {
        info!("驱动状态: WinUSB/libusb 正常");
    } else if has_wrong_driver {
        warn!("驱动状态异常: 设备未安装 WinUSB 驱动");
        info!("请运行以下命令安装驱动: mtkclient install-drivers");
    }

    info!("");
    info!("=== 诊断完成 ===");
}
```

---

## 项目总结

MTKClient-RS 是一个 MediaTek 设备读写工具的 Rust 实现，主要功能包括：

1. **USB 通信** - 通过 libusb 与设备进行通信
2. **设备握手** - 建立与设备的通信会话
3. **分区操作** - 读取、写入、擦除设备分区
4. **Bootloader 解锁/锁定** - 通过修改 seccfg 实现
5. **驱动安装** - 自动安装 WinUSB 驱动
6. **USB 诊断** - 检测设备和驱动状态

核心模块：
- `main.rs` - 程序入口，处理命令行参数
- `config.rs` - 设备配置和芯片参数
- `usb.rs` - 底层 USB 通信
- `preloader.rs` - Preloader 协议实现
- `da_xflash.rs` - DA 加载和分区操作
- `commands.rs` - 命令处理逻辑
- `driver.rs` - 驱动安装工具
- `usb_diag.rs` - 设备诊断功能

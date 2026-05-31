use libloading::{Library, Symbol};
use log::{debug, info, warn};
use std::path::Path;
use std::process::Command;

macro_rules! debug_log {
    ($debug:expr, $($arg:tt)*) => {
        if $debug {
            debug!($($arg)*);
        }
    };
}

type ZadigDetectBootrom = unsafe extern "C" fn() -> i32;
type ZadigInstallEmbeddedDriver = unsafe extern "C" fn() -> i32;

/// 加载 zadig_rust.dll 并返回 Library 句柄和函数指针
fn load_zadig_lib() -> Result<
    (
        Library,
        Symbol<'static, ZadigDetectBootrom>,
        Symbol<'static, ZadigInstallEmbeddedDriver>,
    ),
    String,
> {
    let dll_path = Path::new("zadig_rust.dll");
    if !dll_path.exists() {
        return Err("zadig_rust.dll 未找到，请确保 DLL 与可执行文件在同一目录".to_string());
    }

    let lib = unsafe { Library::new(dll_path) }
        .map_err(|e| format!("加载 zadig_rust.dll 失败: {}", e))?;

    let detect: Symbol<'_, ZadigDetectBootrom> = unsafe {
        lib.get(b"zadig_detect_bootrom")
            .map_err(|e| format!("找不到 zadig_detect_bootrom: {}", e))?
    };
    let install: Symbol<'_, ZadigInstallEmbeddedDriver> = unsafe {
        lib.get(b"zadig_install_embedded_driver")
            .map_err(|e| format!("找不到 zadig_install_embedded_driver: {}", e))?
    };

    // 将 Symbol 的生命周期延长为 'static（安全：Library 句柄同时持有）
    let detect = unsafe {
        std::mem::transmute::<Symbol<'_, ZadigDetectBootrom>, Symbol<'static, ZadigDetectBootrom>>(
            detect,
        )
    };
    let install = unsafe {
        std::mem::transmute::<
            Symbol<'_, ZadigInstallEmbeddedDriver>,
            Symbol<'static, ZadigInstallEmbeddedDriver>,
        >(install)
    };

    Ok((lib, detect, install))
}

/// 安装 MediaTek BROM WinUSB 驱动
/// 通过 zadig_rust.dll 实现检测和安装
pub fn install_winusb_driver(debug: bool, force: bool) -> Result<(), String> {
    debug_log!(debug, "[DRV] install_winusb_driver start force={}", force);

    if !force && check_driver() {
        debug_log!(debug, "[DRV] WinUSB driver already installed, skip");
        info!("WinUSB 驱动已就绪");
        info!("使用 --force 可重新安装");
        return Ok(());
    }

    // 检查管理员权限
    if !is_admin() {
        debug_log!(debug, "[DRV] not admin, requesting elevation");
        info!("正在请求管理员权限...");
        return rerun_as_admin();
    }

    // 加载 zadig_rust.dll
    let (_lib, detect, install) = load_zadig_lib()?;

    if let Some(com_port) = find_mediatek_com_port() {
        debug_log!(debug, "[DRV] detected COM port: {}", com_port);
        info!("找到 BROM COM 口: {}，正在关闭 Watchdog...", com_port);
        if let Err(e) = disable_watchdog_brom(&com_port) {
            warn!("关闭 Watchdog 失败: {}", e);
        } else {
            debug_log!(debug, "[DRV] watchdog disabled via COM port");
            info!("Watchdog 已关闭，设备稳定");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    } else {
        debug_log!(debug, "[DRV] no COM port found, probing WinUSB mode");
        let detected = unsafe { detect() };
        debug_log!(debug, "[DRV] zadig_detect_bootrom returned {}", detected);
        if detected == 0 {
            debug_log!(
                debug,
                "[DRV] device not in WinUSB mode, waiting for COM port"
            );
            info!("未检测到 MediaTek COM 端口，等待设备进入 BROM...");
            info!("请按住 音量+ + 音量- 插入 USB");
            loop {
                if let Some(port) = find_mediatek_com_port() {
                    debug_log!(debug, "[DRV] COM port appeared: {}", port);
                    info!("检测到 BROM COM 口: {}，正在关闭 Watchdog...", port);
                    if let Err(e) = disable_watchdog_brom(&port) {
                        warn!("关闭 Watchdog 失败: {}", e);
                    } else {
                        debug_log!(debug, "[DRV] watchdog disabled after wait");
                        info!("Watchdog 已关闭，设备稳定");
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    break;
                }
                debug_log!(debug, "[DRV] still waiting for COM port...");
                std::thread::sleep(std::time::Duration::from_millis(2000));
            }
        } else {
            debug_log!(debug, "[DRV] device already in WinUSB mode, skip COM wait");
        }
    }

    info!("安装 MediaTek BROM WinUSB 驱动...");
    let result = unsafe { install() };
    if result == 1 {
        debug_log!(debug, "[DRV] zadig install returned success");
        info!("驱动安装成功");
        info!("重新连接设备：");
        info!("  1. 关机");
        info!("  2. 按住音量加 + 音量减，插入 USB");
        info!("  3. 等待 BROM 设备识别");
        Ok(())
    } else {
        debug_log!(debug, "[DRV] zadig install returned {}", result);
        Err("驱动安装失败".to_string())
    }
}

/// 检查是否以管理员权限运行
fn is_admin() -> bool {
    use std::os::windows::process::CommandExt;
    let output = Command::new("net")
        .args(["session"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output();
    match output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// 以管理员权限重新启动当前程序
fn rerun_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法获取 exe 路径: {}", e))?;
    let args: Vec<String> = std::env::args().skip(1).collect();

    let status = Command::new("powershell")
        .args([
            "-Command",
            &format!(
                "Start-Process '{}' -ArgumentList '{}' -Verb RunAs -Wait",
                exe.display(),
                args.join(" ")
            ),
        ])
        .status()
        .map_err(|e| format!("提权失败: {}", e))?;

    if status.success() {
        std::process::exit(0);
    } else {
        Err("提权失败，请以管理员身份运行".to_string())
    }
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

/// 枚举 COM 口找 MediaTek BROM (VID_0E8D PID_0003)
fn find_mediatek_com_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    for p in &ports {
        if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
            && info.vid == 0x0E8D
            && info.pid == 0x0003
        {
            return Some(p.port_name.clone());
        }
    }
    None
}

/// 通过 serialport 关闭 BROM watchdog
/// 对齐 SerialPortTransport::do_handshake + WRITE32 关 WDT
fn disable_watchdog_brom(port_name: &str) -> Result<(), String> {
    let mut port = serialport::new(port_name, 115200)
        .timeout(std::time::Duration::from_millis(1000))
        .open()
        .map_err(|e| format!("无法打开 {}: {}", port_name, e))?;

    // 握手
    let cmd = [0xA0u8, 0x0A, 0x50, 0x05];
    for (i, cmd_byte) in cmd.iter().enumerate() {
        port.write(&[*cmd_byte])
            .map_err(|e| format!("握手写: {}", e))?;
        let mut resp = [0u8; 1];
        port.read_exact(&mut resp)
            .map_err(|e| format!("握手读: {}", e))?;
        if resp[0] != !*cmd_byte {
            return Err(format!(
                "握手失败 字节{}: 期望0x{:02X} 收到0x{:02X}",
                i, !*cmd_byte, resp[0]
            ));
        }
    }

    // WRITE32 关闭 watchdog
    let echo = |port: &mut dyn serialport::SerialPort, data: &[u8]| -> Result<(), String> {
        port.write_all(data).map_err(|e| format!("echo写: {}", e))?;
        let mut buf = vec![0u8; data.len()];
        port.read_exact(&mut buf)
            .map_err(|e| format!("echo读: {}", e))?;
        Ok(())
    };

    echo(&mut *port, &[0xD4])?;
    echo(&mut *port, &0x10007000u32.to_le_bytes())?;
    echo(&mut *port, &1u32.to_le_bytes())?;
    echo(&mut *port, &0x22000000u32.to_le_bytes())?;

    // 释放 COM 口，让 WinUSB 接管
    drop(port);
    Ok(())
}

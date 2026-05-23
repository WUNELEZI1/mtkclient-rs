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

/// 安装 MediaTek BROM WinUSB 驱动
/// 流程：关 Watchdog → 稳端口 → pnputil 装驱动 → certutil 导证书
pub fn install_winusb_driver(debug: bool, force: bool) -> Result<(), String> {
    debug_log!(debug, "[DRV] install_winusb_driver start");

    if !force && check_driver() {
        info!("WinUSB 驱动已就绪");
        info!("使用 --force 可重新安装");
        return Ok(());
    }

    // 步骤 1: 关闭 Watchdog 稳住 COM 端口（BROM 端口 5 秒超时）
    if let Some(com_port) = find_mediatek_com_port(debug) {
        info!("找到 MediaTek USB Port: {}", com_port);
        info!("正在关闭 Watchdog 稳定端口...");
        disable_watchdog_serial(&com_port, debug)?;
        info!("Watchdog 已关闭，等待端口稳定...");
        sleep(Duration::from_secs(2));
    } else {
        info!("未检测到 MediaTek COM 端口，跳过 Watchdog 关闭");
    }

    // 步骤 2: 安装驱动
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

/// 枚举 COM 端口找 MediaTek USB Port
fn find_mediatek_com_port(debug: bool) -> Option<String> {
    debug_log!(debug, "[DRV] searching for MediaTek COM port");
    // 使用 serialport crate 枚举可用端口
    let ports = serialport::available_ports().ok()?;

    for port in &ports {
        debug_log!(debug, "[DRV] checking port: {}", port.port_name);
        if port.port_name.to_lowercase().contains("mediatek") {
            return Some(port.port_name.clone());
        }
    }

    // serialport 可能返回空，fallback 到 wmic
    info!("serialport 枚举未找到 MediaTek 端口，尝试 wmic...");
    if let Ok(output) = Command::new("wmic")
        .args(["path", "Win32_SerialPort", "get", "DeviceID,Name"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.to_lowercase().contains("mediatek") {
                // 解析 COMx
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

/// 通过串口发送 0xA0 关闭 Watchdog
/// 对齐 C# 版 install-filter.exe 行为
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

    // 发送 0xA0 关闭 Watchdog
    let watchdog_disable_cmd: &[u8] = &[0xA0];
    port.write(watchdog_disable_cmd)
        .map_err(|e| format!("发送 Watchdog 关闭命令失败: {}", e))?;

    // 等待响应
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
        .args(["-addstore", "Root", cat_str])
        .status();
    let _ = Command::new("certutil")
        .args(["-addstore", "TrustedPublisher", cat_str])
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

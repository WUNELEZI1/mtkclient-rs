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

    // 检查管理员权限（pnputil 和 certutil 需要）
    if !is_admin() {
        info!("正在请求管理员权限...");
        return rerun_as_admin();
    }

    if !force && check_driver() {
        info!("WinUSB 驱动已就绪");
        info!("使用 --force 可重新安装");
        return Ok(());
    }

    // 步骤 1: 等待设备进入 BROM，关闭 Watchdog
    info!("请按住音量+和音量-，插入USB进入BROM模式...");

    let com_port = loop {
        if let Some(port) = find_mediatek_com_port(debug) {
            break port;
        }
        info!("未检测到 MediaTek COM 端口，等待设备进入 BROM...");
        info!("请按住 音量+ + 音量- 插入 USB");
        sleep(Duration::from_millis(2000));
    };

    info!("找到 MediaTek USB Port: {}", com_port);
    info!("正在关闭 Watchdog 稳定端口...");
    disable_watchdog_serial(&com_port, debug)?;
    info!("Watchdog 已关闭，等待端口稳定...");
    sleep(Duration::from_secs(2));

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

/// 通过串口关闭看门狗（完整 BROM 协议）
/// 对齐 serialport 版本 ZybFlashTool 的 setreg_disablewatchdogtimer
fn disable_watchdog_serial(port_name: &str, debug: bool) -> Result<(), String> {
    debug_log!(
        debug,
        "[DRV] opening serial port {} for BROM handshake",
        port_name
    );

    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(1000))
        .open()
        .map_err(|e| format!("无法打开串口 {}: {}", port_name, e))?;

    // 步骤 1: BROM 握手 — 对齐 Python Port.py:run_handshake
    // 逐字节发送 A0 0A 50 05，每字节回显取反
    let startcmd = [0xA0u8, 0x0A, 0x50, 0x05];
    info!("正在执行 BROM 握手...");
    for (i, cmd_byte) in startcmd.iter().enumerate() {
        port.write(&[*cmd_byte])
            .map_err(|e| format!("握手写字节 {}: {}", i, e))?;
        let mut response = [0u8; 1];
        port.read_exact(&mut response)
            .map_err(|e| format!("握手读字节 {}: {}", i, e))?;
        let expected = !*cmd_byte;
        if response[0] != expected {
            return Err(format!(
                "握手失败 字节 {}: 期望 0x{:02X}, 收到 0x{:02X}",
                i, expected, response[0]
            ));
        }
    }
    info!("  BROM 握手成功");

    // 步骤 2: 关闭看门狗 — WRITE32 命令
    // 对齐 serialport 版本的 setreg_disablewatchdogtimer
    info!("正在关闭看门狗...");

    // WRITE32 命令 = 0xD4
    echo(&mut port, &[0xD4])?;

    // 看门狗寄存器地址 0x10007000（小端序）
    echo(&mut port, &0x10007000u32.to_le_bytes())?;

    // count = 1（小端序）
    echo(&mut port, &1u32.to_le_bytes())?;

    // 看门狗禁用值 0x22000000（小端序）
    echo(&mut port, &0x22000000u32.to_le_bytes())?;

    info!("  看门狗已关闭");
    Ok(())
}

/// BROM echo 协议：发送 data，读回相同字节数并比对
fn echo(port: &mut Box<dyn serialport::SerialPort>, data: &[u8]) -> Result<bool, String> {
    use log::warn;

    port.write_all(data)
        .map_err(|e| format!("echo 写: {}", e))?;

    let mut buf = vec![0u8; data.len()];
    port.read_exact(&mut buf)
        .map_err(|e| format!("echo 读 {} 字节: {}", data.len(), e))?;

    if buf == data {
        Ok(true)
    } else {
        warn!("回显不匹配: 期望 {:02X?}, 收到 {:02X?}", data, buf);
        Ok(false)
    }
}

fn install_driver_inf(debug: bool) -> Result<(), String> {
    let inf_path = exe_relative_path("usb_driver/MediaTek_USB_Port.inf");

    if !inf_path.exists() {
        return Err(format!("INF file not found: {:?}", inf_path));
    }

    let inf_str = inf_path.to_str().unwrap().replace('/', "\\");
    debug_log!(debug, "[DRV] installing INF: {}", inf_str);

    let output = Command::new("pnputil")
        .args(["/add-driver", &inf_str, "/install"])
        .output()
        .map_err(|e| format!("pnputil failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    debug_log!(debug, "[DRV] pnputil stdout: {}", stdout.trim());
    if !stderr.is_empty() {
        debug_log!(debug, "[DRV] pnputil stderr: {}", stderr.trim());
    }

    // pnputil 对已存在的驱动返回 Already exists，也算成功
    if stdout.contains("successfully") || stdout.contains("Already exists") {
        debug_log!(debug, "[DRV] pnputil success (or already installed)");
        return Ok(());
    }

    Err(format!("pnputil installation failed: {}", stdout.trim()))
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

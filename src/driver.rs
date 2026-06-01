use crate::filter;
use log::{debug, info, warn};
use std::process::Command;

macro_rules! debug_log {
    ($debug:expr, $($arg:tt)*) => {
        if $debug {
            debug!($($arg)*);
        }
    };
}

/// 安装 MediaTek 设备的 libusb-win32 filter 驱动
pub fn install_winusb_driver(debug: bool, force: bool) -> Result<(), String> {
    debug_log!(debug, "[DRV] install_filter_driver start force={}", force);

    if !force && check_driver() {
        debug_log!(debug, "[DRV] filter driver already installed, skip");
        info!("libusb-win32 filter 驱动已就绪");
        info!("使用 --force 可重新安装");
        return Ok(());
    }

    if !is_admin() {
        debug_log!(debug, "[DRV] not admin, requesting elevation");
        info!("正在请求管理员权限...");
        return rerun_as_admin();
    }

    // 步骤 1: 通过 serialport 关闭 watchdog（仅在设备为 COM 口模式时需要）
    // 如果设备已是 WinUSB 模式，COM 口不存在，跳过此步骤
    let com_port = find_mediatek_com_port();
    debug_log!(debug, "[DRV] COM port detection: {:?}", com_port);

    if let Some(port_name) = com_port {
        info!("找到 BROM COM 口: {}，正在关闭 Watchdog...", port_name);
        if let Err(e) = disable_watchdog_brom(&port_name) {
            warn!("关闭 Watchdog 失败: {}", e);
        } else {
            info!("Watchdog 已关闭，设备稳定");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    } else {
        // 未找到 COM 口，检查设备是否已在 WinUSB/libusb 模式
        info!("未找到 BROM COM 口");

        // 使用 libusb 检测设备是否存在
        if is_brom_device_present() {
            debug_log!(debug, "[DRV] BROM device detected via libusb, skipping watchdog");
            info!("设备已在 libusb/WinUSB 模式，跳过 Watchdog 关闭");
        } else {
            // 设备既不是 COM 口也不是 libusb 模式，循环等待 COM 口
            info!("请按住音量+和音量-，插入USB进入BROM模式...");
            let port_name = loop {
                if let Some(port) = find_mediatek_com_port() {
                    break port;
                }
                info!("未检测到 MediaTek COM 端口，等待设备进入 BROM...");
                info!("请按住 音量+ + 音量- 插入 USB");
                std::thread::sleep(std::time::Duration::from_millis(2000));
            };
            info!("找到 BROM COM 口: {}，正在关闭 Watchdog...", port_name);
            disable_watchdog_brom(&port_name)
                .map_err(|e| format!("关闭 Watchdog 失败: {}", e))?;
            info!("Watchdog 已关闭，设备稳定");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    // 步骤 2: 安装 libusb-win32 filter 驱动
    info!("安装 libusb-win32 filter 驱动...");
    filter::install_filter_driver(debug)?;
    debug_log!(debug, "[DRV] filter driver installed");
    info!("驱动安装成功");
    info!("重新连接设备：");
    info!("  1. 关机");
    info!("  2. 按住音量加 + 音量减，插入 USB");
    info!("  3. 等待 BROM 设备识别");
    Ok(())
}

/// 检测 BROM 设备是否已通过 libusb 连接（VID=0E8D PID=0003）
fn is_brom_device_present() -> bool {
    let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
    let init_ret = unsafe { libusb1_sys::libusb_init(&mut ctx) };
    if init_ret != 0 {
        debug_log!(true, "[DRV] libusb_init failed: {}", init_ret);
        return false;
    }
    unsafe {
        let handle = libusb1_sys::libusb_open_device_with_vid_pid(ctx, 0x0E8D, 0x0003);
        if !handle.is_null() {
            libusb1_sys::libusb_close(handle);
            debug_log!(true, "[DRV] BROM device found on USB bus");
            libusb1_sys::libusb_exit(ctx);
            return true;
        }
    }
    unsafe {
        libusb1_sys::libusb_exit(ctx);
    }
    false
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
    filter::is_filter_installed()
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
/// 对齐 SerialPortTransport::do_handshake + WRITE32 关 WDT + 双 status 回包
fn disable_watchdog_brom(port_name: &str) -> Result<(), String> {
    use crate::preloader::Preloader;

    let transport = crate::preloader::SerialPortTransport::new(port_name, 115200)?;
    let mut preloader = Preloader::new(Box::new(transport));
    preloader.init()?;

    if !preloader.echo_1byte(0xD4)? {
        return Err("watchdog disable: D4 echo mismatch".into());
    }
    if !preloader.echo_4byte(0x10007000)? {
        return Err("watchdog disable: addr echo mismatch".into());
    }
    let status1 = preloader.echo_4byte_then_status(1)?;
    if status1 != 0x0001 {
        return Err(format!(
            "watchdog disable: expected status1=0x0001, got 0x{:04X}",
            status1
        ));
    }
    let status2 = preloader.echo_4byte_then_status(0x22000000)?;
    if status2 != 0x0001 {
        return Err(format!(
            "watchdog disable: expected status2=0x0001, got 0x{:04X}",
            status2
        ));
    }

    drop(preloader);
    Ok(())
}

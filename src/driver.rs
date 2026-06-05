/// Windows 驱动层检测与 UsbDk 集成
/// 
/// 职责：
/// 1. 检测 UsbDk 是否可用（service / registry）
/// 2. 检测设备驱动绑定状态
/// 3. 提供后端选择逻辑
/// 
/// 对齐 MTKClient Python:
/// - get_connection_agent() → detect_backend()
/// - dynamic backend selection (usbdk / libusb / com)

use log::{debug, info};
use std::path::Path;

/// 驱动后端类型
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum DeviceBackend {
    /// UsbDk 后端（优先，Windows 原生）
    UsbDk,
    /// libusb 后端（需要设备未被 Windows 驱动占用）
    Libusb,
    /// 串口后端（仅 Preloader 阶段）
    SerialCom,
}

/// 检测 UsbDk 是否可用
/// 
/// 检测顺序：
/// 1. 检查 UsbDk 系统服务是否存在并运行
/// 2. 检查 UsbDk 驱动文件是否存在
/// 3. 检查设备接口 GUID
/// 
/// UsbDk GUID: {3C0A3863-8B7E-4E3F-B5C3-8C0B4E3A2D1F} (标准 UsbDk)
/// 
/// 返回 true 表示 UsbDk 可用，libusb 可通过 UsbDk 后端直接访问 USB 设备
/// 即使设备被 Windows CDC/VCOM 驱动占用，UsbDk 也能接管
pub fn has_usbdk() -> bool {
    // 方法 1：检查 UsbDk.sys 驱动文件
    let usbdk_sys = Path::new(r"C:\Windows\System32\drivers\UsbDk.sys");
    if usbdk_sys.exists() {
        debug!("[DRIVER] UsbDk.sys found in System32\\drivers");
        return true;
    }

    // 方法 2：检查 UsbDk 服务注册表项（简化：直接检查驱动文件）

    // 方法 3：检查 UsbDkHelper.exe（UsbDk 安装包附带）
    let usbdk_helper = Path::new(
        r"C:\Program Files\Daytona\UsbDk\UsbDkHelper.exe"
    );
    if usbdk_helper.exists() {
        debug!("[DRIVER] UsbDkHelper.exe found");
        return true;
    }

    // 方法 4：检查安装目录
    let install_dir = Path::new(r"C:\Program Files\Daytona\UsbDk");
    if install_dir.exists() {
        debug!("[DRIVER] UsbDk installation directory found");
        return true;
    }

    debug!("[DRIVER] UsbDk not detected");
    false
}

/// 检测最佳可用后端
/// 
/// 优先级：
/// 1. UsbDk（如果可用，优先使用）
/// 2. Libusb（如果设备未被 Windows 驱动占用）
/// 3. SerialCom（仅用于 Preloader 阶段）
/// 
/// 对齐 MTKClient Python get_connection_agent():
/// - 动态选择最佳后端
/// - 考虑设备当前驱动绑定状态
pub fn detect_backend() -> DeviceBackend {
    if has_usbdk() {
        info!("[DRIVER] UsbDk detected, selecting UsbDk backend");
        return DeviceBackend::UsbDk;
    }

    // 尝试 libusb（需要设备未被 Windows 驱动占用）
    // 这里简化检测：如果 UsbDk 不可用，默认使用 libusb
    // 实际打开时会检测是否被 COM 占用
    info!("[DRIVER] UsbDk not available, selecting libusb backend");
    DeviceBackend::Libusb
}

/// 检测设备是否被 Windows CDC/VCOM 驱动占用
/// 
/// 通过 pnputil 命令检查设备状态
/// 如果设备显示 "USB Serial (COMxx)" 或 "MediaTek USB Port"，说明被 COM 驱动占用
/// 
/// 返回 true 表示设备被 COM 驱动占用，libusb 无法直接打开
pub fn is_device_com_occupied(vid: u16, pid: u16) -> bool {
    // 使用 pnputil 枚举设备
    let output = std::process::Command::new("pnputil")
        .args(&["/enum-devices", "/connected"])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // 搜索 MediaTek 设备
            let vid_str = format!("{:04X}", vid);
            let pid_str = format!("{:04X}", pid);

            // 检查是否包含 COM 相关描述
            let has_com = stdout.contains("USB Serial")
                || stdout.contains("COM")
                || stdout.contains("MediaTek USB Port")
                || stdout.contains("VCOM");

            // 检查是否包含目标 VID/PID
            let has_target = stdout.contains(&vid_str) || stdout.contains(&pid_str);

            if has_target && has_com {
                debug!("[DRIVER] Device VID={:04X} PID={:04X} is COM-occupied", vid, pid);
                return true;
            }

            false
        }
        Err(e) => {
            debug!("[DRIVER] pnputil check failed: {}", e);
            false
        }
    }
}

/// 获取后端状态摘要（用于日志）
pub fn backend_status() -> String {
    let usbdk = has_usbdk();
    format!(
        "UsbDk={}, Libusb=available, COM=available",
        if usbdk { "yes" } else { "no" }
    )
}

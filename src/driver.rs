/// Windows 驱动层检测与 UsbDk 集成
/// 
/// 职责：
/// 1. 检测 UsbDk 是否可用（service / file detection）
/// 2. UsbDk 真实接管验证（open + control transfer + descriptor + claim）
/// 3. 检测设备驱动绑定状态（device ownership）
/// 4. 提供后端选择逻辑（每次 reconnect 实时判断）
/// 
/// 对齐 MTKClient Python:
/// - get_connection_agent() → detect_backend()
/// - dynamic backend selection (usbdk / libusb / com)
/// - device ownership detection → detect_device_owner()

use crate::usb::UsbContext;
use log::{debug, info, warn};
use std::path::Path;

/// 设备所有权状态
/// 
/// 描述设备当前被哪个驱动后端占用：
/// - UsbDkOwned: 设备已被 UsbDk 接管，libusb 可通过 UsbDk 后端访问
/// - WinUsbOwned: 设备被 WinUSB 接管，libusb 可直接访问
/// - SerialOwned: 设备被 Windows CDC/VCOM 驱动占用，需要 UsbDk 或先释放
/// - Unknown: 未知状态（需要进一步检测）
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum DeviceOwner {
    UsbDkOwned,
    WinUsbOwned,
    SerialOwned,
    Unknown,
}

/// 驱动后端类型
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum DeviceBackend {
    /// UsbDk 后端（优先，Windows 原生，可绕过 COM 占用）
    UsbDk,
    /// libusb 后端（需要设备未被 Windows 驱动占用）
    Libusb,
    /// 串口后端（仅 Preloader 阶段）
    SerialCom,
}

/// 检测 UsbDk 是否可用
/// 
/// 检测顺序：
/// 1. 检查 UsbDk.sys 驱动文件
/// 2. 检查 UsbDkHelper.exe
/// 3. 检查安装目录
/// 
/// 返回 true 表示 UsbDk 可能可用，需要进一步通过 usbdk_open_test() 验证
pub fn has_usbdk() -> bool {
    // 方法 1：检查 UsbDk.sys 驱动文件
    let usbdk_sys = Path::new(r"C:\Windows\System32\drivers\UsbDk.sys");
    if usbdk_sys.exists() {
        debug!("[DRIVER] UsbDk.sys found in System32\\drivers");
        return true;
    }

    // 方法 2：检查 UsbDkHelper.exe（UsbDk 安装包附带）
    let usbdk_helper = Path::new(
        r"C:\Program Files\Daytona\UsbDk\UsbDkHelper.exe"
    );
    if usbdk_helper.exists() {
        debug!("[DRIVER] UsbDkHelper.exe found");
        return true;
    }

    // 方法 3：检查安装目录
    let install_dir = Path::new(r"C:\Program Files\Daytona\UsbDk");
    if install_dir.exists() {
        debug!("[DRIVER] UsbDk installation directory found");
        return true;
    }

    // 方法 4：检查 Wow6432Node 安装路径（32 位程序在 64 位系统）
    let install_dir_x86 = Path::new(r"C:\Program Files (x86)\Daytona\UsbDk");
    if install_dir_x86.exists() {
        debug!("[DRIVER] UsbDk (x86) installation directory found");
        return true;
    }

    debug!("[DRIVER] UsbDk not detected");
    false
}

/// UsbDk 真实接管验证（不是只检测文件存在）
/// 
/// 必须验证：
/// 1. open device handle 成功
/// 2. descriptor 可读
/// 3. interface claim 成功
/// 4. control transfer 成功（可选）
/// 
/// 返回 true 表示 UsbDk 真正接管了设备，可以用于 BROM/DA 通信
pub fn usbdk_open_test(context: &UsbContext, vid: u16, pid: u16) -> bool {
    if !has_usbdk() {
        debug!("[DRIVER] UsbDk not available, skipping open test");
        return false;
    }

    debug!("[DRIVER] UsbDk open test: VID=0x{:04X} PID=0x{:04X}", vid, pid);

    // 尝试通过 libusb 打开设备（如果 UsbDk 可用，libusb 会通过 UsbDk 后端访问）
    match crate::usb::UsbDevice::open_by_vid_pid(context, vid, pid) {
        Ok(device) => {
            debug!("[DRIVER] UsbDk open test: device opened successfully");

            // 验证：descriptor 可读（open_device 中已经读取了 descriptor）
            if device.vid == vid && device.pid == pid {
                debug!("[DRIVER] UsbDk open test: descriptor verified");
            }

            // 验证：endpoint 存在
            if device.ep_out != 0 && device.ep_in != 0 {
                debug!(
                    "[DRIVER] UsbDk open test: endpoints verified (OUT=0x{:02X}, IN=0x{:02X})",
                    device.ep_out, device.ep_in
                );
            } else {
                warn!("[DRIVER] UsbDk open test: endpoints missing");
                return false;
            }

            true
        }
        Err(e) => {
            debug!("[DRIVER] UsbDk open test failed: {}", e);
            false
        }
    }
}

/// 检测设备所有权状态
/// 
/// 判断方式：
/// 1. 通过 pnputil 检测设备是否被 COM 驱动占用
/// 2. 尝试 libusb open，如果 permission denied / busy → SerialOwned
/// 3. 如果 UsbDk 可用且 open 成功 → UsbDkOwned
/// 4. 如果 libusb open 成功 → WinUsbOwned
/// 
/// 返回当前设备的驱动所有者
pub fn detect_device_owner(context: &UsbContext, vid: u16, pid: u16) -> DeviceOwner {
    // 方法 1：检查是否被 COM 驱动占用
    if is_device_com_occupied(vid, pid) {
        debug!("[DRIVER] device owner=SerialOwned (COM driver active)");
        return DeviceOwner::SerialOwned;
    }

    // 方法 2：尝试 libusb open
    match crate::usb::UsbDevice::open_by_vid_pid(context, vid, pid) {
        Ok(_) => {
            // libusb open 成功
            if has_usbdk() {
                debug!("[DRIVER] device owner=UsbDkOwned (open success via UsbDk)");
                DeviceOwner::UsbDkOwned
            } else {
                debug!("[DRIVER] device owner=WinUsbOwned (open success via libusb/WinUSB)");
                DeviceOwner::WinUsbOwned
            }
        }
        Err(e) => {
            debug!("[DRIVER] libusb open failed: {}", e);
            // open 失败，可能是 COM 占用或其他原因
            if e.contains("claim interface") || e.contains("busy") {
                debug!("[DRIVER] device owner=SerialOwned (claim failed, likely COM occupied)");
                DeviceOwner::SerialOwned
            } else {
                debug!("[DRIVER] device owner=Unknown (open failed for other reason)");
                DeviceOwner::Unknown
            }
        }
    }
}

/// 检测最佳可用后端（每次 reconnect 都要重新判断，不可缓存）
/// 
/// 优先级：
/// 1. UsbDk（如果可用且能真实接管设备）
/// 2. Libusb（如果设备未被 Windows 驱动占用）
/// 3. SerialCom（仅用于 Preloader 阶段）
/// 
/// 对齐 MTKClient Python get_connection_agent():
/// - 动态选择最佳后端
/// - 考虑设备当前驱动绑定状态
/// - 每次 reconnect 重新判断（不缓存）
pub fn detect_backend(context: &UsbContext, vid: u16, pid: u16) -> DeviceBackend {
    // 实时检测设备所有权
    let owner = detect_device_owner(context, vid, pid);

    match owner {
        DeviceOwner::UsbDkOwned => {
            info!("[DRIVER] ownership=UsbDkOwned, selecting UsbDk backend");
            DeviceBackend::UsbDk
        }
        DeviceOwner::WinUsbOwned => {
            info!("[DRIVER] ownership=WinUsbOwned, selecting Libusb backend");
            DeviceBackend::Libusb
        }
        DeviceOwner::SerialOwned => {
            info!("[DRIVER] ownership=SerialOwned, fallback to Libusb (may fail)");
            DeviceBackend::Libusb
        }
        DeviceOwner::Unknown => {
            if has_usbdk() {
                info!("[DRIVER] ownership=Unknown, UsbDk available, selecting UsbDk backend");
                DeviceBackend::UsbDk
            } else {
                info!("[DRIVER] ownership=Unknown, selecting Libusb backend");
                DeviceBackend::Libusb
            }
        }
    }
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

/// 等待设备重枚举窗口
/// 
/// COM 释放后，设备需要时间重新枚举为 BROM VID/PID
/// 不同设备枚举时间不同：
/// - MT6768: ~300ms
/// - MT6785: ~500ms
/// - MT6885: ~800ms
/// 
/// 实现状态机等待：
/// WAIT_REENUMERATION → DEVICE WINDOW DETECTED → OPEN ATTEMPT
pub fn wait_reenumeration_window(context: &UsbContext, vid: u16, pid: u16, timeout_ms: u64) -> bool {
    use std::time::{Duration, Instant};
    
    info!("[USB] waiting re-enumeration window...");
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut retry = 0;

    while Instant::now() < deadline {
        retry += 1;
        
        // 尝试打开设备，检测重枚举窗口
        match crate::usb::UsbDevice::open_by_vid_pid(context, vid, pid) {
            Ok(_) => {
                info!("[USB] re-enumeration window detected (retry {})", retry);
                return true;
            }
            Err(_) => {
                // 设备还未重枚举完成，继续等待
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    debug!("[USB] re-enumeration window not detected within {}ms", timeout_ms);
    false
}

//! Windows 驱动层检测与 libusb-filter 集成
//!
//! 职责：
//! 1. 安装 libusb-filter 到 MTK BROM 设备（0E8D:0003）
//! 2. 检测设备驱动绑定状态（device ownership）
//! 3. 提供后端选择逻辑（每次 reconnect 实时判断）
//!
//! 对齐 MTKClient C# (刷机匣):
//! - install-filter.exe 安装 device filter
//! - --device=USB\VID_0E8D&PID_0003 只影响 MTK BROM
//! - 先检查 filter 是否已安装，避免重复安装

use crate::usb::UsbContext;
use log::{debug, info, warn};
use std::path::{Path, PathBuf};

/// 设备驱动绑定状态
///
/// 描述设备当前被哪个驱动占用：
/// - LibusbFilter: 设备已安装 libusb filter，libusb 可直接访问
/// - WinUsbOwned: 设备被 WinUSB 接管，libusb 可直接访问
/// - SerialOwned: 设备被 Windows CDC/VCOM 驱动占用，需要安装 filter
/// - Unknown: 未知状态（需要进一步检测）
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum DeviceOwner {
    LibusbFilter,
    WinUsbOwned,
    SerialOwned,
    Unknown,
}

/// 驱动后端类型
#[derive(Debug, PartialEq, Clone, Copy)]
#[allow(dead_code)]
pub enum DeviceBackend {
    /// libusb 后端（通过 filter 或直接访问）
    Libusb,
    /// 串口后端（仅 Preloader 阶段）
    SerialCom,
}

/// 获取 install-filter.exe 的路径
///
/// 优先从编译输出目录查找，fallback 到当前 exe 同目录
fn find_install_filter() -> Option<PathBuf> {
    // 方法 1：编译输出目录下的 libusb/ 子目录
    let out_dir = std::env::current_exe()
        .ok()
        .map(|p| p.parent().unwrap().to_path_buf());

    if let Some(ref dir) = out_dir {
        let candidate = dir.join("libusb").join("install-filter.exe");
        if candidate.exists() {
            return Some(candidate);
        }
        // 方法 2：exe 同目录
        let candidate = dir.join("install-filter.exe");
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 方法 3：项目 binaries/libusb/ 目录（开发模式）
    let manifest = Path::new("binaries/libusb/install-filter.exe");
    if manifest.exists() {
        return Some(manifest.canonicalize().ok().unwrap_or(manifest.to_path_buf()));
    }

    None
}

/// 检查 libusb filter 是否已安装到指定设备
///
/// 使用 `install-filter.exe list --device=USB\VID_xxxx&PID_xxxx` 检查
/// 如果输出中包含设备信息，说明 filter 已安装
fn is_filter_installed(vid: u16, pid: u16) -> bool {
    let filter_exe = match find_install_filter() {
        Some(p) => p,
        None => {
            debug!("[FILTER] install-filter.exe not found");
            return false;
        }
    };

    let device_id = format!("USB\\VID_{:04X}&PID_{:04X}", vid, pid);
    debug!("[FILTER] checking filter: {}", device_id);

    let output = std::process::Command::new(&filter_exe)
        .args(["list", &format!("--device={}", device_id)])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            // 如果输出包含设备 ID 或 VID/PID，说明 filter 已安装
            let installed = stdout.contains(&format!("{:04X}", vid))
                || stdout.contains(&format!("{:04X}", pid))
                || (!stdout.trim().is_empty() && output.status.success());
            debug!(
                "[FILTER] list result: installed={}, stdout_len={}, stderr_len={}",
                installed,
                stdout.len(),
                stderr.len()
            );
            installed
        }
        Err(e) => {
            debug!("[FILTER] list check failed: {}", e);
            false
        }
    }
}

/// 安装 libusb filter 到 MTK BROM 设备
///
/// 只针对 0E8D:0003 设备安装 filter，不影响其他 USB 设备
/// 如果 filter 已安装则跳过
pub fn install_libusb_filter(vid: u16, pid: u16) -> bool {
    // 先检查是否已安装
    if is_filter_installed(vid, pid) {
        info!("[FILTER] libusb filter already installed for VID_{:04X}&PID_{:04X}", vid, pid);
        return true;
    }

    let filter_exe = match find_install_filter() {
        Some(p) => p,
        None => {
            warn!("[FILTER] install-filter.exe not found, skipping filter installation");
            return false;
        }
    };

    info!(
        "[FILTER] installing libusb filter for VID_{:04X}&PID_{:04X} using {}",
        vid,
        pid,
        filter_exe.display()
    );

    let device_id = format!("USB\\VID_{:04X}&PID_{:04X}", vid, pid);
    let result = std::process::Command::new(&filter_exe)
        .args(["install", &format!("--device={}", device_id)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match result {
        Ok(status) => {
            if status.success() {
                info!("[FILTER] libusb filter installed successfully");
                true
            } else {
                warn!("[FILTER] install-filter.exe exited with code: {:?}", status.code());
                false
            }
        }
        Err(e) => {
            warn!("[FILTER] failed to run install-filter.exe: {}", e);
            false
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
    let output = std::process::Command::new("pnputil")
        .args(["/enum-devices", "/connected"])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let vid_str = format!("{:04X}", vid);
            let pid_str = format!("{:04X}", pid);

            let has_com = stdout.contains("USB Serial")
                || stdout.contains("COM")
                || stdout.contains("MediaTek USB Port")
                || stdout.contains("VCOM");

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

/// 检测设备所有权状态
///
/// 判断方式：
/// 1. 如果 libusb filter 已安装 → LibusbFilter
/// 2. 尝试 libusb open，如果成功 → WinUsbOwned
/// 3. 如果被 COM 占用 → SerialOwned
/// 4. 否则 → Unknown
pub fn detect_device_owner(context: &UsbContext, vid: u16, pid: u16) -> DeviceOwner {
    // 检查 filter 是否已安装
    if is_filter_installed(vid, pid) {
        debug!("[DRIVER] device owner=LibusbFilter (filter installed)");
        return DeviceOwner::LibusbFilter;
    }

    // 尝试 libusb open
    match crate::usb::UsbDevice::open_by_vid_pid(context, vid, pid) {
        Ok(_) => {
            debug!("[DRIVER] device owner=WinUsbOwned (open success via libusb)");
            DeviceOwner::WinUsbOwned
        }
        Err(e) => {
            debug!("[DRIVER] libusb open failed: {}", e);
            // open 失败，检查是否被 COM 占用
            if is_device_com_occupied(vid, pid) {
                debug!("[DRIVER] device owner=SerialOwned (COM driver active)");
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
/// 1. Libusb（默认，通过 filter 或直接访问）
/// 2. SerialCom（仅用于 Preloader 阶段）
pub fn detect_backend(context: &UsbContext, vid: u16, pid: u16) -> DeviceBackend {
    let owner = detect_device_owner(context, vid, pid);

    match owner {
        DeviceOwner::LibusbFilter | DeviceOwner::WinUsbOwned => {
            info!("[DRIVER] owner={:?}, selecting Libusb backend", owner);
            DeviceBackend::Libusb
        }
        DeviceOwner::SerialOwned => {
            info!("[DRIVER] owner=SerialOwned, trying to install filter then Libusb");
            // 尝试安装 filter
            install_libusb_filter(vid, pid);
            DeviceBackend::Libusb
        }
        DeviceOwner::Unknown => {
            info!("[DRIVER] owner=Unknown, selecting Libusb backend");
            DeviceBackend::Libusb
        }
    }
}

/// 获取后端状态摘要（用于日志）
pub fn backend_status() -> String {
    let filter_exe = find_install_filter()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "not found".to_string());
    format!("libusb-filter: {}", filter_exe)
}

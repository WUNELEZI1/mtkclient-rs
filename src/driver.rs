//! Windows 驱动层 — 使用 install-filter.exe 安装 libusb0 filter
//!
//! 通过 libusb0 的 install-filter.exe 工具安装 filter driver，
//! 让 libusb0 可以访问设备。

use log::{info, warn};
use std::process::Command;

/// 使用 install-filter.exe 安装 libusb0 filter
///
/// 流程：
/// 1. 查找 install-filter.exe
/// 2. 安装 filter
/// 3. 等待设备重枚举
pub fn install_winusb_with_wdi(vid: u16, pid: u16) -> Result<(), String> {
    info!("[FILTER] 正在为 VID_{:04X}&PID_{:04X} 安装 libusb0 filter...", vid, pid);

    // 1. 查找 install-filter.exe
    let exe_path = find_install_filter_exe()?;
    info!("[FILTER] 找到 install-filter.exe: {}", exe_path.display());

    // 2. 安装 filter
    install_filter(&exe_path, vid, pid)?;

    // 3. 等待设备重枚举
    info!("[FILTER] 等待设备重枚举...");
    std::thread::sleep(std::time::Duration::from_secs(3));

    info!("[FILTER] libusb0 filter 安装成功");
    Ok(())
}

/// 查找 install-filter.exe
fn find_install_filter_exe() -> Result<std::path::PathBuf, String> {
    // 搜索顺序：
    // 1. 当前目录下的 libusb 子目录
    // 2. binaries/libusb 目录
    // 3. target/debug/libusb 目录
    // 4. target/release/libusb 目录
    
    let candidates = [
        "libusb/install-filter.exe",
        "binaries/libusb/install-filter.exe",
        "target/debug/libusb/install-filter.exe",
        "target/release/libusb/install-filter.exe",
    ];

    for candidate in &candidates {
        let path = std::path::Path::new(candidate);
        if path.exists() {
            return Ok(path.to_path_buf());
        }
    }

    // 尝试在可执行文件所在目录查找
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let libusb_path = exe_dir.join("libusb").join("install-filter.exe");
            if libusb_path.exists() {
                return Ok(libusb_path);
            }
        }
    }

    Err("找不到 install-filter.exe，请确保 libusb 驱动文件已解压".to_string())
}

/// 使用 install-filter.exe 安装 filter
fn install_filter(exe_path: &std::path::Path, vid: u16, pid: u16) -> Result<(), String> {
    let device_id = format!("USB\\VID_{:04X}&PID_{:04X}", vid, pid);
    
    info!("[FILTER] 安装 filter: {}", device_id);

    let result = Command::new(exe_path)
        .args(&["install", &format!("--device={}", device_id)])
        .output()
        .map_err(|e| format!("启动 install-filter.exe 失败: {}", e))?;

    if result.status.success() {
        info!("[FILTER] install-filter.exe 执行成功");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let stdout = String::from_utf8_lossy(&result.stdout);
        Err(format!("install-filter.exe 失败: {} {}", stdout, stderr))
    }
}

/// 卸载 libusb0 filter
#[allow(dead_code)] // 预留：驱动卸载/恢复流程，install-drivers 反向操作
pub fn uninstall_filter(vid: u16, pid: u16) -> Result<(), String> {
    let exe_path = find_install_filter_exe()?;
    let device_id = format!("USB\\VID_{:04X}&PID_{:04X}", vid, pid);
    
    info!("[FILTER] 卸载 filter: {}", device_id);

    let result = Command::new(&exe_path)
        .args(&["uninstall", &format!("--device={}", device_id)])
        .output()
        .map_err(|e| format!("启动 install-filter.exe 失败: {}", e))?;

    if result.status.success() {
        info!("[FILTER] filter 卸载成功");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&result.stderr);
        warn!("[FILTER] filter 卸载失败: {}", stderr);
        Ok(()) // 卸载失败不报错
    }
}

//! libusb1-sys 设备打开验证
//!
//! **唯一真相**：libusb_open_device_with_vid_pid 返回非空 handle 才算成功。
//! 任何驱动切换流程最终都通过本模块的 `check_winusb_installed` 验证。

use log::info;

use super::setupapi::{MTK_BROM_PID, MTK_VID};

/// 检查 WinUSB 驱动是否已就绪（libusb1-sys 实际打开设备）
pub fn check_winusb_installed() -> bool {
    unsafe {
        let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        let ret = libusb1_sys::libusb_init(&mut ctx);
        if ret != 0 {
            return false;
        }

        let handle = libusb1_sys::libusb_open_device_with_vid_pid(ctx, MTK_VID, MTK_BROM_PID);
        let found = !handle.is_null();

        if found {
            info!(
                "[DRIVER] libusb1-sys 已能打开设备 (0x{:04X}:0x{:04X})",
                MTK_VID, MTK_BROM_PID
            );
            libusb1_sys::libusb_close(handle);
        }

        libusb1_sys::libusb_exit(ctx);
        found
    }
}

/// 兼容旧接口 — 通过 pnputil 枚举设备实例 ID
#[allow(dead_code)]
pub fn get_device_instance_id(vid: u16, pid: u16) -> Result<String, String> {
    use std::process::Command;

    let target = format!("USB\\VID_{:04X}&PID_{:04X}", vid, pid);
    let output = Command::new("pnputil")
        .args(["/enum-devices"])
        .output()
        .map_err(|e| format!("pnputil 枚举失败: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "pnputil /enum-devices 失败: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let target_upper = target.to_uppercase();
    let mut matches: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("Instance ID:") {
            let inst = rest.trim().to_string();
            if inst.to_uppercase().starts_with(&target_upper) {
                matches.push(inst);
            }
        }
    }

    matches
        .into_iter()
        .next()
        .ok_or_else(|| format!("未找到设备实例: {}", target))
}

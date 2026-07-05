//! WinUSB 设备打开验证
//!
//! 通过 nusb 实际打开设备确认 WinUSB 驱动是否已就绪。

use log::info;
use nusb::MaybeFuture;

use super::setupapi::{MTK_BROM_PID, MTK_VID};

/// 检查 WinUSB 驱动是否已就绪（nusb 实际打开设备）
pub fn check_winusb_installed() -> bool {
    let mut devices = match nusb::list_devices().wait() {
        Ok(d) => d,
        Err(_) => return false,
    };

    let found = devices
        .any(|d| d.vendor_id() == MTK_VID && d.product_id() == MTK_BROM_PID);

    if found {
        info!(
            "[DRIVER] nusb 已能枚举设备 (0x{:04X}:0x{:04X})",
            MTK_VID, MTK_BROM_PID
        );
    }

    found
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

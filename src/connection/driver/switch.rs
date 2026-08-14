//! WinUSB 驱动切换核心流程
//!
//! 流程（Zadig 风格，不卸载设备、不需要重新插拔）：
//! 1. libusb 已经能打开设备  → 直接返回 OK
//! 2. wdi-rs create_list      → 找到 BROM 设备
//! 3. wdi-rs prepare_driver   → 生成/签名 WinUSB INF
//! 4. UpdateDriverForPlugAndPlayDevicesW(INSTALLFLAG_FORCE)
//!    → 强制覆盖 wdm_usb
//! 5. 轮询 libusb 验证        → 最多 N 秒

use log::{info, warn};
use std::path::PathBuf;
use std::time::Duration;
use wdi_rs::{CreateListOptions, PrepareDriverOptions, create_list, prepare_driver};

use super::detect::{BromDriverType, check_brom_driver_type};
use super::setupapi::{
    INF_NAME, INSTALLFLAG_FORCE, INSTALLFLAG_NONINTERACTIVE, MTK_BROM_PID, MTK_VID,
    UpdateDriverForPlugAndPlayDevicesW,
};

const MTKCLIENT_TEMP_DIR: &str = "mtkclient_winusb";
const POLL_INTERVAL_MS: u64 = 500;
const RETRY_COUNT: u32 = 3;
const RETRY_DELAY_MS: u64 = 500;
const STABILIZE_DELAY_MS: u64 = 200;
const POLL_TIMEOUT_SEC: u32 = 10;

/// 切换 BROM 设备到 WinUSB 驱动（Zadig 风格）
pub fn switch_to_winusb() -> Result<(), String> {
    info!(
        "[DRIVER] 切换 BROM 设备 (0x{:04X}:0x{:04X}) 到 WinUSB 驱动（Zadig 风格）...",
        MTK_VID, MTK_BROM_PID
    );

    if !super::admin::is_admin() {
        return Err("需要管理员权限才能切换驱动".to_string());
    }

    // 步骤 1：快速路径 — 只有 SetupAPI 确认当前服务是 WinUSB/libwdi 才跳过
    if current_driver_is_winusb() {
        info!("[DRIVER] SetupAPI 确认设备已是 WinUSB，无需切换");
        return Ok(());
    }

    // 步骤 2：wdi-rs create_list 找设备
    let device = find_brom_device()?;

    // 步骤 3：wdi-rs prepare_driver 生成 WinUSB INF
    let inf_dir = get_inf_dir();
    std::fs::create_dir_all(&inf_dir).map_err(|e| format!("创建 INF 目录失败: {}", e))?;

    info!(
        "[DRIVER] wdi-rs prepare_driver - 生成/签名 WinUSB INF: {}\\{}",
        inf_dir.display(),
        INF_NAME
    );

    let options = PrepareDriverOptions {
        driver_type: wdi_rs::DriverType::WinUsb,
        vendor_name: Some("MediaTek".to_string()),
        device_guid: None,
        disable_cat: false,
        disable_signing: false,
        cert_subject: None,
        external_inf: false,
        use_wcid_driver: false,
    };

    let inf_dir_str = inf_dir.to_str().ok_or("INF 目录路径包含非 UTF-8 字符")?;
    prepare_driver(&device, inf_dir_str, INF_NAME, &options)
        .map_err(|e| format!("wdi-rs prepare_driver 失败: {}", e))?;
    info!("[DRIVER] WinUSB INF 已生成、签名、并注册到驱动商店");

    // 步骤 4：调用 Windows API 强制安装
    info!("[DRIVER] 调用 UpdateDriverForPlugAndPlayDevicesW (INSTALLFLAG_FORCE)...");
    force_install_via_api(&device, &inf_dir)?;

    // 步骤 5：libusb 验证
    if poll_libusb_ready(POLL_TIMEOUT_SEC) {
        info!("[DRIVER] 设备已切换到 WinUSB（验证通过）");
        return Ok(());
    }

    Err("WinUSB 强制安装后 libusb1-sys 仍找不到设备（10秒超时）".to_string())
}

/// 用 wdi-rs create_list 找到 BROM 设备（带重试，等设备稳定）
fn find_brom_device() -> Result<wdi_rs::Device, String> {
    // 关掉 COM 口后设备可能正在重枚举，给它一点时间
    info!("[DRIVER] 等待设备稳定 ({}ms)...", STABILIZE_DELAY_MS);
    std::thread::sleep(Duration::from_millis(STABILIZE_DELAY_MS));

    let mut last_err = String::new();
    for attempt in 1..=RETRY_COUNT {
        info!(
            "[DRIVER] wdi-rs create_list 第 {}/{} 次尝试",
            attempt, RETRY_COUNT
        );

        let options = CreateListOptions {
            list_all: true,
            list_hubs: false,
            trim_whitespaces: true,
        };

        match create_list(options) {
            Ok(devices) => {
                for device in devices.iter() {
                    if device.vid == MTK_VID && device.pid == MTK_BROM_PID {
                        info!(
                            "[DRIVER] 找到 BROM 设备: vid=0x{:04X} pid=0x{:04X} mi={}",
                            device.vid, device.pid, device.mi
                        );
                        return Ok(device);
                    }
                }
                last_err = format!(
                    "BROM 设备不在列表中 (VID=0x{:04X}, PID=0x{:04X})",
                    MTK_VID, MTK_BROM_PID
                );
                warn!("[DRIVER] {}", last_err);
            }
            Err(e) => {
                last_err = format!("wdi-rs create_list 失败: {}", e);
                warn!("[DRIVER] {}", last_err);
            }
        }

        if attempt < RETRY_COUNT {
            std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS));
        }
    }

    Err(last_err)
}

fn should_skip_switch_for_driver(driver_type: &BromDriverType) -> bool {
    matches!(driver_type, BromDriverType::WinUsb)
}

fn current_driver_is_winusb() -> bool {
    check_brom_driver_type()
        .map(|driver_type| should_skip_switch_for_driver(&driver_type))
        .unwrap_or(false)
}

/// INF 输出目录
fn get_inf_dir() -> PathBuf {
    std::env::temp_dir().join(MTKCLIENT_TEMP_DIR)
}

/// 通过 Windows API 强制安装驱动（绕开 libwdi 预检）
///
/// 关键：HardwareId 参数**不能为 NULL**，否则会返回 ERROR_INVALID_PARAMETER (87)。
/// libwdi 内部就是把 `device.hardware_id`（如 `USB\VID_0E8D&PID_0003`）传过去。
#[cfg(target_os = "windows")]
fn force_install_via_api(device: &wdi_rs::Device, inf_dir: &std::path::Path) -> Result<(), String> {
    let inf_path = inf_dir.join(INF_NAME);
    if !inf_path.exists() {
        return Err(format!("INF 文件不存在: {}", inf_path.display()));
    }

    let inf_path_w: Vec<u16> = inf_path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let hardware_id = device
        .hardware_id
        .as_deref()
        .or(device.device_id.as_deref())
        .ok_or_else(|| {
            "设备没有 hardware_id 或 device_id（无法调用 UpdateDriverForPlugAndPlayDevicesW)"
                .to_string()
        })?;

    info!(
        "[DRIVER] 使用硬件 ID 调用 UpdateDriverForPlugAndPlayDevicesW: {}",
        hardware_id
    );

    let hardware_id_w: Vec<u16> = hardware_id
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut reboot_required: i32 = 0;

    let result = unsafe {
        UpdateDriverForPlugAndPlayDevicesW(
            std::ptr::null_mut(),   // hwndParent = NULL
            hardware_id_w.as_ptr(), // HardwareId = USB\VID_0E8D&PID_0003
            inf_path_w.as_ptr(),    // FullInfPath
            INSTALLFLAG_FORCE | INSTALLFLAG_NONINTERACTIVE,
            &mut reboot_required,
        )
    };

    if result == 0 {
        let err = std::io::Error::last_os_error();
        Err(format!(
            "UpdateDriverForPlugAndPlayDevicesW 失败: {} (err={})",
            err,
            err.raw_os_error().unwrap_or(0)
        ))
    } else {
        info!(
            "[DRIVER] Windows API 强制安装成功 (reboot_required={})",
            reboot_required
        );
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn force_install_via_api(
    _device: &wdi_rs::Device,
    _inf_dir: &std::path::Path,
) -> Result<(), String> {
    Err("仅 Windows 支持".to_string())
}

/// 轮询 libusb 设备是否就绪
fn poll_libusb_ready(max_seconds: u32) -> bool {
    for i in 1..=(max_seconds * 2) {
        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        if current_driver_is_winusb() {
            info!(
                "[DRIVER] SetupAPI 验证 WinUSB 通过 ({}×{}ms)",
                i, POLL_INTERVAL_MS
            );
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::driver::detect::BromDriverType;

    #[test]
    fn switch_is_skipped_only_for_confirmed_winusb_driver() {
        assert!(should_skip_switch_for_driver(&BromDriverType::WinUsb));
    }

    #[test]
    fn serial_driver_must_not_skip_switch_even_if_usb_can_be_enumerated() {
        assert!(!should_skip_switch_for_driver(&BromDriverType::Serial));
    }

    #[test]
    fn unknown_driver_must_not_skip_switch() {
        assert!(!should_skip_switch_for_driver(&BromDriverType::Unknown(
            "未知驱动".to_string()
        )));
    }
}

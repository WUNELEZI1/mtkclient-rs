//! WinUSB driver switch core flow
//!
//! Flow (Zadig-style; no device uninstall, no replug required):
//! 1. libusb can already open the device  → return OK immediately
//! 2. wdi-rs create_list      → find the BROM device
//! 3. wdi-rs prepare_driver   → generate/sign the WinUSB INF
//! 4. UpdateDriverForPlugAndPlayDevicesW(INSTALLFLAG_FORCE)
//!    → force overwrite wdm_usb
//! 5. poll libusb to verify    → up to N seconds
//!
//! This file is compiled only on Windows with the `winusb-driver` feature: it depends on wdi-rs
//! (Windows only, requires the libwdi native library). Otherwise `switch_stub.rs` provides stub
//! implementations with the same signatures, so the crate compiles and tests run without Windows / libwdi toolchain.

#![cfg(all(target_os = "windows", feature = "winusb-driver"))]

use log::{info, warn};
use std::path::PathBuf;
use std::time::Duration;
use wdi_rs::{CreateListOptions, PrepareDriverOptions, create_list, prepare_driver};

use super::detect::{check_brom_driver_type, should_skip_switch_for_driver};
use super::setupapi::{
    INF_NAME, INSTALLFLAG_FORCE, INSTALLFLAG_NONINTERACTIVE, MTK_BROM_PID, MTK_VID,
    UpdateDriverForPlugAndPlayDevicesW,
};

const MTKCLIENT_TEMP_DIR: &str = "mtkclient_winusb";
const POLL_INTERVAL_MS: u64 = 500;
/// On first install the device (after closing the COM port) re-enumerates slowly; allow extra lookup attempts
const RETRY_COUNT: u32 = 5;
const RETRY_DELAY_MS: u64 = 700;
/// Wait for the device to stabilize before the first install (it may be re-enumerating after the COM port closes;
/// too short a wait makes create_list miss the device and falsely report failure)
const STABILIZE_DELAY_MS: u64 = 700;
/// Verification polling duration: first WinUSB install (catalog signing + driver binding) may be slow
const POLL_TIMEOUT_SEC: u32 = 15;
/// Retry count for the whole install flow (prepare → force install → re-enumerate → verify)
const INSTALL_RETRY_COUNT: u32 = 4;
/// Exponential backoff base: 800ms → 1600ms → 3200ms → 6400ms
const INSTALL_BACKOFF_BASE_MS: u64 = 800;
/// Wait time after an active re-enumeration for the device to reappear (first install binding is slow; allow enough time)
const REENUM_WAIT_MS: u64 = 2500;

/// Switch the BROM device to the WinUSB driver (Zadig-style)
pub fn switch_to_winusb() -> Result<(), String> {
    info!(
        "[DRIVER] 切换 BROM 设备 (0x{:04X}:0x{:04X}) 到 WinUSB 驱动（Zadig 风格）...",
        MTK_VID, MTK_BROM_PID
    );

    if !super::admin::is_admin() {
        return Err("需要管理员权限才能切换驱动".to_string());
    }

    // Step 1: fast path — skip only when SetupAPI confirms the current service is WinUSB/libwdi
    if current_driver_is_winusb() {
        info!("[DRIVER] SetupAPI 确认设备已是 WinUSB，无需切换");
        return Ok(());
    }

    let mut last_err = String::new();
    for attempt in 1..=INSTALL_RETRY_COUNT {
        info!(
            "[DRIVER] WinUSB 安装尝试 {}/{}",
            attempt, INSTALL_RETRY_COUNT
        );

        // Step 2: find the device with wdi-rs create_list
        let device = match find_brom_device() {
            Ok(d) => d,
            Err(e) => {
                last_err = e;
                warn!("[DRIVER] 第 {} 次未找到设备: {}", attempt, last_err);
                if attempt < INSTALL_RETRY_COUNT {
                    let delay = INSTALL_BACKOFF_BASE_MS * (1u64 << (attempt - 1));
                    std::thread::sleep(Duration::from_millis(delay));
                }
                continue;
            }
        };

        // Step 3: generate the WinUSB INF with wdi-rs prepare_driver
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
        if let Err(e) = prepare_driver(&device, inf_dir_str, INF_NAME, &options) {
            last_err = format!("wdi-rs prepare_driver 失败: {}", e);
            warn!(
                "[DRIVER] 第 {} 次 prepare_driver 失败: {}",
                attempt, last_err
            );
            if attempt < INSTALL_RETRY_COUNT {
                let delay = INSTALL_BACKOFF_BASE_MS * (1u64 << (attempt - 1));
                std::thread::sleep(Duration::from_millis(delay));
            }
            continue;
        }
        info!("[DRIVER] WinUSB INF 已生成、签名、并注册到驱动商店");

        // Step 4: force-install via the Windows API
        info!("[DRIVER] 调用 UpdateDriverForPlugAndPlayDevicesW (INSTALLFLAG_FORCE)...");
        if let Err(e) = force_install_via_api(&device, &inf_dir) {
            last_err = format!("强制安装失败: {}", e);
            warn!("[DRIVER] 第 {} 次强制安装失败: {}", attempt, last_err);
            if attempt < INSTALL_RETRY_COUNT {
                let delay = INSTALL_BACKOFF_BASE_MS * (1u64 << (attempt - 1));
                std::thread::sleep(Duration::from_millis(delay));
            }
            continue;
        }

        // Step 5: actively re-enumerate the device so the new driver takes effect immediately (no manual replug).
        // On some machines the device stays bound to the old driver after the first install; a re-enumeration is required to switch to WinUSB reliably.
        match unsafe { super::setupapi::find_and_restart_brom_device() } {
            Ok(()) => {
                info!("[DRIVER] 已触发设备重枚举，等待 WinUSB 驱动生效...");
                std::thread::sleep(Duration::from_millis(REENUM_WAIT_MS));
            }
            Err(e) => warn!("[DRIVER] 主动重枚举失败（依赖轮询兜底）: {}", e),
        }

        // Step 6: SetupAPI verification (the driver becomes WinUSB after device re-enumeration)
        if poll_libusb_ready(POLL_TIMEOUT_SEC) {
            info!("[DRIVER] 设备已切换到 WinUSB（验证通过）");
            return Ok(());
        }
        last_err = "WinUSB 强制安装后设备驱动未变为 WinUSB（轮询超时）".to_string();
        warn!("[DRIVER] 第 {} 次验证失败: {}", attempt, last_err);
        // Polling timed out but the device may still be online, merely stuck on the old driver: trigger one more
        // re-enumeration to nudge it, avoiding a full prepare cycle (saves time, improves first-install success).
        if attempt < INSTALL_RETRY_COUNT {
            if unsafe { super::setupapi::find_and_restart_brom_device() }.is_ok() {
                std::thread::sleep(Duration::from_millis(REENUM_WAIT_MS));
            }
            let delay = INSTALL_BACKOFF_BASE_MS * (1u64 << (attempt - 1));
            std::thread::sleep(Duration::from_millis(delay));
        }
    }

    Err(format!(
        "WinUSB 驱动安装失败（已重试 {} 次）: {}\n\
         手动解决：以管理员身份打开设备管理器 → 找到 \"MTK USB Port\" / \"BROM\" 设备 → \
         右键“更新驱动程序” → “浏览我的计算机以查找驱动” → 指向目录 '{}' → \
         选择 {} 强制安装。",
        INSTALL_RETRY_COUNT,
        last_err,
        get_inf_dir().display(),
        INF_NAME
    ))
}

/// Find the BROM device with wdi-rs create_list (with retries, waiting for the device to stabilize)
fn find_brom_device() -> Result<wdi_rs::Device, String> {
    // The device may be re-enumerating after the COM port closes; give it some time
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

fn current_driver_is_winusb() -> bool {
    check_brom_driver_type()
        .map(|driver_type| should_skip_switch_for_driver(&driver_type))
        .unwrap_or(false)
}

/// INF output directory
fn get_inf_dir() -> PathBuf {
    std::env::temp_dir().join(MTKCLIENT_TEMP_DIR)
}

/// Force-install the driver via the Windows API (bypassing the libwdi precheck)
///
/// Key: the HardwareId parameter **must not be NULL**, otherwise it returns ERROR_INVALID_PARAMETER (87).
/// libwdi internally passes `device.hardware_id` (e.g. `USB\VID_0E8D&PID_0003`).
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

/// Poll whether the libusb device is ready
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

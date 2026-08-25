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
/// 首次安装时设备（关掉 COM 口后）重枚举较慢，多给几次查找机会
const RETRY_COUNT: u32 = 5;
const RETRY_DELAY_MS: u64 = 700;
/// 首次安装前等待设备稳定的时间（关掉 COM 口后设备可能正在重枚举，
/// 太短会导致 create_list 找不到设备而误判失败）
const STABILIZE_DELAY_MS: u64 = 700;
/// 验证轮询时长：首次安装 WinUSB 目录签名 + 驱动绑定可能较慢
const POLL_TIMEOUT_SEC: u32 = 15;
/// 整段安装流程重试次数（prepare → 强制安装 → 重枚举 → 验证）
const INSTALL_RETRY_COUNT: u32 = 4;
/// 指数退避基数：800ms → 1600ms → 3200ms → 6400ms
const INSTALL_BACKOFF_BASE_MS: u64 = 800;
/// 主动重枚举后等待设备重新出现的时长（首次安装驱动绑定较慢，给足时间）
const REENUM_WAIT_MS: u64 = 2500;

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

    let mut last_err = String::new();
    for attempt in 1..=INSTALL_RETRY_COUNT {
        info!(
            "[DRIVER] WinUSB 安装尝试 {}/{}",
            attempt, INSTALL_RETRY_COUNT
        );

        // 步骤 2：wdi-rs create_list 找设备
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

        // 步骤 4：调用 Windows API 强制安装
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

        // 步骤 5：主动重枚举设备，使新驱动立即生效（免去手动重新插拔）。
        // 部分机器首次安装后设备仍绑定旧驱动，必须触发一次重枚举才能稳定切换到 WinUSB。
        match unsafe { super::setupapi::find_and_restart_brom_device() } {
            Ok(()) => {
                info!("[DRIVER] 已触发设备重枚举，等待 WinUSB 驱动生效...");
                std::thread::sleep(Duration::from_millis(REENUM_WAIT_MS));
            }
            Err(e) => warn!("[DRIVER] 主动重枚举失败（依赖轮询兜底）: {}", e),
        }

        // 步骤 6：SetupAPI 验证（设备重枚举后驱动变为 WinUSB）
        if poll_libusb_ready(POLL_TIMEOUT_SEC) {
            info!("[DRIVER] 设备已切换到 WinUSB（验证通过）");
            return Ok(());
        }
        last_err = "WinUSB 强制安装后设备驱动未变为 WinUSB（轮询超时）".to_string();
        warn!("[DRIVER] 第 {} 次验证失败: {}", attempt, last_err);
        // 轮询超时但设备可能仍在线、只是卡在旧驱动上：再触发一次重枚举尝试推它一把，
        // 避免直接进入下一轮完整 prepare（节省时间、提高首次安装成功率）。
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

//! Windows 驱动层 — WinUSB 驱动安装 + 管理员权限检测
//!
//! 驱动切换流程（Zadig 风格，不卸载设备、不需要重新插拔）：
//! 1. libusb 能打开设备     → 已就绪，直接返回
//! 2. wdi-rs create_list     → 找到 BROM 设备（device info）
//! 3. wdi-rs prepare_driver  → 生成 + 自签名 + 注册 WinUSB INF 到驱动商店
//! 4. UpdateDriverForPlugAndPlayDevicesW(INSTALLFLAG_FORCE)
//!                          → 强制覆盖 wdm_usb（绕过 libwdi 预检）
//! 5. libusb 实际打开验证   → 唯一真相
//!
//! 关键点：
//! - wdi-rs 高层 install() 会先 check_existing_driver → 设备有 wdm_usb 时
//!   返回 Error::Exists 且**不**生成 INF。
//! - 但 wdi-rs 公开了 prepare_driver（wdi.rs），它**不**做 exists 检查，
//!   直接生成 + 签名 INF 并加入驱动商店。
//! - 然后用 raw FFI 调 UpdateDriverForPlugAndPlayDevicesW + INSTALLFLAG_FORCE
//!   强制安装已准备好的 INF。Zadig 就是这个套路。

use log::{info, warn};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use wdi_rs::{
    create_list, prepare_driver, CreateListOptions, DriverType, PrepareDriverOptions,
};

const MTK_VID: u16 = 0x0E8D;
const MTK_BROM_PID: u16 = 0x0003;
const INF_NAME: &str = "mtk_brom_winusb.inf";

// =============================================================================
// Windows API FFI: UpdateDriverForPlugAndPlayDevicesW
// =============================================================================

#[cfg(target_os = "windows")]
#[repr(C)]
struct HWND__ {
    _unused: [u8; 0],
}
#[cfg(target_os = "windows")]
type HWND = *mut HWND__;

#[cfg(target_os = "windows")]
#[link(name = "setupapi")]
unsafe extern "system" {
    /// `UpdateDriverForPlugAndPlayDevicesW` - 强制安装匹配硬件 ID 的驱动
    ///
    /// 当 `InstallFlags` 包含 `INSTALLFLAG_FORCE` 时，会强制覆盖现有驱动。
    /// Zadig 内部用的就是这个 API。
    fn UpdateDriverForPlugAndPlayDevicesW(
        hwndParent: HWND,
        hardwareId: *const u16,
        fullInfPath: *const u16,
        installFlags: u32,
        bRebootRequired: *mut i32,
    ) -> i32;
}

#[cfg(target_os = "windows")]
const INSTALLFLAG_FORCE: u32 = 0x00000001;
#[cfg(target_os = "windows")]
const INSTALLFLAG_NONINTERACTIVE: u32 = 0x00000004;

// =============================================================================
// 管理员权限
// =============================================================================

/// 检查当前进程是否以管理员身份运行
#[cfg(target_os = "windows")]
pub fn is_admin() -> bool {
    is_elevated::is_elevated()
}

#[cfg(not(target_os = "windows"))]
pub fn is_admin() -> bool {
    true
}

/// 以管理员权限重启当前进程
///
/// 设置环境变量 `MTKCLIENT_ELEVATED=1` 给子进程，避免子进程再次触发提权造成死循环。
#[cfg(target_os = "windows")]
pub fn restart_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("获取 exe 路径失败: {}", e))?;
    let args: Vec<String> = std::env::args().skip(1).collect();

    info!("[DRIVER] 正在以管理员权限重启: {}", exe.display());

    let ps_cmd = format!(
        "$env:MTKCLIENT_ELEVATED='1'; Start-Process '{}' -ArgumentList '{}' -Verb RunAs -Wait",
        exe.display(),
        args.iter()
            .map(|a| format!("'{}'", a.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",")
    );

    let result = std::process::Command::new("powershell")
        .args(&["-NoProfile", "-Command", &ps_cmd])
        .status()
        .map_err(|e| format!("启动管理员进程失败: {}", e))?;

    if result.success() {
        std::process::exit(0);
    } else {
        Err("管理员重启失败（用户可能取消了 UAC）".to_string())
    }
}

#[cfg(not(target_os = "windows"))]
pub fn restart_as_admin() -> Result<(), String> {
    Ok(())
}

// =============================================================================
// 核心功能
// =============================================================================

/// 检查 WinUSB 驱动是否已就绪（libusb1-sys 实际打开设备）
///
/// **唯一真相**：libusb_open_device_with_vid_pid 返回非空 handle 才算成功。
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
            info!("[DRIVER] libusb1-sys 已能打开设备 (0x0E8D:0x0003)");
            libusb1_sys::libusb_close(handle);
        }

        libusb1_sys::libusb_exit(ctx);
        found
    }
}

/// 切换 BROM 设备到 WinUSB 驱动（Zadig 风格）
///
/// 完整流程（不卸载设备、不需要重新插拔）：
/// ```
/// 1. check_winusb_installed       → 已就绪则直接返回
/// 2. wdi-rs create_list            → 找到 BROM 设备
/// 3. wdi-rs prepare_driver         → 生成/签名/注册 WinUSB INF（不检查现有驱动）
/// 4. UpdateDriverForPlugAndPlayDevicesW(INSTALLFLAG_FORCE)
///                                  → 强制覆盖 wdm_usb
/// 5. 轮询 libusb 打开验证          → 最多 10 秒
/// ```
pub fn switch_to_winusb() -> Result<(), String> {
    info!("[DRIVER] 切换 BROM 设备 (0x0E8D:0x0003) 到 WinUSB 驱动（Zadig 风格）...");

    if !is_admin() {
        return Err("需要管理员权限才能切换驱动".to_string());
    }

    // 步骤 1：快速路径 — libusb 已经能打开设备
    if check_winusb_installed() {
        info!("[DRIVER] 设备已就绪，无需切换");
        return Ok(());
    }

    // 步骤 2：wdi-rs create_list 找设备
    // 关键：wdi-rs 的 create_list 不会因设备有 wdm_usb 而失败
    let device = find_brom_device()?;

    // 步骤 3：wdi-rs prepare_driver 生成 WinUSB INF
    // 关键：prepare_driver 不检查设备是否有驱动（只有 install() 才检查）
    let inf_dir = get_inf_dir();
    std::fs::create_dir_all(&inf_dir).map_err(|e| format!("创建 INF 目录失败: {}", e))?;

    info!(
        "[DRIVER] wdi-rs prepare_driver - 生成/签名 WinUSB INF: {}\\{}",
        inf_dir.display(),
        INF_NAME
    );

    let options = PrepareDriverOptions {
        driver_type: DriverType::WinUsb,
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
    // Zadig 内部用的就是这个 API。INSTALLFLAG_FORCE 让 Windows 强制覆盖现有驱动
    // 关键：HardwareId 参数不能为 NULL，否则会返回 ERROR_INVALID_PARAMETER (87)
    info!("[DRIVER] 调用 UpdateDriverForPlugAndPlayDevicesW (INSTALLFLAG_FORCE)...");
    force_install_via_api(&device, &inf_dir)?;

    // 步骤 5：libusb 验证
    if poll_libusb_ready(10) {
        info!("[DRIVER] 设备已切换到 WinUSB（验证通过）");
        return Ok(());
    }

    Err("WinUSB 强制安装后 libusb1-sys 仍找不到设备（10秒超时）".to_string())
}

/// 用 wdi-rs create_list 找到 BROM 设备（带重试，等设备稳定）
fn find_brom_device() -> Result<wdi_rs::Device, String> {
    // 关掉 COM 口后设备可能正在重枚举，给它一点时间
    info!("[DRIVER] 等待设备稳定 (200ms)...");
    std::thread::sleep(Duration::from_millis(200));

    // 最多 3 次重试
    let mut last_err = String::new();
    for attempt in 1..=3 {
        info!("[DRIVER] wdi-rs create_list 第 {}/3 次尝试", attempt);

        // list_all: true 列出所有设备，不只是 libwdi 已知驱动的设备
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
                last_err = format!("BROM 设备不在列表中 (VID=0x{:04X}, PID=0x{:04X})", MTK_VID, MTK_BROM_PID);
                warn!("[DRIVER] {}", last_err);
            }
            Err(e) => {
                last_err = format!("wdi-rs create_list 失败: {}", e);
                warn!("[DRIVER] {}", last_err);
            }
        }

        // 重试前等待
        if attempt < 3 {
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    Err(last_err)
}

/// INF 输出目录
fn get_inf_dir() -> PathBuf {
    std::env::temp_dir().join("mtkclient_winusb")
}

/// 通过 Windows API 强制安装驱动（绕开 libwdi 预检）
///
/// 关键：HardwareId 参数**不能为 NULL**，否则会返回 ERROR_INVALID_PARAMETER (87)。
/// libwdi 内部就是把 `device.hardware_id`（如 `USB\VID_0E8D&PID_0003`）传过去。
#[cfg(target_os = "windows")]
fn force_install_via_api(device: &wdi_rs::Device, inf_dir: &PathBuf) -> Result<(), String> {
    let inf_path = inf_dir.join(INF_NAME);
    if !inf_path.exists() {
        return Err(format!("INF 文件不存在: {}", inf_path.display()));
    }

    let inf_path_w: Vec<u16> = inf_path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // 优先用 hardware_id（libwdi 内部用法），否则用 device_id
    let hardware_id = device
        .hardware_id
        .as_deref()
        .or_else(|| device.device_id.as_deref())
        .ok_or_else(|| "设备没有 hardware_id 或 device_id（无法调用 UpdateDriverForPlugAndPlayDevicesW）".to_string())?;

    info!("[DRIVER] 使用硬件 ID 调用 UpdateDriverForPlugAndPlayDevicesW: {}", hardware_id);

    let hardware_id_w: Vec<u16> = hardware_id
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut reboot_required: i32 = 0;

    let result = unsafe {
        UpdateDriverForPlugAndPlayDevicesW(
            std::ptr::null_mut(),          // hwndParent = NULL
            hardware_id_w.as_ptr(),        // HardwareId = USB\VID_0E8D&PID_0003
            inf_path_w.as_ptr(),           // FullInfPath
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
fn force_install_via_api(_device: &wdi_rs::Device, _inf_dir: &PathBuf) -> Result<(), String> {
    Err("仅 Windows 支持".to_string())
}

/// 轮询 libusb 设备是否就绪
fn poll_libusb_ready(max_seconds: u32) -> bool {
    for i in 1..=(max_seconds * 2) {
        std::thread::sleep(Duration::from_millis(500));
        if check_winusb_installed() {
            info!("[DRIVER] libusb1-sys 验证通过 ({}×500ms)", i);
            return true;
        }
    }
    false
}

/// 确保 WinUSB 驱动已安装（检查 + 切换）
pub fn ensure_winusb_driver(_vid: u16, _pid: u16) -> Result<(), String> {
    if check_winusb_installed() {
        return Ok(());
    }
    switch_to_winusb()
}

/// 兼容旧接口
pub fn install_winusb_with_wdi(_vid: u16, _pid: u16) -> Result<(), String> {
    switch_to_winusb()
}

/// 兼容旧接口 - pnputil 枚举（保留备用）
#[allow(dead_code)]
pub fn get_device_instance_id(vid: u16, pid: u16) -> Result<String, String> {
    let target = format!("USB\\VID_{:04X}&PID_{:04X}", vid, pid);
    let output = Command::new("pnputil")
        .args(&["/enum-devices"])
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

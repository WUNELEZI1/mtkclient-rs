//! USB 上下文 + 设备检测（Android 分支）
//!
//! Android 不允许普通 App 枚举 USB 设备，必须由 Kotlin 侧用 UsbManager
//! 申请权限后，把 UsbDeviceConnection 的 fd 传给 Rust。
//! 因此这里不做主动枚举，改为接收 Kotlin 注册的设备信息。

use crate::system::config::DeviceType;

/// USB 设备阶段
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum UsbStage {
    Brom,
    Preloader,
    Da,
    Unknown,
}

impl UsbStage {
    pub fn from_pid(pid: u16) -> Self {
        match pid {
            0x0003 => UsbStage::Brom,
            0x2000 => UsbStage::Preloader,
            0x2001 => UsbStage::Da,
            _ => UsbStage::Unknown,
        }
    }
}

/// USB 上下文（Android 上仅做占位，设备信息由 Kotlin 传入）
pub struct UsbContext {
    _valid: bool,
}

impl UsbContext {
    pub fn new() -> Result<Self, String> {
        Ok(UsbContext { _valid: true })
    }
}

// ============================================================================
// Android 专属：从 Kotlin 传入的 fd 打开设备
// ============================================================================

/// Android 上由 Kotlin 通过 JNI 传入的设备信息
#[derive(Debug, Clone, Copy)]
pub struct AndroidUsbDevice {
    /// UsbDeviceConnection 的文件描述符
    pub fd: i32,
    pub vid: u16,
    pub pid: u16,
}

static ANDROID_USB_DEVICE: std::sync::OnceLock<AndroidUsbDevice> = std::sync::OnceLock::new();

/// 由 JNI 层调用，把 Kotlin 拿到的 fd 注册进来
pub fn set_android_usb_device(fd: i32, vid: u16, pid: u16) {
    let _ = ANDROID_USB_DEVICE.set(AndroidUsbDevice { fd, vid, pid });
}

/// 取当前已注册的 Android USB 设备（供 device.rs 打开时使用）
pub fn get_android_usb_device() -> Option<AndroidUsbDevice> {
    ANDROID_USB_DEVICE.get().copied()
}

// ============================================================================
// 设备枚举：Android 上不主动枚举，直接返回 Kotlin 注册的设备
// ============================================================================

/// Android 不支持主动枚举，返回 Kotlin 已注册的设备
#[allow(dead_code)]
fn scan_nusb_devices() -> Vec<(u16, u16)> {
    match get_android_usb_device() {
        Some(d) => vec![(d.vid, d.pid)],
        None => Vec::new(),
    }
}

/// 返回第一个 MediaTek 设备的 (VID, PID, DeviceType)
#[allow(dead_code)]
pub fn get_first_mtk_vid_pid() -> Option<(u16, u16, DeviceType)> {
    scan_nusb_devices().into_iter().find_map(|(vid, pid)| {
        let device_type = DeviceType::from_vid_pid(vid, pid);
        log::trace!(
            "[USB] 获取第一个联发科VIDPID: 找到 BROM 设备 VID=0x{:04X} PID=0x{:04X} type={:?}",
            vid,
            pid,
            device_type
        );
        Some((vid, pid, device_type))
    })
}
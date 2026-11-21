//! Low-level Windows SetupAPI FFI bindings and constants.
//!
//! All SetupAPI extern declarations and flag constants live here and are used
//! on demand by detect/switch and others, avoiding duplicated unsafe extern blocks.

#![cfg(target_os = "windows")]
// Note: `INF_NAME` / `INSTALLFLAG_*` / `UpdateDriverForPlugAndPlayDevicesW` /
// `SetupDiRestartDevices` / `find_and_restart_brom_device` are used only by the driver
// install path (switch.rs, requires the `winusb-driver` feature), so each is gated by
// `#[cfg(feature = "winusb-driver")]` instead of a module-level `#![allow(dead_code)]`,
// keeping zero dead_code warnings when the feature is off.

// =============================================================================
// SetupAPI handles and structs
// =============================================================================

#[repr(C)]
pub(crate) struct Hwnd__ {
    _unused: [u8; 0],
}
pub(crate) type Hwnd = *mut Hwnd__;

/// SetupAPI device info set handle (opaque)
#[repr(C)]
pub(crate) struct Hdevinfo__ {
    _unused: [u8; 0],
}
pub(crate) type Hdevinfo = *mut Hdevinfo__;

/// SetupAPI device info struct
#[repr(C)]
pub(crate) struct SpDevinfoData {
    pub(crate) cb_size: u32,
    pub(crate) class_guid: [u8; 16],
    pub(crate) dev_inst: u32,
    pub(crate) reserved: usize,
}

impl SpDevinfoData {
    pub(crate) fn new() -> Self {
        Self {
            cb_size: std::mem::size_of::<SpDevinfoData>() as u32,
            class_guid: [0u8; 16],
            dev_inst: 0,
            reserved: 0,
        }
    }
}

pub(crate) const MTK_VID: u16 = 0x0E8D;
pub(crate) const MTK_BROM_PID: u16 = 0x0003;
/// Used only by the driver install path (switch.rs); gated by the `winusb-driver` feature.
#[cfg(feature = "winusb-driver")]
pub(crate) const INF_NAME: &str = "mtk_brom_winusb.inf";

// =============================================================================
// SetupAPI constants
// =============================================================================

/// Used only by the driver install path (switch.rs); gated by the `winusb-driver` feature.
#[cfg(feature = "winusb-driver")]
pub(crate) const INSTALLFLAG_FORCE: u32 = 0x00000001;
/// Used only by the driver install path (switch.rs); gated by the `winusb-driver` feature.
#[cfg(feature = "winusb-driver")]
pub(crate) const INSTALLFLAG_NONINTERACTIVE: u32 = 0x00000004;
pub(crate) const DIGCF_PRESENT: u32 = 0x00000002;
pub(crate) const DIGCF_ALLCLASSES: u32 = 0x00000004;
pub(crate) const SPDRP_HARDWAREID: u32 = 0x00000001;
pub(crate) const SPDRP_DEVICEDESC: u32 = 0x00000000;
pub(crate) const SPDRP_SERVICE: u32 = 0x00000004;
pub(crate) const SPDRP_MFG: u32 = 0x0000000B;
pub(crate) const SPDRP_COMPATIBLEIDS: u32 = 0x00000002;
pub(crate) const SPDRP_PORTNAME: u32 = 0x0000001C;

// =============================================================================
// SetupAPI + UpdateDriverForPlugAndPlayDevicesW extern functions
// =============================================================================

#[link(name = "setupapi")]
unsafe extern "system" {
    /// `UpdateDriverForPlugAndPlayDevicesW` - force-install a driver matching a hardware ID.
    ///
    /// When `InstallFlags` contains `INSTALLFLAG_FORCE`, existing drivers are force-overwritten.
    /// This is the API Zadig uses internally.
    /// Used only by the driver install path (switch.rs); gated by the `winusb-driver` feature.
    #[cfg(feature = "winusb-driver")]
    pub(crate) fn UpdateDriverForPlugAndPlayDevicesW(
        hwndParent: Hwnd,
        hardwareId: *const u16,
        fullInfPath: *const u16,
        installFlags: u32,
        bRebootRequired: *mut i32,
    ) -> i32;

    /// `SetupDiGetClassDevsW` - obtain a device info set
    pub(crate) fn SetupDiGetClassDevsW(
        class_guid: *const u8,
        enumerator: *const u16,
        hwnd_parent: Hwnd,
        flags: u32,
    ) -> Hdevinfo;

    /// `SetupDiEnumDeviceInfo` - enumerate devices in a device info set
    pub(crate) fn SetupDiEnumDeviceInfo(
        device_info_set: Hdevinfo,
        member_index: u32,
        device_info_data: *mut SpDevinfoData,
    ) -> i32;

    /// `SetupDiGetDeviceRegistryPropertyW` - read a device registry property
    pub(crate) fn SetupDiGetDeviceRegistryPropertyW(
        device_info_set: Hdevinfo,
        device_info_data: *const SpDevinfoData,
        property: u32,
        property_reg_data_type: *mut u32,
        property_buffer: *mut u8,
        property_buffer_size: u32,
        required_size: *mut u32,
    ) -> i32;

    /// `SetupDiDestroyDeviceInfoList` - release a device info set
    pub(crate) fn SetupDiDestroyDeviceInfoList(device_info_set: Hdevinfo) -> i32;

    /// `SetupDiRestartDevices` - restart (re-enumerate) a device node so a newly installed driver takes effect.
    ///
    /// After force-installing WinUSB, the connected device instance may still be bound to the old
    /// driver until re-enumerated. This API spares the user a manual replug and is key to reliable
    /// Zadig-style installation.
    /// Used only by the driver install path (switch.rs); gated by the `winusb-driver` feature.
    #[cfg(feature = "winusb-driver")]
    pub(crate) fn SetupDiRestartDevices(
        device_info_set: Hdevinfo,
        device_info_data: *const SpDevinfoData,
    ) -> i32;
}

/// Enumerate all currently attached USB devices
pub(crate) unsafe fn enum_usb_devices() -> Result<Hdevinfo, String> {
    let usb_enum: Vec<u16> = "USB\0".encode_utf16().collect();
    let h = unsafe {
        SetupDiGetClassDevsW(
            std::ptr::null(),
            usb_enum.as_ptr(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        )
    };
    if h.is_null() {
        Err("SetupDiGetClassDevsW (USB) 失败".to_string())
    } else {
        Ok(h)
    }
}

/// Enumerate all currently attached port (COM) devices
pub(crate) unsafe fn enum_ports_devices() -> Result<Hdevinfo, String> {
    let ports_enum: Vec<u16> = "Ports\0".encode_utf16().collect();
    let h = unsafe {
        SetupDiGetClassDevsW(
            std::ptr::null(),
            ports_enum.as_ptr(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        )
    };
    if h.is_null() {
        Err("SetupDiGetClassDevsW (Ports) 失败".to_string())
    } else {
        Ok(h)
    }
}

/// Read a wide-string registry property
pub(crate) unsafe fn read_reg_wide(
    info: Hdevinfo,
    dev: &SpDevinfoData,
    property: u32,
) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut required_size: u32 = 0;
    let mut reg_type: u32 = 0;
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            info,
            dev,
            property,
            &mut reg_type,
            buf.as_mut_ptr() as *mut u8,
            (buf.len() * 2) as u32,
            &mut required_size,
        )
    };
    if ok != 0 {
        let s = String::from_utf16_lossy(&buf[..(required_size as usize / 2)])
            .trim_end_matches('\0')
            .to_string();
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    }
}

/// Find the MTK BROM device node (VID_0E8D&PID_0003) and actively trigger re-enumeration.
///
/// Used after force-installing the WinUSB driver so the new driver binds to the current device
/// immediately (no manual replug). On failure it just returns an error; callers should fall back
/// to polling and must not abort the install flow.
/// Used only by the driver install path (switch.rs); gated by the `winusb-driver` feature.
#[cfg(feature = "winusb-driver")]
pub(crate) unsafe fn find_and_restart_brom_device() -> Result<(), String> {
    let h = unsafe { enum_usb_devices()? };

    let mut dev_data = SpDevinfoData::new();
    let mut idx: u32 = 0;
    let mut found = false;
    while unsafe { SetupDiEnumDeviceInfo(h, idx, &mut dev_data) } != 0 {
        idx += 1;
        if let Some(hwid) = unsafe { read_reg_wide(h, &dev_data, SPDRP_HARDWAREID) } {
            if hwid.to_uppercase().contains("VID_0E8D&PID_0003") {
                found = true;
                break;
            }
        }
    }

    if !found {
        unsafe {
            SetupDiDestroyDeviceInfoList(h);
        }
        return Err("未找到 BROM 设备节点 (VID_0E8D&PID_0003)".to_string());
    }

    let ret = unsafe { SetupDiRestartDevices(h, &dev_data) };
    unsafe {
        SetupDiDestroyDeviceInfoList(h);
    }
    if ret == 0 {
        Err(format!(
            "SetupDiRestartDevices 失败: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

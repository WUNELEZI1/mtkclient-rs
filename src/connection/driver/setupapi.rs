//! Windows SetupAPI 底层 FFI 绑定 + 常量
//!
//! 集中存放所有 SetupAPI 外部函数声明与 flags 常量，
//! 由 detect/switch 等子模块按需使用，避免在多个文件里重复 unsafe extern 块。

#![cfg(target_os = "windows")]

// =============================================================================
// SetupAPI 句柄与结构体
// =============================================================================

#[repr(C)]
pub(crate) struct Hwnd__ {
    _unused: [u8; 0],
}
pub(crate) type Hwnd = *mut Hwnd__;

/// SetupAPI 设备信息集句柄（不透明）
#[repr(C)]
pub(crate) struct Hdevinfo__ {
    _unused: [u8; 0],
}
pub(crate) type Hdevinfo = *mut Hdevinfo__;

/// SetupAPI 设备信息结构
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
pub(crate) const INF_NAME: &str = "mtk_brom_winusb.inf";

// =============================================================================
// SetupAPI 常量
// =============================================================================

pub(crate) const INSTALLFLAG_FORCE: u32 = 0x00000001;
pub(crate) const INSTALLFLAG_NONINTERACTIVE: u32 = 0x00000004;
pub(crate) const DIGCF_PRESENT: u32 = 0x00000002;
pub(crate) const DIGCF_ALLCLASSES: u32 = 0x00000004;
pub(crate) const SPDRP_HARDWAREID: u32 = 0x00000001;
pub(crate) const SPDRP_DEVICEDESC: u32 = 0x00000000;
pub(crate) const SPDRP_MFG: u32 = 0x0000000B;
pub(crate) const SPDRP_DRIVER: u32 = 0x0000000C;
pub(crate) const SPDRP_DEVTYPE: u32 = 0x0000001F;
pub(crate) const SPDRP_COMPATIBLEIDS: u32 = 0x00000002;
pub(crate) const REG_SZ: u32 = 1;
pub(crate) const SPDRP_PORTNAME: u32 = 0x0000001C;

// =============================================================================
// SetupAPI + UpdateDriverForPlugAndPlayDevicesW 外部函数
// =============================================================================

#[link(name = "setupapi")]
unsafe extern "system" {
    /// `UpdateDriverForPlugAndPlayDevicesW` - 强制安装匹配硬件 ID 的驱动
    ///
    /// 当 `InstallFlags` 包含 `INSTALLFLAG_FORCE` 时，会强制覆盖现有驱动。
    /// Zadig 内部用的就是这个 API。
    pub(crate) fn UpdateDriverForPlugAndPlayDevicesW(
        hwndParent: Hwnd,
        hardwareId: *const u16,
        fullInfPath: *const u16,
        installFlags: u32,
        bRebootRequired: *mut i32,
    ) -> i32;

    /// `SetupDiGetClassDevsW` - 获取设备信息集
    pub(crate) fn SetupDiGetClassDevsW(
        class_guid: *const u8,
        enumerator: *const u16,
        hwnd_parent: Hwnd,
        flags: u32,
    ) -> Hdevinfo;

    /// `SetupDiEnumDeviceInfo` - 枚举设备信息集中的设备
    pub(crate) fn SetupDiEnumDeviceInfo(
        device_info_set: Hdevinfo,
        member_index: u32,
        device_info_data: *mut SpDevinfoData,
    ) -> i32;

    /// `SetupDiGetDeviceRegistryPropertyW` - 获取设备注册表属性
    pub(crate) fn SetupDiGetDeviceRegistryPropertyW(
        device_info_set: Hdevinfo,
        device_info_data: *const SpDevinfoData,
        property: u32,
        property_reg_data_type: *mut u32,
        property_buffer: *mut u8,
        property_buffer_size: u32,
        required_size: *mut u32,
    ) -> i32;

    /// `SetupDiDestroyDeviceInfoList` - 释放设备信息集
    pub(crate) fn SetupDiDestroyDeviceInfoList(device_info_set: Hdevinfo) -> i32;
}

/// 枚举所有当前接入的 USB 设备
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

/// 枚举所有当前接入的端口（COM）设备
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

/// 读取注册表宽字符串属性
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

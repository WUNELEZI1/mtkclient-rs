//! BROM device driver detection (query only; never opens the device).
//!
//! Enumerates the USB bus via SetupAPI to identify the driver currently bound to the BROM device:
//! - `BromDriverType::WinUsb` → WinUSB driver installed by libwdi
//! - `BromDriverType::Serial` → MediaTek Inc. serial driver
//! - `BromDriverType::Unknown(name)` → any other driver (name preserved)
//!
//! Benefit: only reads the registry and never opens the device, avoiding COM port contention.

#[cfg(target_os = "windows")]
use log::trace;

#[cfg(target_os = "windows")]
use super::setupapi::{
    self, MTK_BROM_PID, MTK_VID, SPDRP_COMPATIBLEIDS, SPDRP_DEVICEDESC, SPDRP_HARDWAREID,
    SPDRP_MFG, SPDRP_PORTNAME, SPDRP_SERVICE, SpDevinfoData, enum_ports_devices, enum_usb_devices,
    read_reg_wide,
};

/// BROM device driver type.
///
/// Constructed only by the Windows SetupAPI enumeration path (`check_brom_driver_type` /
/// `detect_brom_driver_from_usb_bus`) and the classification logic; other platforms have no
/// driver-type concept. Unit tests (`#[cfg(test)]`) reference it directly, so it is defined
/// only for Windows or test builds.
#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, PartialEq)]
pub enum BromDriverType {
    /// WinUSB driver (installed by libwdi)
    WinUsb,
    /// Native serial driver (MediaTek Inc.)
    Serial,
    /// Unknown driver (carries the manufacturer name)
    Unknown(String),
}

/// USB bus detection result.
///
/// `WinUsbReady` is also constructed on non-Windows by the direct nusb probe (see the
/// non-Windows `detect_brom_driver_from_usb_bus` below), so it is not platform-gated;
/// `SerialPort` / `Unknown` can only come from Windows SetupAPI classification, so they are
/// defined on Windows only.
#[derive(Debug, Clone)]
pub enum UsbBusDetectionResult {
    /// No device found
    NotFound,
    /// WinUSB driver (libwdi); accessible directly via libusb
    WinUsbReady,
    /// Serial driver (MediaTek); requires a port handshake before switching to WinUSB
    #[cfg(target_os = "windows")]
    SerialPort(String), // COM port name
    /// Unknown driver
    #[cfg(target_os = "windows")]
    Unknown(String), // driver manufacturer
}

#[cfg(any(target_os = "windows", test))]
fn classify_brom_driver_from_fields(
    service: &str,
    mfg: &str,
    device_desc: &str,
    compatible_ids: &str,
) -> BromDriverType {
    let service_lower = service.to_lowercase();
    let mfg_lower = mfg.to_lowercase();
    let desc_lower = device_desc.to_lowercase();
    let compatible_lower = compatible_ids.to_lowercase();

    if service_lower.contains("winusb")
        || mfg_lower.contains("libwdi")
        || compatible_lower.contains("winusb")
        || compatible_lower.contains("ms_compatible_id")
    {
        BromDriverType::WinUsb
    } else if service_lower.contains("wdm_usb")
        || service_lower.contains("usbser")
        || (desc_lower.contains("mediatek usb port") && mfg_lower.contains("mediatek"))
    {
        BromDriverType::Serial
    } else {
        BromDriverType::Unknown(if mfg.is_empty() {
            device_desc.to_string()
        } else {
            mfg.to_string()
        })
    }
}

/// USB device info for a COM port
#[derive(Debug, Clone)]
pub struct ComPortUsbInfo {
    /// Device description (e.g. "MediaTek USB Port")
    pub device_desc: String,
    /// Driver manufacturer (e.g. "libwdi" or "MediaTek Inc.")
    pub driver_mfg: String,
}

/// Whether the WinUSB driver switch can be skipped: only when WinUSB is confirmed bound.
///
/// Pure classification logic kept here so it can be unit-tested on every platform
/// (no Windows SetupAPI dependency).
/// - WinUsb → skip (already the target driver)
/// - Serial → do not skip (switch still needed, even if the USB bus is enumerable)
/// - Unknown → do not skip (driver missing/unknown, install still needed)
///
/// Used only by the Windows + `winusb-driver` production path (switch.rs); otherwise referenced by tests.
#[cfg(any(all(target_os = "windows", feature = "winusb-driver"), test))]
pub fn should_skip_switch_for_driver(driver_type: &BromDriverType) -> bool {
    matches!(driver_type, BromDriverType::WinUsb)
}

/// Enumerate all COM port names available on the system.
/// Uses the serialport crate (queries the Windows registry HARDWARE\DEVICEMAP\SERIALCOMM),
/// which is more reliable than enumerating the SetupAPI Ports class.
pub fn enumerate_all_com_ports() -> Vec<String> {
    match serialport::available_ports() {
        Ok(ports) => ports.into_iter().map(|p| p.port_name).collect(),
        Err(e) => {
            log::warn!("[COM] 枚举 COM 口失败: {}", e);
            Vec::new()
        }
    }
}

/// Used only by the driver switch path (switch.rs) on Windows + `winusb-driver`.
#[cfg(all(target_os = "windows", feature = "winusb-driver"))]
pub fn check_brom_driver_type() -> Result<BromDriverType, String> {
    unsafe {
        let device_info_set = enum_usb_devices()?;
        let target_hardware_id =
            format!("USB\\VID_{:04X}&PID_{:04X}", MTK_VID, MTK_BROM_PID).to_uppercase();

        let mut dev_info = SpDevinfoData::new();
        let mut result = Err("未找到 BROM 设备".to_string());

        for index in 0..256 {
            if setupapi::SetupDiEnumDeviceInfo(device_info_set, index, &mut dev_info) == 0 {
                break;
            }

            if let Some(hw_id) = read_reg_wide(device_info_set, &dev_info, SPDRP_HARDWAREID)
                && hw_id.to_uppercase().contains(&target_hardware_id)
            {
                let service =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_SERVICE).unwrap_or_default();
                let mfg = read_reg_wide(device_info_set, &dev_info, SPDRP_MFG).unwrap_or_default();
                let device_desc =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_DEVICEDESC).unwrap_or_default();
                let compatible_ids = read_reg_wide(device_info_set, &dev_info, SPDRP_COMPATIBLEIDS)
                    .unwrap_or_default();

                result = Ok(classify_brom_driver_from_fields(
                    &service,
                    &mfg,
                    &device_desc,
                    &compatible_ids,
                ));
                break;
            }
        }

        setupapi::SetupDiDestroyDeviceInfoList(device_info_set);
        result
    }
}

// Non-Windows has no SetupAPI, and `check_brom_driver_type` has no real call site (the
// switch.rs driver switch compiles only on Windows + `winusb-driver`), so its placeholder
// stub was removed and no `#[cfg(not(target_os = "windows"))]` branch is kept.

#[cfg(target_os = "windows")]
pub fn detect_brom_driver_from_usb_bus() -> UsbBusDetectionResult {
    unsafe {
        let device_info_set = match enum_usb_devices() {
            Ok(h) => h,
            Err(_) => return UsbBusDetectionResult::NotFound,
        };

        let target_hardware_id =
            format!("USB\\VID_{:04X}&PID_{:04X}", MTK_VID, MTK_BROM_PID).to_uppercase();
        let mut dev_info = SpDevinfoData::new();
        let mut result = UsbBusDetectionResult::NotFound;

        for index in 0..256 {
            if setupapi::SetupDiEnumDeviceInfo(device_info_set, index, &mut dev_info) == 0 {
                break;
            }

            if let Some(hw_id) = read_reg_wide(device_info_set, &dev_info, SPDRP_HARDWAREID) {
                if !hw_id.to_uppercase().contains(&target_hardware_id) {
                    continue;
                }

                let device_desc =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_DEVICEDESC).unwrap_or_default();
                let driver_mfg =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_MFG).unwrap_or_default();
                let service =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_SERVICE).unwrap_or_default();
                let compatible_ids = read_reg_wide(device_info_set, &dev_info, SPDRP_COMPATIBLEIDS)
                    .unwrap_or_default();

                trace!(
                    "[USB_BUS] 找到 BROM 设备: desc='{}', mfg='{}', service='{}'",
                    device_desc, driver_mfg, service
                );

                result = match classify_brom_driver_from_fields(
                    &service,
                    &driver_mfg,
                    &device_desc,
                    &compatible_ids,
                ) {
                    BromDriverType::WinUsb => UsbBusDetectionResult::WinUsbReady,
                    // Serial driver: do not resolve the COM port name here; the caller enumerates all COM ports and tries each
                    BromDriverType::Serial => UsbBusDetectionResult::SerialPort(String::new()),
                    BromDriverType::Unknown(name) => UsbBusDetectionResult::Unknown(name),
                };
                break;
            }
        }

        setupapi::SetupDiDestroyDeviceInfoList(device_info_set);
        result
    }
}

#[cfg(not(target_os = "windows"))]
pub fn detect_brom_driver_from_usb_bus() -> UsbBusDetectionResult {
    // Non-Windows has no SetupAPI, so the driver type cannot be read; instead use nusb to check
    // whether a BROM device (VID=0x0E8D, PID=0x0003) is already visible. If so, nusb can open it
    // directly (usbfs on Linux, equivalent to WinUSB ready on Windows), so return `WinUsbReady`
    // and let the caller use the direct USB path; otherwise return `NotFound` and fall back to
    // serial enumeration.
    if crate::usb::get_first_mtk_vid_pid().is_some() {
        UsbBusDetectionResult::WinUsbReady
    } else {
        UsbBusDetectionResult::NotFound
    }
}

#[cfg(target_os = "windows")]
pub fn query_com_port_usb_info(com_port: &str) -> Option<ComPortUsbInfo> {
    unsafe {
        let device_info_set = enum_ports_devices().ok()?;
        let mut dev_info = SpDevinfoData::new();

        for index in 0..256 {
            if setupapi::SetupDiEnumDeviceInfo(device_info_set, index, &mut dev_info) == 0 {
                break;
            }

            if let Some(port_name) = read_reg_wide(device_info_set, &dev_info, SPDRP_PORTNAME) {
                if port_name != com_port {
                    continue;
                }
                let device_desc =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_DEVICEDESC).unwrap_or_default();
                let driver_mfg =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_MFG).unwrap_or_default();
                setupapi::SetupDiDestroyDeviceInfoList(device_info_set);
                return Some(ComPortUsbInfo {
                    device_desc,
                    driver_mfg,
                });
            }
        }

        setupapi::SetupDiDestroyDeviceInfoList(device_info_set);
        None
    }
}

#[cfg(not(target_os = "windows"))]
pub fn query_com_port_usb_info(_com_port: &str) -> Option<ComPortUsbInfo> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winusb_service_wins_over_mediatek_usb_port_name() {
        assert_eq!(
            classify_brom_driver_from_fields("WinUSB", "MediaTek Inc.", "MediaTek USB Port", ""),
            BromDriverType::WinUsb
        );
    }

    #[test]
    fn libwdi_mfg_wins_over_mediatek_usb_port_name() {
        assert_eq!(
            classify_brom_driver_from_fields("", "libwdi", "MediaTek USB Port", ""),
            BromDriverType::WinUsb
        );
    }

    #[test]
    fn wdm_usb_service_is_serial_driver() {
        assert_eq!(
            classify_brom_driver_from_fields("wdm_usb", "MediaTek Inc.", "MediaTek USB Port", ""),
            BromDriverType::Serial
        );
    }

    #[test]
    fn usbser_service_is_serial_driver() {
        assert_eq!(
            classify_brom_driver_from_fields("usbser", "", "MediaTek USB Port", ""),
            BromDriverType::Serial
        );
    }

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

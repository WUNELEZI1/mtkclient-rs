//! BROM 设备驱动检测（只查询，不打开）
//!
//! 通过 SetupAPI 枚举 USB 总线，识别 BROM 设备当前使用的驱动类型：
//! - `BromDriverType::WinUsb` → libwdi 安装的 WinUSB 驱动
//! - `BromDriverType::Serial` → MediaTek Inc. 串口驱动
//! - `BromDriverType::Unknown(name)` → 其他驱动（保留名称）
//!
//! 优势：只读注册表，不实际打开设备，避免 COM 口占用问题。

#[cfg(target_os = "windows")]
use log::trace;

#[cfg(target_os = "windows")]
use super::setupapi::{
    self, MTK_BROM_PID, MTK_VID, SPDRP_COMPATIBLEIDS, SPDRP_DEVICEDESC, SPDRP_HARDWAREID,
    SPDRP_MFG, SPDRP_PORTNAME, SPDRP_SERVICE, SpDevinfoData, enum_ports_devices, enum_usb_devices,
    read_reg_wide,
};

/// BROM 设备驱动类型
///
/// 仅由 Windows 的 SetupAPI 枚举路径（`check_brom_driver_type` /
/// `detect_brom_driver_from_usb_bus`）与分类逻辑构造，非 Windows 无驱动类型概念；
/// 单测（`#[cfg(test)]`）亦直接引用，故仅在 Windows 或测试构建中定义。
#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, PartialEq)]
pub enum BromDriverType {
    /// WinUSB 驱动（libwdi 安装）
    WinUsb,
    /// 原始串口驱动（MediaTek Inc.）
    Serial,
    /// 未知驱动（包含制造商名称）
    Unknown(String),
}

/// USB 总线检测结果
///
/// `WinUsbReady` 在非 Windows 下由 nusb 直连探测构造（见本文件非 Windows 版
/// `detect_brom_driver_from_usb_bus`），故不做平台门控；`SerialPort` / `Unknown`
/// 仅能由 Windows SetupAPI 驱动分类产生，因此只在 Windows 下定义。
#[derive(Debug, Clone)]
pub enum UsbBusDetectionResult {
    /// 未找到设备
    NotFound,
    /// WinUSB 驱动（libwdi），可以直接用 libusb 访问
    WinUsbReady,
    /// 串口驱动（MediaTek），需要先打开串口握手再切换 WinUSB
    #[cfg(target_os = "windows")]
    SerialPort(String), // COM 口名称
    /// 未知驱动
    #[cfg(target_os = "windows")]
    Unknown(String), // 驱动制造商
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

/// COM 口对应的 USB 设备信息
#[derive(Debug, Clone)]
pub struct ComPortUsbInfo {
    /// 设备描述（如 "MediaTek USB Port"）
    pub device_desc: String,
    /// 驱动制造商（如 "libwdi" 或 "MediaTek Inc."）
    pub driver_mfg: String,
}

/// 是否应跳过 WinUSB 驱动切换：仅当已确认绑定 WinUSB 时才跳过。
///
/// 纯分类逻辑，放此处以便在所有平台单测（不依赖 Windows SetupAPI）。
/// - WinUsb → 跳过（已是目标驱动）
/// - Serial → 不跳过（仍需切换，即使 USB 总线可枚举）
/// - Unknown → 不跳过（驱动未安装/未知，仍需安装）
///
/// 仅在 Windows + `winusb-driver` 的生产路径（switch.rs）使用，其余情况由测试引用。
#[cfg(any(all(target_os = "windows", feature = "winusb-driver"), test))]
pub fn should_skip_switch_for_driver(driver_type: &BromDriverType) -> bool {
    matches!(driver_type, BromDriverType::WinUsb)
}

/// 枚举系统所有可用 COM 口名称
/// 使用 serialport crate（查询 Windows 注册表 HARDWARE\DEVICEMAP\SERIALCOMM），
/// 比 SetupAPI 枚举 Ports 类更可靠。
pub fn enumerate_all_com_ports() -> Vec<String> {
    match serialport::available_ports() {
        Ok(ports) => ports.into_iter().map(|p| p.port_name).collect(),
        Err(e) => {
            log::warn!("[COM] 枚举 COM 口失败: {}", e);
            Vec::new()
        }
    }
}

/// 仅在 Windows + `winusb-driver` 的驱动切换路径（switch.rs）使用。
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

// 非 Windows 没有 SetupAPI，`check_brom_driver_type` 无真实调用点（switch.rs 的驱动
// 切换仅在 Windows + `winusb-driver` 下编译），故删除其占位桩实现，不再保留
// `#[cfg(not(target_os = "windows"))]` 分支。

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
                    // 串口驱动：不在这里找 COM 口名，交给上层枚举所有 COM 口逐个握手
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
    // 非 Windows 无 SetupAPI，无法读取驱动类型；改用 nusb 直接枚举是否已存在
    // BROM 设备（VID=0x0E8D, PID=0x0003）。若可见，说明该设备已能被 nusb 直接
    // 打开（Linux 走 usbfs，语义等同于 Windows 的 WinUSB 就绪），返回
    // `WinUsbReady` 让上层走 USB 直连；否则返回 `NotFound` 退回串口枚举。
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

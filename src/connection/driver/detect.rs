//! BROM 设备驱动检测（只查询，不打开）
//!
//! 通过 SetupAPI 枚举 USB 总线，识别 BROM 设备当前使用的驱动类型：
//! - `BromDriverType::WinUsb` → libwdi 安装的 WinUSB 驱动
//! - `BromDriverType::Serial` → MediaTek Inc. 串口驱动
//! - `BromDriverType::Unknown(name)` → 其他驱动（保留名称）
//!
//! 优势：只读注册表，不实际打开设备，避免 COM 口占用问题。

use log::trace;

use super::setupapi::{
    self, MTK_BROM_PID, MTK_VID, SPDRP_COMPATIBLEIDS, SPDRP_DEVICEDESC, SPDRP_HARDWAREID,
    SPDRP_MFG, SPDRP_PORTNAME, SpDevinfoData, enum_ports_devices, enum_usb_devices, read_reg_wide,
};

/// BROM 设备驱动类型
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
#[derive(Debug, Clone)]
pub enum UsbBusDetectionResult {
    /// 未找到设备
    NotFound,
    /// WinUSB 驱动（libwdi），可以直接用 libusb 访问
    WinUsbReady,
    /// 串口驱动（MediaTek），需要先打开串口握手再切换 WinUSB
    SerialPort(String), // COM 口名称
    /// 未知驱动
    Unknown(String), // 驱动制造商
}

/// COM 口对应的 USB 设备信息
#[derive(Debug, Clone)]
pub struct ComPortUsbInfo {
    /// 设备描述（如 "MediaTek USB Port"）
    pub device_desc: String,
    /// 驱动制造商（如 "libwdi" 或 "MediaTek Inc."）
    pub driver_mfg: String,
}

/// 通过硬件 ID 查找 COM 口
/// 比 find_com_port_for_brom_device 更可靠：不依赖设备描述，直接用 VID/PID 硬件 ID 关联
#[cfg(target_os = "windows")]
fn find_com_port_by_hardware_id() -> Option<String> {
    unsafe {
        let device_info_set = enum_ports_devices().ok()?;
        let mut dev_info = SpDevinfoData::new();
        let mut result = None;

        // 目标硬件 ID 的多种可能格式
        let target_vid_pid = format!("VID_{:04X}&PID_{:04X}", MTK_VID, MTK_BROM_PID).to_uppercase();

        for index in 0..256 {
            if setupapi::SetupDiEnumDeviceInfo(device_info_set, index, &mut dev_info) == 0 {
                break;
            }

            // 读取所有兼容硬件 ID（SPDRP_COMPATIBLEIDS）
            // 串口驱动注册的兼容 ID 通常包含 "USB\VID_0E8D&PID_0003" 格式
            if let Some(compatible_ids) =
                read_reg_wide(device_info_set, &dev_info, SPDRP_COMPATIBLEIDS)
                && compatible_ids.to_uppercase().contains(&target_vid_pid)
                && let Some(port_name) = read_reg_wide(device_info_set, &dev_info, SPDRP_PORTNAME)
            {
                let device_desc =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_DEVICEDESC).unwrap_or_default();
                trace!(
                    "[USB_BUS] 通过硬件 ID 找到 COM 口: {} (desc='{}')",
                    port_name, device_desc
                );
                result = Some(port_name);
                break;
            }

            // 也检查 SPDRP_HARDWAREID
            if let Some(hw_id) = read_reg_wide(device_info_set, &dev_info, SPDRP_HARDWAREID)
                && hw_id.to_uppercase().contains(&target_vid_pid)
                && let Some(port_name) = read_reg_wide(device_info_set, &dev_info, SPDRP_PORTNAME)
            {
                let device_desc =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_DEVICEDESC).unwrap_or_default();
                trace!(
                    "[USB_BUS] 通过 HardwareID 找到 COM 口: {} (desc='{}')",
                    port_name, device_desc
                );
                result = Some(port_name);
                break;
            }
        }

        setupapi::SetupDiDestroyDeviceInfoList(device_info_set);
        result
    }
}

#[cfg(not(target_os = "windows"))]
fn find_com_port_by_hardware_id() -> Option<String> {
    None
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

#[cfg(target_os = "windows")]
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
                if let Some(mfg) = read_reg_wide(device_info_set, &dev_info, SPDRP_MFG) {
                    let mfg_lower = mfg.to_lowercase();
                    if mfg_lower.contains("libwdi") {
                        result = Ok(BromDriverType::WinUsb);
                    } else if mfg_lower.contains("mediatek") {
                        result = Ok(BromDriverType::Serial);
                    } else {
                        result = Ok(BromDriverType::Unknown(mfg));
                    }
                }
                break;
            }
        }

        setupapi::SetupDiDestroyDeviceInfoList(device_info_set);
        result
    }
}

#[cfg(not(target_os = "windows"))]
pub fn check_brom_driver_type() -> Result<BromDriverType, String> {
    Err("仅 Windows 支持".to_string())
}

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

                trace!(
                    "[USB_BUS] 找到 BROM 设备: desc='{}', mfg='{}'",
                    device_desc, driver_mfg
                );

                let desc_lower = device_desc.to_lowercase();
                let mfg_lower = driver_mfg.to_lowercase();

                if desc_lower.contains("mediatek usb port") {
                    if mfg_lower.contains("libwdi") {
                        result = UsbBusDetectionResult::WinUsbReady;
                    } else if mfg_lower.contains("mediatek") {
                        // 串口驱动：不在这里找 COM 口名，交给上层枚举所有 COM 口逐个握手
                        result = UsbBusDetectionResult::SerialPort(String::new());
                    } else {
                        result = UsbBusDetectionResult::Unknown(driver_mfg);
                    }
                } else {
                    // 设备描述不匹配，但仍在 USB 总线上 → 未知
                    result = UsbBusDetectionResult::Unknown(device_desc);
                }
                break;
            }
        }

        setupapi::SetupDiDestroyDeviceInfoList(device_info_set);
        result
    }
}

#[cfg(not(target_os = "windows"))]
pub fn detect_brom_driver_from_usb_bus() -> UsbBusDetectionResult {
    UsbBusDetectionResult::NotFound
}

#[cfg(target_os = "windows")]
fn find_com_port_for_brom_device() -> Option<String> {
    unsafe {
        let device_info_set = enum_ports_devices().ok()?;
        let mut dev_info = SpDevinfoData::new();
        let mut result = None;

        for index in 0..256 {
            if setupapi::SetupDiEnumDeviceInfo(device_info_set, index, &mut dev_info) == 0 {
                break;
            }

            if let Some(port_name) = read_reg_wide(device_info_set, &dev_info, SPDRP_PORTNAME)
                && let Some(device_desc) =
                    read_reg_wide(device_info_set, &dev_info, SPDRP_DEVICEDESC)
                && device_desc.to_lowercase().contains("mediatek usb port")
            {
                trace!(
                    "[USB_BUS] 找到 COM 口: {} (desc='{}')",
                    port_name, device_desc
                );
                result = Some(port_name);
                break;
            }
        }

        setupapi::SetupDiDestroyDeviceInfoList(device_info_set);
        result
    }
}

#[cfg(not(target_os = "windows"))]
fn find_com_port_for_brom_device() -> Option<String> {
    None
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

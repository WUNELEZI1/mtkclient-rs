use crate::config::DeviceType;
use log::{debug, info, warn};

/// USB 设备状态
#[derive(Debug, Clone, PartialEq)]
pub enum UsbDiagState {
    /// 未检测到任何 MediaTek 设备
    NoDevice,
    /// 检测到设备但驱动异常（如 CDC 而非 libusb）
    WrongDriver,
    /// 检测到 Preloader 模式（PID 0x0003/0x0001）
    Preloader,
    /// 检测到 BROM 模式
    Brom,
    /// 端点通信异常（libusb 报错）
    #[allow(dead_code)] // 预留：底层 USB 故障分支，当前诊断流程未必总会构造
    EndpointError,
    /// 设备连接但握手失败
    #[allow(dead_code)] // 预留：握手失败的诊断分支，供主流程错误映射使用
    HandshakeFailed,
}

/// 扫描系统中是否有 MediaTek USB 设备
/// 返回 (vid, pid, is_brom, driver_info)
pub fn scan_mediatek_devices() -> Vec<(u16, u16, bool, String)> {
    let mut results = Vec::new();

    unsafe {
        let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut ctx) != 0 {
            warn!("[USB诊断] libusb_init 失败");
            return results;
        }

        let mut dev_list: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let dev_count = libusb1_sys::libusb_get_device_list(ctx, &mut dev_list);
        if dev_count < 0 {
            warn!("[USB诊断] get_device_list 失败: {}", dev_count);
            libusb1_sys::libusb_exit(ctx);
            return results;
        }

        for i in 0..dev_count as isize {
            let dev = *dev_list.wrapping_offset(i);
            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(dev, &mut desc) != 0 {
                continue;
            }

            let vid = desc.idVendor;
            let pid = desc.idProduct;

            // 只关心中 MediaTek 设备
            if vid != 0x0E8D {
                continue;
            }

            let dev_config = DeviceType::from_vid_pid(vid, pid);
            let is_brom = dev_config.is_brom();

            // 尝试打开设备以获取更多信息
            let mut handle: *mut libusb1_sys::libusb_device_handle = std::ptr::null_mut();
            let driver_info = if libusb1_sys::libusb_open(dev, &mut handle) == 0 {
                // 尝试 claim interface 来判断驱动状态
                let claim_ret = libusb1_sys::libusb_claim_interface(handle, 1);
                let info = if claim_ret == 0 {
                    libusb1_sys::libusb_release_interface(handle, 1);
                    "libusb 正常".to_string()
                } else if claim_ret == -12 {
                    // LIBUSB_ERROR_NOT_FOUND — 接口不存在
                    "接口未找到".to_string()
                } else {
                    format!("claim_interface 失败: {}", claim_ret)
                };
                libusb1_sys::libusb_close(handle);
                info
            } else {
                "设备被其他驱动占用".to_string()
            };

            results.push((vid, pid, is_brom, driver_info));
        }

        libusb1_sys::libusb_free_device_list(dev_list, 1);
        libusb1_sys::libusb_exit(ctx);
    }

    results
}

/// 诊断 USB 连接状态
/// 对齐 Python Port.py handshake() + detectdevices()
pub fn diagnose_connection() -> UsbDiagState {
    let devices = scan_mediatek_devices();

    if devices.is_empty() {
        return UsbDiagState::NoDevice;
    }

    // 检查第一个设备即可
    let (vid, pid, is_brom, driver_info) = &devices[0];
    debug!(
        "[USB诊断] VID={:04X} PID={:04X} BROM={} 驱动={}",
        vid, pid, is_brom, driver_info
    );

    if driver_info.contains("被其他驱动占用") {
        return UsbDiagState::WrongDriver;
    }

    if !is_brom {
        return UsbDiagState::Preloader;
    }

    UsbDiagState::Brom
}

/// 打印诊断提示（类似 Python 的 loop == 5 提示）
pub fn print_connection_hint() {
    info!("");
    info!("设备连接提示:");
    info!("  1. 确保设备已关机");
    info!("  2. BROM 模式: 按住 音量+ + 音量- + (或所有按键) 并插入 USB");
    info!("  3. Preloader 模式: 不要按任何键，直接插入 USB");
    info!("  4. 如果已连接但无响应，按住电源键 10 秒重置");
    info!("  5. 运行 'check-driver' 确认驱动已安装");
    info!("");
}

/// 枚举 USB 设备，输出详细信息（用于 list-usb 命令）
pub fn enumerate_usb_devices() {
    info!("扫描 USB 设备...");

    unsafe {
        let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut ctx) != 0 {
            info!("libusb 初始化失败");
            return;
        }

        let mut dev_list: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let dev_count = libusb1_sys::libusb_get_device_list(ctx, &mut dev_list);
        if dev_count < 0 {
            info!("获取设备列表失败: {}", dev_count);
            libusb1_sys::libusb_exit(ctx);
            return;
        }

        info!("找到 {} 个 USB 设备", dev_count);

        for i in 0..dev_count as isize {
            let dev = *dev_list.wrapping_offset(i);
            let mut desc: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(dev, &mut desc) != 0 {
                continue;
            }

            let vid = desc.idVendor;
            let pid = desc.idProduct;

            // 显示所有设备，但高亮 MediaTek
            let is_mtk = vid == 0x0E8D;
            let dev_config = DeviceType::from_vid_pid(vid, pid);

            let mode = if is_mtk {
                if dev_config.is_brom() {
                    "BROM"
                } else if dev_config.is_preloader() {
                    "Preloader"
                } else {
                    "未知模式"
                }
            } else {
                ""
            };

            if is_mtk {
                info!(
                    "  [MediaTek] Bus {} Device {}: VID={:04X} PID={:04X} 模式={}",
                    libusb1_sys::libusb_get_bus_number(dev),
                    libusb1_sys::libusb_get_device_address(dev),
                    vid,
                    pid,
                    mode
                );
            } else {
                debug!(
                    "  Bus {} Device {}: VID={:04X} PID={:04X}",
                    libusb1_sys::libusb_get_bus_number(dev),
                    libusb1_sys::libusb_get_device_address(dev),
                    vid,
                    pid
                );
            }
        }

        libusb1_sys::libusb_free_device_list(dev_list, 1);
        libusb1_sys::libusb_exit(ctx);
    }
}

/// 检测是否有其他程序占用了 MediaTek 设备
pub fn check_device_occupation() -> Option<String> {
    let devices = scan_mediatek_devices();

    for (_vid, _pid, _is_brom, driver_info) in &devices {
        if driver_info.contains("被其他驱动占用") || driver_info.contains("busy") {
            return Some(
                "设备可能被其他程序占用。请关闭 SP Flash Tool、MCT 或其他 MTK 工具后重试。"
                    .to_string(),
            );
        }
    }

    None
}

/// 综合诊断报告（用于 diagnose 命令）
pub fn diagnose_and_report() {
    info!("=== USB 连接诊断 ===");
    info!("");

    // 1. 扫描 MediaTek 设备
    let devices = scan_mediatek_devices();

    if devices.is_empty() {
        warn!("未检测到任何 MediaTek USB 设备");
        info!("");
        info!("请检查:");
        info!("  1. USB 线缆是否连接良好");
        info!("  2. 设备是否已关机");
        info!("  3. 是否已进入 BROM 模式 (按住 音量+ + 音量- 插入 USB)");
        info!("  4. Windows 设备管理器中是否显示 MediaTek USB 设备");
        return;
    }

    info!("检测到 {} 个 MediaTek 设备:", devices.len());
    for (vid, pid, is_brom, driver_info) in &devices {
        let mode = if *is_brom { "BROM" } else { "Preloader" };
        info!(
            "  VID={:04X} PID={:04X} 模式={} 驱动={}",
            vid, pid, mode, driver_info
        );
    }
    info!("");

    // 2. 驱动状态检查
    let mut has_wrong_driver = false;
    let mut has_correct_driver = false;
    for (_vid, _pid, _is_brom, driver_info) in &devices {
        if driver_info.contains("libusb 正常") {
            has_correct_driver = true;
        } else {
            has_wrong_driver = true;
        }
    }

    if has_correct_driver && !has_wrong_driver {
        info!("驱动状态: libusb 正常");
    } else if has_wrong_driver {
        warn!("驱动状态异常: 设备驱动未正确安装");
        info!("请检查设备是否安装了 libusb-win32 filter 驱动");
    }
    info!("");

    // 3. 设备占用检查
    if let Some(msg) = check_device_occupation() {
        warn!("{}", msg);
        info!("");
    }

    // 4. 模式建议
    for (_vid, pid, is_brom, _) in &devices {
        if !is_brom {
            info!("设备处于 Preloader 模式 (PID={:04X})", pid);
            info!("如需 BROM 模式:");
            info!("  1. 断开 USB");
            info!("  2. 关机（按住电源键 10 秒）");
            info!("  3. 按住 音量+ + 音量- 插入 USB");
            break;
        }
    }

    info!("");
    info!("=== 诊断完成 ===");
}

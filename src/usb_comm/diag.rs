//! USB 连接诊断与状态检测

use crate::system::config::DeviceType;
use log::{info, trace, warn};

/// USB 设备状态
#[derive(Debug, Clone, PartialEq)]
pub enum USB诊断状态 {
    /// 未检测到任何 MediaTek 设备
    无设备,
    /// 检测到设备但驱动异常（如 CDC 而非 WinUSB）
    驱动错误,
    /// 检测到 Preloader 模式（PID 0x0003/0x0001）
    Preloader,
    /// 检测到 BROM 模式
    Brom,
    /// 端点通信异常（libusb 报错）
    #[allow(dead_code)]
    端点错误,
    /// 设备连接但握手失败
    #[allow(dead_code)]
    握手失败,
}

/// 扫描系统中是否有 MediaTek USB 设备
/// 返回 (vid, pid, is_brom, driver_info)
pub fn 扫描联发科设备() -> Vec<(u16, u16, bool, String)> {
    let mut 结果列表 = Vec::new();

    unsafe {
        let mut 上下文指针: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut 上下文指针) != 0 {
            warn!("[USB诊断] libusb_init 失败");
            return 结果列表;
        }

        let mut 设备列表: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let 设备数量 = libusb1_sys::libusb_get_device_list(上下文指针, &mut 设备列表);
        if 设备数量 < 0 {
            warn!("[USB诊断] get_device_list 失败: {}", 设备数量);
            libusb1_sys::libusb_exit(上下文指针);
            return 结果列表;
        }

        for i in 0..设备数量 as isize {
            let 设备 = *设备列表.wrapping_offset(i);
            let mut 描述符: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(设备, &mut 描述符) != 0 {
                continue;
            }

            let vid = 描述符.idVendor;
            let pid = 描述符.idProduct;

            // 只关心中 MediaTek 设备
            if vid != 0x0E8D {
                continue;
            }

            let 设备类型 = DeviceType::from_vid_pid(vid, pid);
            let 是BROM = 设备类型.is_brom();

            // 尝试打开设备以获取更多信息
            let mut 设备句柄: *mut libusb1_sys::libusb_device_handle = std::ptr::null_mut();
            let 驱动信息 = if libusb1_sys::libusb_open(设备, &mut 设备句柄) == 0 {
                // 尝试 claim interface 来判断驱动状态
                let claim_ret = libusb1_sys::libusb_claim_interface(设备句柄, 1);
                let 信息 = if claim_ret == 0 {
                    libusb1_sys::libusb_release_interface(设备句柄, 1);
                    "WinUSB/libusb 正常".to_string()
                } else if claim_ret == -12 {
                    // LIBUSB_ERROR_NOT_FOUND — 接口不存在
                    "接口未找到".to_string()
                } else {
                    format!("claim_interface 失败: {}", claim_ret)
                };
                libusb1_sys::libusb_close(设备句柄);
                信息
            } else {
                "设备被其他驱动占用".to_string()
            };

            结果列表.push((vid, pid, 是BROM, 驱动信息));
        }

        libusb1_sys::libusb_free_device_list(设备列表, 1);
        libusb1_sys::libusb_exit(上下文指针);
    }

    结果列表
}

/// 诊断 USB 连接状态
/// 对齐 Python Port.py handshake() + detectdevices()
pub fn 诊断连接() -> USB诊断状态 {
    let 设备列表 = 扫描联发科设备();

    if 设备列表.is_empty() {
        return USB诊断状态::无设备;
    }

    // 检查第一个设备即可
    let (vid, pid, 是BROM, 驱动信息) = &设备列表[0];
    trace!(
        "[USB诊断] VID={:04X} PID={:04X} BROM={} 驱动={}",
        vid, pid, 是BROM, 驱动信息
    );

    if 驱动信息.contains("被其他驱动占用") {
        return USB诊断状态::驱动错误;
    }

    if !是BROM {
        return USB诊断状态::Preloader;
    }

    USB诊断状态::Brom
}

/// 打印诊断提示（类似 Python 的 loop == 5 提示）
pub fn 打印连接提示() {
    info!("");
    info!("设备连接提示:");
    info!("  1. 确保设备已关机");
    info!("  2. BROM 模式: 按住 音量+ + 音量- + (或所有按键) 并插入 USB");
    info!("  3. Preloader 模式: 不要按任何键，直接插入 USB");
    info!("  4. 如果已连接但无响应，按住电源键 10 秒重置");
    info!("  5. 运行 'check-driver' 确认 WinUSB 驱动已安装");
    info!("");
}

/// 检测 libusb 错误类型并给出诊断信息
#[allow(dead_code)]
pub fn 分类libusb错误(err: i32) -> &'static str {
    match err {
        -1 => "LIBUSB_ERROR_IO (I/O 错误)",
        -2 => "LIBUSB_ERROR_INVALID_PARAM (参数错误)",
        -3 => "LIBUSB_ERROR_ACCESS (访问被拒绝 — 检查驱动)",
        -4 => "LIBUSB_ERROR_NO_DEVICE (设备已断开)",
        -5 => "LIBUSB_ERROR_NOT_FOUND (资源未找到)",
        -6 => "LIBUSB_ERROR_BUSY (设备繁忙 — 可能被其他程序占用)",
        -7 => "LIBUSB_ERROR_TIMEOUT (超时 — 设备未响应)",
        -9 => "LIBUSB_ERROR_PIPE (端点 halt/stall — 需要清除)",
        -10 => "LIBUSB_ERROR_INTERRUPTED (操作中断)",
        -11 => "LIBUSB_ERROR_NO_MEM (内存不足)",
        -12 => "LIBUSB_ERROR_NOT_SUPPORTED (不支持的操作)",
        -99 => "LIBUSB_ERROR_OTHER (其他错误)",
        _ => "未知 libusb 错误",
    }
}

/// 尝试恢复 USB 端点状态（清除 halt）
#[allow(dead_code)]
pub fn 尝试恢复端点(
    设备句柄: *mut libusb1_sys::libusb_device_handle,
    输入端点: u8,
    输出端点: u8,
) {
    unsafe {
        trace!("[USB恢复] 尝试清除端点 halt...");
        let ret_in = libusb1_sys::libusb_clear_halt(设备句柄, 输入端点);
        let ret_out = libusb1_sys::libusb_clear_halt(设备句柄, 输出端点);
        if ret_in == 0 && ret_out == 0 {
            trace!("[USB恢复] 端点恢复成功");
        } else {
            warn!("[USB恢复] 恢复失败: IN={}, OUT={}", ret_in, ret_out);
        }
    }
}

/// 枚举 USB 设备，输出详细信息（用于 list-usb 命令）
pub fn 枚举USB设备() {
    info!("扫描 USB 设备...");

    unsafe {
        let mut 上下文指针: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        if libusb1_sys::libusb_init(&mut 上下文指针) != 0 {
            info!("libusb 初始化失败");
            return;
        }

        let mut 设备列表: *const *mut libusb1_sys::libusb_device = std::ptr::null_mut();
        let 设备数量 = libusb1_sys::libusb_get_device_list(上下文指针, &mut 设备列表);
        if 设备数量 < 0 {
            info!("获取设备列表失败: {}", 设备数量);
            libusb1_sys::libusb_exit(上下文指针);
            return;
        }

        info!("找到 {} 个 USB 设备", 设备数量);

        for i in 0..设备数量 as isize {
            let 设备 = *设备列表.wrapping_offset(i);
            let mut 描述符: libusb1_sys::libusb_device_descriptor = std::mem::zeroed();
            if libusb1_sys::libusb_get_device_descriptor(设备, &mut 描述符) != 0 {
                continue;
            }

            let vid = 描述符.idVendor;
            let pid = 描述符.idProduct;

            // 显示所有设备，但高亮 MediaTek
            let 是联发科 = vid == 0x0E8D;
            let 设备类型 = DeviceType::from_vid_pid(vid, pid);

            let 模式 = if 是联发科 {
                if 设备类型.is_brom() {
                    "BROM"
                } else if 设备类型.is_preloader() {
                    "Preloader"
                } else {
                    "未知模式"
                }
            } else {
                ""
            };

            if 是联发科 {
                info!(
                    "  [MediaTek] Bus {} Device {}: VID={:04X} PID={:04X} 模式={}",
                    libusb1_sys::libusb_get_bus_number(设备),
                    libusb1_sys::libusb_get_device_address(设备),
                    vid,
                    pid,
                    模式
                );
            } else {
                trace!(
                    "  Bus {} Device {}: VID={:04X} PID={:04X}",
                    libusb1_sys::libusb_get_bus_number(设备),
                    libusb1_sys::libusb_get_device_address(设备),
                    vid,
                    pid
                );
            }
        }

        libusb1_sys::libusb_free_device_list(设备列表, 1);
        libusb1_sys::libusb_exit(上下文指针);
    }
}

/// 检测是否有其他程序占用了 MediaTek 设备
pub fn 检查设备占用() -> Option<String> {
    let 设备列表 = 扫描联发科设备();

    for (_vid, _pid, _是BROM, 驱动信息) in &设备列表 {
        if 驱动信息.contains("被其他驱动占用") || 驱动信息.contains("busy") {
            return Some(
                "设备可能被其他程序占用。请关闭 SP Flash Tool、MCT 或其他 MTK 工具后重试。"
                    .to_string(),
            );
        }
    }

    None
}

/// 综合诊断报告（用于 diagnose 命令）
pub fn 诊断并报告() {
    info!("=== USB 连接诊断 ===");
    info!("");

    // 1. 扫描 MediaTek 设备
    let 设备列表 = 扫描联发科设备();

    if 设备列表.is_empty() {
        warn!("未检测到任何 MediaTek USB 设备");
        info!("");
        info!("请检查:");
        info!("  1. USB 线缆是否连接良好");
        info!("  2. 设备是否已关机");
        info!("  3. 是否已进入 BROM 模式 (按住 音量+ + 音量- 插入 USB)");
        info!("  4. Windows 设备管理器中是否显示 MediaTek USB 设备");
        return;
    }

    info!("检测到 {} 个 MediaTek 设备:", 设备列表.len());
    for (vid, pid, 是BROM, 驱动信息) in &设备列表 {
        let 模式 = if *是BROM { "BROM" } else { "Preloader" };
        info!(
            "  VID={:04X} PID={:04X} 模式={} 驱动={}",
            vid, pid, 模式, 驱动信息
        );
    }
    info!("");

    // 2. 驱动状态检查
    let mut 有错误驱动 = false;
    let mut 有正确驱动 = false;
    for (_vid, _pid, _是BROM, 驱动信息) in &设备列表 {
        if 驱动信息.contains("WinUSB/libusb 正常") {
            有正确驱动 = true;
        } else {
            有错误驱动 = true;
        }
    }

    if 有正确驱动 && !有错误驱动 {
        info!("驱动状态: WinUSB/libusb 正常");
    } else if 有错误驱动 {
        warn!("驱动状态异常: 设备未安装 WinUSB 驱动");
        info!("请运行以下命令安装驱动: mtkclient install-drivers");
        info!("或者使用 Zadig 工具手动安装 WinUSB 驱动");
    }
    info!("");

    // 3. 设备占用检查
    if let Some(msg) = 检查设备占用() {
        warn!("{}", msg);
        info!("");
    }

    // 4. 模式建议
    for (_vid, pid, 是BROM, _) in &设备列表 {
        if !是BROM {
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

//! USB 连接诊断与状态检测
//!
//! 使用 nusb 设备枚举替代 libusb unsafe 代码

use crate::system::config::DeviceType;
use log::{info, trace, warn};
use nusb::MaybeFuture;

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
    /// 端点通信异常
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

    let devices = match nusb::list_devices().wait() {
        Ok(d) => d,
        Err(e) => {
            warn!("[USB诊断] 枚举设备失败: {:?}", e);
            return 结果列表;
        }
    };

    for dev in devices {
        let vid = dev.vendor_id();
        let pid = dev.product_id();

        if vid != 0x0E8D {
            continue;
        }

        let 设备类型 = DeviceType::from_vid_pid(vid, pid);
        let 是BROM = 设备类型.is_brom();

        // 尝试打开设备以判断驱动状态
        let 驱动信息 = match dev.open().wait() {
            Ok(device) => {
                let 信息 = match device.claim_interface(1).wait() {
                    Ok(_) => "WinUSB 正常".to_string(),
                    Err(e) => {
                        let err_str = format!("{:?}", e);
                        if err_str.contains("NotFound") || err_str.contains("not_found") {
                            "接口未找到".to_string()
                        } else {
                            format!("claim_interface 失败: {}", err_str)
                        }
                    }
                };
                // nusb: drop device 会自动 release 和 close
                信息
            }
            Err(_) => "设备被其他驱动占用".to_string(),
        };

        结果列表.push((vid, pid, 是BROM, 驱动信息));
    }

    结果列表
}

/// 诊断 USB 连接状态
pub fn 诊断连接() -> USB诊断状态 {
    let 设备列表 = 扫描联发科设备();

    if 设备列表.is_empty() {
        return USB诊断状态::无设备;
    }

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

/// 打印诊断提示
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

/// 检测 nusb 错误类型并给出诊断信息
#[allow(dead_code)]
pub fn 分类libusb错误(err: &str) -> &'static str {
    if err.contains("Timeout") || err.contains("timeout") {
        "超时 (设备未响应)"
    } else if err.contains("Pipe") || err.contains("pipe") {
        "端点 halt/stall — 需要清除"
    } else if err.contains("NotFound") || err.contains("not_found") {
        "设备未找到"
    } else if err.contains("Busy") || err.contains("busy") {
        "设备繁忙 — 可能被其他程序占用"
    } else if err.contains("Access") || err.contains("access") {
        "访问被拒绝 — 检查驱动"
    } else {
        "未知错误"
    }
}

/// 枚举 USB 设备，输出详细信息（用于 list-usb 命令）
pub fn 枚举USB设备() {
    info!("扫描 USB 设备...");

    let devices: Vec<_> = match nusb::list_devices().wait() {
        Ok(d) => d.collect(),
        Err(_) => {
            info!("枚举失败");
            return;
        }
    };

    info!("找到 {} 个 USB 设备", devices.len());

    for dev in devices {
        let vid = dev.vendor_id();
        let pid = dev.product_id();
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
                "  [MediaTek] VID={:04X} PID={:04X} 模式={}",
                vid, pid, 模式
            );
        } else {
            trace!("  VID={:04X} PID={:04X}", vid, pid);
        }
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

    let mut 有错误驱动 = false;
    let mut 有正确驱动 = false;
    for (_vid, _pid, _是BROM, 驱动信息) in &设备列表 {
        if 驱动信息.contains("WinUSB") {
            有正确驱动 = true;
        } else {
            有错误驱动 = true;
        }
    }

    if 有正确驱动 && !有错误驱动 {
        info!("驱动状态: WinUSB 正常");
    } else if 有错误驱动 {
        warn!("驱动状态异常: 设备未安装 WinUSB 驱动");
        info!("请运行以下命令安装驱动: mtkclient install-drivers");
        info!("或者使用 Zadig 工具手动安装 WinUSB 驱动");
    }
    info!("");

    if let Some(msg) = 检查设备占用() {
        warn!("{}", msg);
        info!("");
    }

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

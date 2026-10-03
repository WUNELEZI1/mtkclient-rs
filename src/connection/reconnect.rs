use crate::color::Colorize;
use crate::preloader::Preloader;
use crate::usb;
use crate::usb::{UsbContext, UsbStage};
use log::{debug, info, trace, warn};
use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::manager::{ConnectionManager, DeviceMode};

use crate::preloader::SerialPortTransport;

impl ConnectionManager {
    /// WinUSB 重连循环
    /// 无限等待直到设备出现（用户手动按组合键进入 BROM 模式可能需要几十秒到几分钟）
    pub fn reconnect_loop(
        &self,
        context: &UsbContext,
        target_stage: UsbStage,
    ) -> Result<usb::UsbDevice, String> {
        let mut retry = 0;
        let mut context_refreshed = false;
        // 总等待上限：设备重枚举通常 < 2s，超时说明设备未回到目标阶段，避免无限卡死。
        const RECONNECT_TOTAL_WAIT_SECS: u64 = 60;
        let start = Instant::now();

        info!(
            "[RECONNECT] scanning for stage={:?} (最多等待 {}s)...",
            target_stage, RECONNECT_TOTAL_WAIT_SECS
        );

        let pids = match target_stage {
            UsbStage::Brom => vec![0x0003u16],
            UsbStage::Preloader => vec![0x2000u16],
            UsbStage::Da => vec![0x2001u16],
            UsbStage::Unknown => vec![0x0003u16, 0x2000u16, 0x2001u16],
        };

        loop {
            retry += 1;

            // Ctrl+C 检查：用户取消时退出等待
            if crate::cancel::requested() {
                return Err("用户取消等待".to_string());
            }

            // 总等待超时：避免设备未重枚举到目标阶段时无限卡在端口检测
            if start.elapsed().as_secs() >= RECONNECT_TOTAL_WAIT_SECS {
                return Err(format!(
                    "[RECONNECT] 等待设备重连超时 ({}s)：未检测到 {:?}。请确认设备已回到目标模式后重试。",
                    RECONNECT_TOTAL_WAIT_SECS, target_stage
                ));
            }

            for &pid in &pids {
                match usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, pid) {
                    Ok(device) => {
                        info!(
                            "[RECONNECT] success on attempt {} (stage={:?}, PID=0x{:04X})",
                            retry, device.stage, device.pid
                        );
                        return Ok(device);
                    }
                    Err(e) => {
                        trace!(
                            "[RECONNECT] open_by_vid_pid failed for PID=0x{:04X}: {}（诊断: {}）",
                            pid,
                            e,
                            crate::usb::diag::classify_libusb_error(&e)
                        );
                    }
                }
            }

            // 驱动切换后旧的 libusb context 可能无法枚举新设备
            // 每 50 次重试（约 10 秒）尝试用新 context 打开
            if retry % 50 == 0 && !context_refreshed {
                trace!("[RECONNECT] 尝试刷新 libusb context...");
                if let Ok(new_ctx) = UsbContext::new() {
                    for &pid in &pids {
                        if let Ok(device) = usb::UsbDevice::open_by_vid_pid(&new_ctx, 0x0E8D, pid) {
                            info!("[RECONNECT] 使用新 context 成功连接 (attempt {})", retry);
                            return Ok(device);
                        }
                    }
                }
                context_refreshed = true;
            }

            if retry % super::manager::RECONNECT_LOG_INTERVAL == 1 {
                trace!(
                    "[RECONNECT] retry {} (scanning {} PIDs)...",
                    retry,
                    pids.len()
                );
            }
            std::thread::sleep(Duration::from_millis(super::manager::RECONNECT_INTERVAL_MS));
        }
    }

    // 说明：原 reconnect_after_da / reconnect_after_kamakiri / reconnect_after_usb_reset /
    // try_quick_connect / mode / stage / port_name 等接口为历史预留（DA 会话复用已全面
    // 禁用，见 main.rs / session.rs 注释），全平台无真实调用点，已按“显式废弃的死代码
    // 直接删除”原则移除；其依赖的 USB_REENUM_DELAY_MS / QUICK_CONNECT_INTERVAL_MS 常量
    // 亦一并从 manager.rs 清理。

    /// Preloader 模式初始化：等待 Preloader VCOM (PID=0x2000)，握手后直接返回
    /// allow_brom_fallback: 当找不到 Preloader 时是否尝试 fallback 到 BROM（--mode auto 用 true，--mode preloader 用 false）
    ///
    /// 注：DA 会话复用已全面禁用（见 main.rs / session.rs device_online），不再有 allow_da_fallback 分支。
    pub(crate) fn smart_init_preloader(
        &mut self,
        context: &UsbContext,
        allow_brom_fallback: bool,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("等待 Preloader VCOM 设备连接 (PID=0x2000)，无需按任何按键...");

        // 0. Preloader 模式的 DA 会话复用已禁用（不再尝试）。
        //
        // 根因：Preloader 串口（VCOM, PID=0x2000）在进程退出时因 DTR 掉线会导致设备
        // 复位，并重新枚举为同一 PID=0x2000 的 Preloader 握手态——此时 DA 早已死掉，
        // 但 PID 与“DA 模式串口”完全一样，无法仅凭 PID 区分。旧逻辑据此盲目复用
        // .state(da_loaded) + PID=0x2000，向一个处于 Preloader 握手态（无 DA）的设备
        // 直接发 DA SHUTDOWN，被误读后 status 巧合返回 0 → 假成功 → 设备毫无反应
        // （典型表现：reboot 日志显示“DA SHUTDOWN 成功”但设备不重启）。
        //
        // 由于 Preloader 串口的 DA 不跨进程存活，会话复用在此模式下本质上永远无效，
        // 故统一改为“每次都重新握手 + 重载 DA”，保证发往的是活着的、已验证的 DA 链路
        // （与你 run 3 的 printgpt 走的是同一条已被验证可用的新鲜路径）。
        // 注：BROM 模式的 DA 同样不跨进程存活（DA 加载后 PID 仍是 0x0003，设备复位后亦回
        // 0x0003，DA 已死），故 BROM 亦不做 .state DA 会话复用——统一由 smart_init(Brom)
        // 重新握手 + 重载 DA。此处仅说明历史设计，复用逻辑已全部禁用。

        let mut retry_count = 0u32;
        let mut reported_ports = HashSet::new();
        const PRELOADER_MAX_RETRY: u32 = 50; // 约 10 秒 (50 * 200ms)
        // 等待策略（对齐 brom 路径 manager.rs 主循环，仅 Ctrl+C 退出）：
        // ① 未检测到 Preloader 端口时【无限等待】——用户手动进入 Preloader 握手态可能需要
        //    若干秒到几分钟，不应有 30s 上限（修复 bug #1：等待应无限）。
        // ② 端口已出现但握手【持续失败】（设备处于 DA 模式而非 Preloader 握手态）时，无限重试
        //    无益，超出窗口后给出明确指引（修复 bug #3：避免"发现端口后卡死"）。
        const PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS: u64 = 25;
        let mut handshake_fail_start: Option<Instant> = None;

        loop {
            retry_count += 1;

            // Ctrl+C 检查：用户取消时退出等待
            if crate::cancel::requested() {
                return Err("用户取消等待".to_string());
            }
            // 注意：Preloader 模式不再设总等待上限（对齐 brom 路径），未检测到端口时无限等待。

            // 1. 枚举 COM 口，找 PID=0x2000 的 Preloader VCOM
            if let Ok(ports) = serialport::available_ports() {
                for p in &ports {
                    if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                        && info.vid == 0x0E8D
                        && info.pid == 0x2000
                    {
                        if reported_ports.insert(p.port_name.clone()) {
                            info!(
                                "[PRELOADER] 发现 Preloader COM 口: {} (VID={:04X} PID={:04X})",
                                p.port_name, info.vid, info.pid
                            );
                        } else {
                            trace!(
                                "[PRELOADER] 发现 Preloader COM 口: {} (VID={:04X} PID={:04X})",
                                p.port_name, info.vid, info.pid
                            );
                        }
                        match self.preloader_serial_handshake(&p.port_name) {
                            Ok(preloader) => {
                                self.mode = DeviceMode::Preloader;
                                self.stage = UsbStage::Preloader;
                                self.port_name = Some(p.port_name.clone());
                                return Ok((preloader, DeviceMode::Preloader));
                            }
                            Err(e) => {
                                warn!("[PRELOADER] {} 握手失败: {}", p.port_name, e);
                                // 端口已出现但握手反复失败：设备很可能处于 DA 模式（非 Preloader
                                // 握手态），继续无限重试不会自动恢复，超出窗口后给出明确指引
                                //（修复 bug #3：避免"发现端口后卡死"）。
                                match handshake_fail_start {
                                    None => handshake_fail_start = Some(Instant::now()),
                                    Some(t)
                                        if t.elapsed().as_secs()
                                            >= PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS =>
                                    {
                                        return Err(format!(
                                            "已检测到 Preloader 端口 {} 但握手持续失败 ({}s)：设备可能处于 DA 模式而非 Preloader 握手态，或串口被其他程序占用。请断开重连设备（使其回到 Preloader 握手态），或改用 --mode brom。",
                                            p.port_name, PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS
                                        ));
                                    }
                                    Some(_) => {}
                                }
                            }
                        }
                    }
                }
            }

            // 2. 串口没找到，尝试 WinUSB（PID=0x2000）
            if let Ok(usb_device) = usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x2000) {
                info!(
                    "[PRELOADER] WinUSB 设备已连接: VID={:04X} PID={:04X}",
                    usb_device.vid, usb_device.pid
                );
                let mut preloader = Preloader::new(Box::new(usb_device));
                match preloader.init_preloader() {
                    Ok(true) => {
                        self.mode = DeviceMode::Preloader;
                        self.stage = UsbStage::Preloader;
                        return Ok((preloader, DeviceMode::Preloader));
                    }
                    Ok(false) => {
                        warn!("[PRELOADER] init 未成功，继续等待...");
                    }
                    Err(e) => {
                        warn!("[PRELOADER] init 失败: {}，继续等待...", e);
                        // 端口已出现但 init 反复失败：同串口握手失败，超出窗口后给出明确指引。
                        match handshake_fail_start {
                            None => handshake_fail_start = Some(Instant::now()),
                            Some(t)
                                if t.elapsed().as_secs()
                                    >= PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS =>
                            {
                                return Err(format!(
                                    "已检测到 Preloader WinUSB 设备 (PID=0x2000) 但 init 持续失败 ({}s)：设备可能处于 DA 模式而非 Preloader 握手态。请断开重连设备，或改用 --mode brom。",
                                    PRELOADER_HANDSHAKE_FAIL_WINDOW_SECS
                                ));
                            }
                            Some(_) => {}
                        }
                    }
                }
            }

            // 3. 尝试一定次数后仍未找到 Preloader 设备
            if retry_count >= PRELOADER_MAX_RETRY {
                if allow_brom_fallback {
                    trace!(
                        "[PRELOADER] 等待 {} 次 (约 {} 秒) 未找到 Preloader 设备，尝试检测 BROM 设备...",
                        retry_count,
                        (retry_count * 200) / 1000
                    );

                    // 检测 BROM COM 口
                    if let Ok(ports) = serialport::available_ports() {
                        for p in &ports {
                            if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                                && info.vid == 0x0E8D
                                && info.pid == 0x0003
                            {
                                warn!(
                                    "[PRELOADER] 检测到 BROM COM 口: {}，自动切换到 BROM 模式",
                                    p.port_name
                                );
                                return self
                                    .smart_init(context, crate::system::config::WorkMode::Brom);
                            }
                        }
                    }

                    // 检测 BROM WinUSB 设备
                    if let Ok(_usb_device) =
                        usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x0003)
                    {
                        warn!(
                            "[PRELOADER] 检测到 BROM WinUSB 设备 (PID=0x0003)，自动切换到 BROM 模式"
                        );
                        return self.smart_init(context, crate::system::config::WorkMode::Brom);
                    }

                    trace!("[PRELOADER] 未检测到 BROM 设备，继续等待 Preloader 设备...");
                } else {
                    trace!(
                        "[PRELOADER] 等待 {} 次 (约 {} 秒) 未找到 Preloader 设备，继续等待...",
                        retry_count,
                        (retry_count * 200) / 1000
                    );
                }

                // 重置计数器继续等待（给用户更多时间）
                retry_count = 0;
            }

            std::thread::sleep(Duration::from_millis(super::manager::RECONNECT_INTERVAL_MS));
        }
    }

    /// Preloader 串口握手：打开 COM 口 → init_preloader
    pub(crate) fn preloader_serial_handshake(&self, port_name: &str) -> Result<Preloader, String> {
        // 必须用 open_raw 而非 new：new 会先 verify_port_exists（打开→关闭 COM 口），
        // 打开串口会清空接收缓冲区中的 Preloader "READY" 同步信号；Preloader 设备在
        // 进入握手态时持续发送 READY（刷机匣成功日志 0x52/45/41/44/59 = "READY"），
        // verify 的打开-关闭会消耗掉这些字节，导致后续握手"未收到任何响应字节"。
        // 对齐 preloader_boot_mode.rs 的成功做法（open_raw 不清缓冲）。
        let transport = SerialPortTransport::open_raw(port_name, 115200)?;
        let mut preloader = Preloader::new(Box::new(transport));
        if !preloader
            .init_preloader()
            .map_err(|e| format!("串口 init_preloader 失败: {}", e))?
        {
            return Err("串口 Preloader 握手失败".to_string());
        }
        debug!(
            "{}",
            format!("[PRELOADER] {} 握手成功", port_name).green().bold()
        );
        Ok(preloader)
    }
}

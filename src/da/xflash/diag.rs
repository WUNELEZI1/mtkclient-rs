//! DA 设备诊断与状态查询
//!
//! 包含 DAXFlash 加载完成后用于设备信息查询与会话状态管理的方法：
//! - `reinit`：查询 SRAM/DRAM/EMMC 芯片信息
//! - `get_emmc_info`：查询 EMMC Boot1/Boot2 大小
//! - `save_session_state`：将 DA 会话写入 .state 文件
//! - `get_emi_data` / `get_extensions_data` / `get_last_gpt_data`：调试用 getter
//!
//! 拆分动机：这些方法都是"只读 / 元信息"操作，不参与 DA 加载主流程。

use log::{info, trace, warn};
use std::time::Duration;

use crate::da::xflash::{DAXFlash, EmmcInfo};

// =============================================================================
// 设备信息查询
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 重新初始化（DA 复用路径：只做心跳，不做任何可能改变 DA 状态的操作）
    ///
    /// DA 复用时设备已经处于完全初始化状态，reinit 的唯一目的是确认 DA 还活着。
    /// 发送 GET_CHIP_ID 作为心跳，不发送 GET_EMMC_INFO 或其他可能改变状态的命令。
    /// USB 高速重连已完全移除（只在 upload_da2 初始化时执行一次）。
    pub(crate) fn reinit(&mut self) -> Result<(), String> {
        // 心跳：GET_CHIP_ID（确认 DA 存活）
        match self.send_devctrl(0x010106, None) {
            Ok(data) if data.len() >= 10 => {
                let hw_code = u16::from_le_bytes([data[0], data[1]]);
                info!("  芯片 HW Code: 0x{:04X}", hw_code);
            }
            Ok(_) => trace!("[reinit] GET_CHIP_ID 返回空数据"),
            Err(e) => trace!("[reinit] GET_CHIP_ID 失败 (可能不支持): {}", e),
        }

        Ok(())
    }

    /// USB 高速重连：检测当前速度，如果是 full-speed 则切换并重连
    /// 串口模式下自动跳过（无 USB 速度概念）。
    pub(crate) fn try_usb_high_speed_reconnect(&mut self) {
        if !self.preloader.device.is_libusb() {
            trace!("[RECONNECT] 串口模式，跳过 USB 高速重连");
            return;
        }

        let speed = match self.get_usb_speed() {
            Ok(s) => s,
            Err(e) => {
                trace!("[RECONNECT] get_usb_speed 失败: {}，跳过重连", e);
                return;
            }
        };

        if speed != "full-speed" {
            info!("[RECONNECT] 当前 USB 速度: {}，无需重连", speed);
            return;
        }

        info!("[RECONNECT] 检测到 full-speed，执行高速重连...");

        // 1. 命令设备切换到高速
        if let Err(e) = self.set_usb_speed() {
            warn!("[RECONNECT] set_usb_speed 失败: {}，跳过重连", e);
            return;
        }

        // 2. USB 总线复位（触发设备以新速度重新枚举）
        if let Err(e) = self.preloader.device.reset_device() {
            warn!("[RECONNECT] USB reset 失败: {}，跳过重连", e);
            return;
        }

        // 3. 释放旧 USB 句柄
        if let Err(e) = self.preloader.device.close_device() {
            warn!("[RECONNECT] 关闭旧 USB 句柄失败: {}，跳过重连", e);
            return;
        }
        info!("[RECONNECT] 已关闭旧 USB 句柄，等待设备重新枚举...");

        // 4. 等待设备重新枚举（2 秒，对齐 Python time.sleep(2)）
        std::thread::sleep(Duration::from_secs(2));

        // 5. 重新打开 USB 设备
        let usb_context = match crate::usb::USB上下文::新建() {
            Ok(ctx) => ctx,
            Err(e) => {
                warn!("[RECONNECT] 创建 USB 上下文失败: {}，重连终止", e);
                return;
            }
        };

        // 循环尝试打开设备（对齐 Python while not connect()）
        let max_retries = 5;
        for attempt in 1..=max_retries {
            match self.preloader.device.reopen_device(&usb_context) {
                Ok(_) => {
                    info!(
                        "[RECONNECT] USB 高速重连成功 (第 {} 次尝试)，读取速度将显著提升",
                        attempt
                    );
                    return;
                }
                Err(e) => {
                    trace!(
                        "[RECONNECT] 第 {}/{} 次尝试失败: {}",
                        attempt, max_retries, e
                    );
                    if attempt < max_retries {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }

        warn!(
            "[RECONNECT] USB 重连失败（{} 次尝试），保持当前连接",
            max_retries
        );
    }

    /// 获取 EMMC 完整信息（Boot1/Boot2/RPMB/User Size/Block Size/CID）
    /// 对齐 Python: send_devctrl(0x01010C, None) → parse_emmc_info
    ///
    /// 响应数据布局（XFlash GET_EMMC_INFO 0x01010C，80 字节）：
    ///   [0..4]   boot1_size          (u32 LE, 字节)
    ///   [4..8]   boot2_size          (u32 LE, 字节)
    ///   [8..12]  rpmb_size           (u32 LE, 字节)
    ///   [12..16] emmc_type           (u32 LE: 0=unknown, 1=EMMC, 2=UFS, 3=SD ...)
    ///   [16..24] user_size           (u64 LE, 字节)
    ///   [24..28] block_size          (u32 LE, 字节)
    ///   [28..44] cid                 (16 字节 ASCII/MID)
    ///   [44..80] reserved / otp
    /// 短超时 200ms，不支持时快速失败
    pub fn get_emmc_info(&mut self) -> Result<EmmcInfo, String> {
        let data = self.with_short_timeout(200, |da| da.send_devctrl(0x01010C, None))?;
        if data.len() < 24 {
            return Err(format!(
                "EMMC info 数据太短（需要至少 24 字节，实际 {} 字节）",
                data.len()
            ));
        }

        let boot1_size = u32::from_le_bytes(data[0..4].try_into().unwrap()) as u64;
        let boot2_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as u64;
        let rpmb_size = if data.len() >= 12 {
            u32::from_le_bytes(data[8..12].try_into().unwrap()) as u64
        } else {
            0
        };
        let emmc_type_code = if data.len() >= 16 {
            u32::from_le_bytes(data[12..16].try_into().unwrap())
        } else {
            0
        };
        let user_size = if data.len() >= 24 {
            u64::from_le_bytes(data[16..24].try_into().unwrap())
        } else {
            0
        };
        let block_size = if data.len() >= 28 {
            u32::from_le_bytes(data[24..28].try_into().unwrap())
        } else {
            512
        };
        // CID 段在 28..44 (16 字节) — 厂商信息 ASCII
        let cid = if data.len() >= 44 {
            data[28..44].to_vec()
        } else {
            Vec::new()
        };

        // 类型代码 → 字符串描述
        let emmc_type = match emmc_type_code {
            0 => "Unknown".to_string(),
            1 => "EMMC".to_string(),
            2 => "UFS".to_string(),
            3 => "SD".to_string(),
            4 => "NAND".to_string(),
            5 => "NAND_PARALLEL".to_string(),
            6 => "UFS_CARD".to_string(),
            n => format!("Type#{}", n),
        };

        Ok(EmmcInfo {
            boot1_size,
            boot2_size,
            rpmb_size,
            user_size,
            block_size,
            emmc_type,
            cid,
        })
    }

    /// 获取 EMMC 简化信息（仅 Boot1/Boot2 大小，兼容老设备响应）
    /// 短超时 200ms，不支持时快速失败
    pub fn get_emmc_info_simple(&mut self) -> Result<EmmcInfo, String> {
        let data = self.with_short_timeout(200, |da| da.send_devctrl(0x01010C, None))?;
        if data.len() < 8 {
            return Err("EMMC info 数据太短".to_string());
        }
        let boot1_size = u32::from_le_bytes(data[0..4].try_into().unwrap()) as u64;
        let boot2_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as u64;
        Ok(EmmcInfo {
            boot1_size,
            boot2_size,
            rpmb_size: 0,
            user_size: 0,
            block_size: 512,
            emmc_type: "EMMC (简化)".to_string(),
            cid: Vec::new(),
        })
    }
}

// =============================================================================
// 会话状态保存
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 保存 DA 会话状态到 .state 文件
    ///
    /// 写入内容：
    /// - usb_vid / usb_pid：当前 USB 设备的 VID/PID
    /// - hw_code：当前芯片的 HW code
    /// - target_config：当前设备的安全配置
    /// - da_loaded=true：标记 DA 已加载
    ///
    /// 如果获取 VID/PID 或 HW code 失败，会静默忽略（不影响 DA 加载成功的结果）
    pub(crate) fn save_session_state(&mut self) {
        let (vid, pid) = match (
            self.preloader.device.get_vid(),
            self.preloader.device.get_pid(),
        ) {
            (Some(v), Some(p)) => (v, p),
            _ => {
                // 串口模式：get_vid/pid 返回 None，使用 MTK 默认 VID/PID
                trace!("save_session_state: 串口模式，使用默认 VID/PID");
                (0x0E8D, 0x2000)
            }
        };

        // 获取 HW code（从 chip 配置中读取，避免再次发送 USB 命令）
        let hw_code_result = self
            .preloader
            .chip
            .map(|c| c.hw_code)
            .ok_or_else(|| "chip not set".to_string());

        // 获取 target_config（直接读取本地状态，不发送 USB 命令）
        // 实际实现中 target_config 在 init() 时已读入 chip 配置
        let target_config = 0u32; // 占位 — session.rs 的 target_config 字段当前不参与复用判断

        if let Ok(hw_code) = hw_code_result {
            let preloader_path = self.preloader_path.as_deref();
            let init_mode = if self.preloader.is_preloader_mode {
                crate::connection::session::InitMode::Preloader
            } else {
                crate::connection::session::InitMode::Brom
            };
            crate::connection::save_da_session(
                vid,
                pid,
                hw_code,
                target_config,
                preloader_path,
                init_mode,
            );
            for name in &self.optional_query_failures {
                crate::connection::session::mark_optional_query_failed(name);
            }
        } else {
            warn!("save_session_state: chip 未初始化，跳过 .state 保存");
        }
    }
}

// =============================================================================
// DA 心跳检测 / 会话保持
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// DA 心跳检测：发送轻量级命令检测 DA 是否仍然在线
    /// 使用 1 秒短超时，避免默认 5 秒超时导致用户等待过久
    /// 返回 true 表示 DA 存活，false 表示 DA 已断开或设备已重启
    pub fn da_heartbeat(&mut self) -> bool {
        // 临时缩短超时到 1 秒，加快心跳检测速度
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader
            .device
            .set_timeout(Duration::from_millis(1000));

        // 使用 GET_CHIP_ID (0x010106) 作为心跳命令，数据量小且安全
        let result = match self.send_devctrl(0x010106, None) {
            Ok(data) if data.len() >= 2 => {
                trace!("[HEARTBEAT] DA 存活，响应 {} 字节", data.len());
                true
            }
            Ok(_) => {
                trace!("[HEARTBEAT] DA 响应空数据，视为存活");
                true
            }
            Err(e) => {
                trace!("[HEARTBEAT] DA 无响应: {}", e);
                false
            }
        };

        self.preloader.device.set_timeout(orig_timeout);
        result
    }

    /// 检查 DA 会话是否有效，如果无效则重置会话状态
    /// 注意：如果用户按了 Ctrl+C，不视为 DA 失效，避免误清 .state
    pub fn check_da_session(&mut self) -> bool {
        // 用户主动取消时不做心跳检测，避免 send_devctrl 被中断后误判为 DA 失效
        if crate::cancel::requested() || crate::cancel::force_requested() {
            trace!("[DA_SESSION] 用户取消中，跳过心跳检测");
            return true;
        }

        if self.da_heartbeat() {
            true
        } else {
            warn!("[DA_SESSION] DA 会话已失效，重置会话状态");
            crate::connection::reset_session();
            false
        }
    }

    /// DA 会话恢复：关闭死连接 → 重新打开 → sync
    /// 用于 Ctrl+C 中断后设备未重启的场景（DA 仍在运行，只是管道断了）
    /// 支持 USB 和串口两种传输层。
    pub(crate) fn reconnect_usb(
        &mut self, context: &crate::usb::USB上下文
    ) -> Result<(), String> {
        if !self.preloader.device.is_libusb() {
            // 串口模式：关闭并重新打开串口
            info!("[RECONNECT_USB] 串口模式，关闭并重新打开串口...");
            self.preloader.device.close_device()?;
            std::thread::sleep(Duration::from_millis(500));

            // fallback: 扫描 Preloader COM 口
            let port_name =
                crate::preloader::SerialPortTransport::find_brom_port_with_timeout(3000)
                    .and_then(|result| match result {
                        crate::preloader::BromPortResult::SerialPort(p) => Some(p),
                        _ => None,
                    })
                    .ok_or("无法找到 Preloader 串口端口")?;

            info!("[RECONNECT_USB] 重新打开串口 {}...", port_name);
            let transport = crate::preloader::SerialPortTransport::new(&port_name, 115200)
                .map_err(|e| format!("重新打开串口失败: {}", e))?;
            self.preloader.device = Box::new(transport);

            // 串口重连后同样需要排空残留 + 协议同步 + 确认 DA 存活
            self.preloader.device.drain_pending();
            if let Err(e) = self.xflash_sync() {
                warn!("[RECONNECT_USB] 串口 sync 失败: {}，DA 可能已掉线", e);
                return Err(format!("串口重连后 sync 失败: {}", e));
            }
            std::thread::sleep(Duration::from_millis(50));
            if !self.da_heartbeat() {
                return Err("串口重连后 DA 心跳失败，可能需要重新加载 DA".to_string());
            }

            info!("[RECONNECT_USB] 串口重新连接成功（协议同步完成）");
            return Ok(());
        }

        info!("[RECONNECT_USB] 关闭死 USB 连接...");
        self.preloader.device.close_device()?;

        info!("[RECONNECT_USB] 等待设备重新枚举...");
        std::thread::sleep(Duration::from_millis(500));

        // 重新打开 USB 设备（使用同一 VID/PID）
        info!("[RECONNECT_USB] 重新打开 USB 设备...");
        let max_retries = 5;
        for attempt in 1..=max_retries {
            match self.preloader.device.reopen_device(context) {
                Ok(_) => {
                    info!("[RECONNECT_USB] 重新连接成功 (第 {} 次尝试)", attempt);

                    // 重连后：清空 USB 管道残留 → 协议同步 → 确认 DA 存活
                    self.preloader.device.drain_pending();
                    if let Err(e) = self.xflash_sync() {
                        warn!("[RECONNECT_USB] sync 失败: {}，DA 可能已掉线", e);
                        return Err(format!("重连后 sync 失败: {}", e));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                    if !self.da_heartbeat() {
                        return Err("重连后 DA 心跳失败，可能需要重新加载 DA".to_string());
                    }
                    info!("[RECONNECT_USB] 重连后协议同步完成，DA 存活");

                    return Ok(());
                }
                Err(e) => {
                    trace!(
                        "[RECONNECT_USB] 第 {}/{} 次尝试失败: {}",
                        attempt, max_retries, e
                    );
                    if attempt < max_retries {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }

        Err(format!("USB 重新连接失败（5 次尝试）"))
    }
}

// =============================================================================
// 调试 / Getter 方法
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 获取 EMI 数据（调试模式使用）
    pub fn get_emi_data(&self) -> Option<&Vec<u8>> {
        self.emi.as_ref()
    }

    /// 获取 DA extensions 数据（调试模式使用）
    pub fn get_extensions_data(&self) -> Option<Vec<u8>> {
        if self.da2_data.is_empty() {
            None
        } else {
            self.generate_da_extensions()
        }
    }

    /// 获取最后一次 GPT 读取的原始数据（调试模式使用）
    pub fn get_last_gpt_data(&self) -> Result<&Vec<u8>, String> {
        self.last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据".to_string())
    }
}

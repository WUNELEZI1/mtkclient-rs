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

use crate::da::xflash::protocol::GET_DA_VER_CMD;
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
        match self.send_devctrl(GET_DA_VER_CMD, None) {
            Ok(data) if data.len() >= 10 => {
                let hw_code = u16::from_le_bytes([data[0], data[1]]);
                info!("  芯片 HW Code: 0x{:04X}", hw_code);
            }
            Ok(_) => trace!("[reinit] GET_DA_VER_CMD 返回空数据"),
            Err(e) => trace!("[reinit] GET_DA_VER_CMD 失败 (可能不支持): {}", e),
        }

        Ok(())
    }

    /// USB 高速重连：检测当前速度，如果是 full-speed 则切换并重连
    /// 串口模式下自动跳过（无 USB 速度概念）。
    /// 老平台 DA（MT6771/0x788、MT6768/0x707、MT6765/0x676 等）在 full-speed 下
    /// 切换 USB 速度会 stall 端点，而 WinUSB/nusb 的 clear_halt 恢复不了该 stall，
    /// 表现为 setup_env 的 write 报 "os error 31" (ERROR_GEN_FAILURE)，DA 加载卡死。
    /// mtkclient Python 实现也仅在明确支持高速切换的新 DA 上调用 set_usb_speed，
    /// 老平台 DA 默认在 full-speed 直接工作，不切速度。这里据此跳过以避免 os error 31。
    fn chip_skips_usb_speed_switch(hw_code: u16) -> bool {
        matches!(
            hw_code,
            // 老平台（full-speed 工作稳定，切高速必 stall）
            0x788 | // MT6771/MT8385/MT8183/MT8666 (Helio P60/P70/G80)
            0x707 | // MT6768/MT6769
            0x676 | // MT6765/MT6762 (Helio A25/P22)
            0x762 | // MT6763 (Helio P23)
            0x0699 // MT6799 等更早平台
        )
    }

    pub(crate) fn try_usb_high_speed_reconnect(&mut self) {
        if !self.preloader.device.is_libusb() {
            trace!("[RECONNECT] 串口模式，跳过 USB 高速重连");
            return;
        }

        // 老平台 DA 跳过 USB 速度切换，避免 WinUSB clear_halt 触发 os error 31
        let hw_code = self.preloader.chip.map(|c| c.hw_code).unwrap_or(0);
        if Self::chip_skips_usb_speed_switch(hw_code) {
            trace!(
                "[RECONNECT] HW Code 0x{:04X} 为老平台，跳过 USB 速度切换（避免 os error 31）",
                hw_code
            );
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
    ///
    /// 对齐刷机匣 MtkClient 日志协议:
    ///   1. DEVICE_CTRL(0x010009) → status
    ///   2. 0x00040001 → status → xread(104 bytes) → status
    ///
    /// 返回格式（104 字节）：
    ///   [0..4]   emmc_type (u32)
    ///   [4..8]   block_size (u32)
    ///   [8..72]  8 x qword: boot1, boot2, rpmb, gp1..gp4, user
    ///   [72..88] CID (16 bytes)
    ///
    /// 短超时 200ms，不支持时快速失败
    pub fn get_emmc_info(&mut self) -> Result<EmmcInfo, String> {
        let data = self.with_short_timeout(200, |da| da.send_emmc_query(0x00040001))?;
        Self::parse_emmc_info(&data)
    }

    /// 获取 EMMC 简化信息（短超时 50ms，不支持时快速失败）
    pub fn get_emmc_info_simple(&mut self) -> Result<EmmcInfo, String> {
        let data = self.with_short_timeout(50, |da| da.send_emmc_query(0x00040001))?;
        if data.len() < 8 {
            return Err("EMMC info 数据太短".to_string());
        }
        Self::parse_emmc_info(&data)
    }

    /// 发送 EMMC 查询命令（DEVICE_CTRL 前缀 + 子命令）
    ///
    /// 对齐刷机匣: 每次 0x0004XXXX 命令前都先发 DEVICE_CTRL
    fn send_emmc_query(&mut self, cmd: u32) -> Result<Vec<u8>, String> {
        use crate::da::xflash::protocol::{CMD_MAGIC, pack3};

        // Step 1: DEVICE_CTRL 前缀
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;
        let st1 = self.status()?;
        if st1 != 0 {
            return Err(format!("EMMC query DEVICE_CTRL status: 0x{:08X}", st1));
        }

        // Step 2: 发送子命令
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&cmd.to_le_bytes())?;
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!(
                "EMMC query cmd=0x{:08X} status: 0x{:08X}",
                cmd, st2
            ));
        }

        // Step 3: 读取响应数据
        let resp = self.xread_data()?;
        let _ = self.status();
        Ok(resp)
    }

    /// 解析 EMMC info 数据（104 字节格式）
    fn parse_emmc_info(data: &[u8]) -> Result<EmmcInfo, String> {
        if data.len() < 8 {
            return Err(format!(
                "EMMC info 数据太短（需要至少 8 字节，实际 {} 字节）",
                data.len()
            ));
        }

        let emmc_type_code = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let block_size = u32::from_le_bytes(data[4..8].try_into().unwrap());

        let read_qword = |offset: usize| -> u64 {
            if data.len() >= offset + 8 {
                u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
            } else {
                0
            }
        };

        let boot1_size = read_qword(8);
        let boot2_size = read_qword(16);
        let rpmb_size = read_qword(24);
        let user_size = read_qword(64);

        let cid = if data.len() >= 88 {
            data[72..88].to_vec()
        } else {
            Vec::new()
        };

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

impl<'a> DAXFlash<'a> {}

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

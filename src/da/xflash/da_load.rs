//! DA 加载主流程
//!
//! 实现 DAXFlash::upload_da() 完整流程，对齐 Python mtkclient xflash_lib.py:
//!   upload_da1 → get_expire_date → set_reset_key → set_checksum_level
//!     → get_connection_agent → send_emi → upload_da2 → get_sla_status
//!     → reinit → drain USB buffer → boot_to(0x4FFF0000, daextdata)
//!     → CUSTOM_ACK → CUSTOM_SET_STORAGE
//!
//! 本模块只编排流程，不实现具体步骤 — 所有步骤都在
//! `da_xflash_setup` / `da_xflash_protocol` / `da_extension` 子模块。

use log::{info, trace, warn};
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use crate::connection::session::{
    mark_optional_query_failed as save_optional_query_failure, optional_query_failed,
};
use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::DA_EXT_BOOT_ADDR;

// =============================================================================
// 公开常量 — 复用子模块的 DA extensions 相关 magic / devctrl
// =============================================================================

/// DA extensions CUSTOM_ACK 期望的响应 magic（对齐刷机匣 xflash_lib.py:1253）
pub const DA_EXTENSIONS_ACK_MAGIC: u32 = 0xA1A2A3A4;
/// CUSTOM_ACK devctrl id
const DA_EXTENSIONS_DEVCTRL_ACK: u32 = 0x0F0000;
/// CUSTOM_SET_STORAGE devctrl id（对齐刷机匣 xflash_lib.py:1256）
const DA_EXTENSIONS_DEVCTRL_SET_STORAGE: u32 = 0x0F0005;
const QUERY_EXPIRE_DATE: &str = "get_expire_date";
const QUERY_CONNECTION_AGENT: &str = "get_connection_agent";
const QUERY_SLA_STATUS: &str = "get_sla_status";

// =============================================================================
// DA 加载主流程
// =============================================================================

impl<'a> DAXFlash<'a> {
    fn optional_query_should_skip(&self, name: &str) -> bool {
        self.optional_query_failures.iter().any(|item| item == name) || optional_query_failed(name)
    }

    fn mark_optional_query_failed(&mut self, name: &str) {
        if !self.optional_query_failures.iter().any(|item| item == name) {
            self.optional_query_failures.push(name.to_string());
        }
        save_optional_query_failure(name);
    }

    /// 上传 DA（完整流程，对齐 Python upload_da）
    pub fn upload_da(&mut self) -> Result<bool, String> {
        trace!("开始 DA 加载流程...");

        if !self.upload_da1()? {
            return Err("Stage1 上传失败".to_string());
        }

        // --- 速度级别 1 = 完整协议，2/3 = 跳过可选查询 ---
        if self.da_x_speed == 1 {
            if self.optional_query_should_skip(QUERY_EXPIRE_DATE) {
                trace!("跳过已知失败的可选查询: {}", QUERY_EXPIRE_DATE);
            } else {
                match self.get_expire_date() {
                    Ok(d) if !d.is_empty() => trace!("  过期日期: {:02X?}", d),
                    Err(e) => {
                        warn!("get_expire_date 失败 (可能不支持): {}", e);
                        self.mark_optional_query_failed(QUERY_EXPIRE_DATE);
                    }
                    _ => {}
                }
            }

            if let Err(e) = self.set_reset_key(0x68) {
                warn!("set_reset_key 失败 (可能不支持): {}", e);
            }

            if let Err(e) = self.set_checksum_level(0x0) {
                warn!("set_checksum_level 失败 (可能不支持): {}", e);
            }
        } else {
            trace!(
                "[SPEED{}] 跳过 get_expire_date / set_reset_key / set_checksum_level",
                self.da_x_speed
            );
        }

        // 连接代理判断（级别3直接根据已知模式推断）
        let is_brom_conn = if self.da_x_speed >= 3 {
            // 极速模式：跳过 get_connection_agent，直接根据模式推断
            let brom = !self.preloader.is_preloader_mode;
            trace!(
                "[SPEED3] 跳过 get_connection_agent，推断 conn_agent={}",
                if brom { "brom" } else { "preloader" }
            );
            brom
        } else {
            let conn_agent = if self.optional_query_should_skip(QUERY_CONNECTION_AGENT) {
                trace!("跳过已知失败的可选查询: {}", QUERY_CONNECTION_AGENT);
                "brom".to_string()
            } else {
                match self.get_connection_agent() {
                    Ok(agent) => agent,
                    Err(e) => {
                        warn!("get_connection_agent 失败: {}", e);
                        self.mark_optional_query_failed(QUERY_CONNECTION_AGENT);
                        "brom".to_string()
                    }
                }
            };
            trace!("  连接代理: {}", conn_agent);
            conn_agent == "brom"
        };

        if is_brom_conn {
            if let Some(emi_data) = self.emi.clone() {
                trace!("发送 EMI 数据...");
                self.send_emi(&emi_data)?;
            } else {
                warn!("未找到 EMI 数据，跳过发送");
            }
        }

        if !self.upload_da2()? {
            return Err("Stage2 上传失败".to_string());
        }

        // DA2 启动后会发送 SYNC 信号（约 0.5-1s 后到达），需消费掉以免污染后续命令。
        // 事件驱动排空（替代盲等 sleep(800)）：短超时轮询读取并消费 SYNC，
        // 管道连续静默即停止——SYNC 早到省时、晚到也不会提前退出丢包。
        self.drain_until_quiet(1500);

        if self.da_x_speed == 1 {
            if self.optional_query_should_skip(QUERY_SLA_STATUS) {
                trace!("跳过已知失败的可选查询: {}", QUERY_SLA_STATUS);
            } else {
                match self.get_sla_status() {
                    Ok(sla) => {
                        if sla != 0 {
                            trace!("  DA SLA 已启用: 0x{:08X}", sla);
                        } else {
                            trace!("  DA SLA 未启用");
                        }
                    }
                    Err(e) => {
                        warn!("get_sla_status 失败: {}", e);
                        self.mark_optional_query_failed(QUERY_SLA_STATUS);
                    }
                }
            }
        } else {
            trace!("[SPEED{}] 跳过 get_sla_status", self.da_x_speed);
        }

        if !self.preloader.device.is_libusb() {
            // 串口模式：跳过 USB 高速重连，直接执行精简 reinit
            trace!("[PRELOADER] 串口模式，跳过 USB 高速重连");
            if let Err(e) = self.reinit() {
                warn!("reinit 失败: {}", e);
            }
        } else if self.da_x_speed >= 3 {
            // 极速模式：跳过设备信息查询，但仍执行 USB 高速重连
            trace!("[SPEED3] 跳过设备信息查询，直接执行 USB 高速重连");
            self.try_usb_high_speed_reconnect();
        } else if let Err(e) = self.reinit() {
            warn!("reinit 失败: {}", e);
        }

        // 10. 加载 DA extensions（对齐 Python xflash_lib.py 第 1247-1258 行）
        //      Python: daextdata = self.xft.patch()
        //              if self.boot_to(addr=0x4FFF0000, da=daextdata):
        //                  ret = self.send_devctrl(XCmd.CUSTOM_ACK)
        //                  status = self.status()
        //                  if status == 0x0 and unpack("<I", ret)[0] == 0xA1A2A3A4:
        trace!("正在加载 DA extensions...");

        // 清空 USB 输入缓冲区：reinit() 后设备可能发送残留数据，
        // 如果不干净，会污染后续 BOOT_TO 命令的响应。
        self.drain_usb_input();

        if self.patch_da
            && let Some(ext_data) = self.generate_da_extensions()
        {
            match self.boot_to(DA_EXT_BOOT_ADDR, &ext_data, true, 0.5) {
                Ok(_) => {
                    // boot_to 已读取 SYNC status，DA extension 代码正在执行。
                    // 等待 DA extensions 完全初始化，然后清空 USB 缓冲区，
                    // 避免残留数据污染 CUSTOM_ACK 的响应。
                    sleep(Duration::from_millis(100));
                    self.drain_usb_input();
                    self.load_da_extensions_after_boot();
                }
                Err(e) => {
                    warn!("boot_to(extensions) 失败: {}", e);
                }
            }
        }
        if !self.daext {
            warn!("DA extensions 未启用");
        }

        info!("DA 加载完成");

        // === 保存 DA 会话状态 ===
        // DA 加载成功后，将当前设备状态写入 .state 文件，
        // 下次程序启动时如果检测到设备已处于 DA 模式且 .state 有效，可以跳过 BROM→DA 流程。
        self.save_session_state();

        Ok(true)
    }

    /// BOOT_TO 成功后的 CUSTOM_ACK / CUSTOM_SET_STORAGE 流程
    fn load_da_extensions_after_boot(&mut self) {
        // Python 第 1251 行：boot_to 成功后立刻发送 CUSTOM_ACK
        // boot_to 已等待响应，只需极短缓冲给设备切换上下文
        std::thread::sleep(Duration::from_millis(20));
        if let Ok(ack) = self.send_devctrl(DA_EXTENSIONS_DEVCTRL_ACK, None) {
            // send_devctrl 内部已消费 xread 后的 status 包（DA 协议 4 包：
            //   stage1_status → stage2_status → xread_data → status_after_data）
            // 不再重复读取 status——否则第 5 个包不存在，5s 超时导致：
            //   DA Extensions 未启用 → HACC 软件回退 → DA 状态机失步 → 写入超时
            // magic 匹配即认为成功（DA 返回 0xA1A2A3A4 时 status 必为 0）
            if ack.len() >= 4 {
                let magic = u32::from_le_bytes([ack[0], ack[1], ack[2], ack[3]]);
                if magic == DA_EXTENSIONS_ACK_MAGIC {
                    // 检测设备存储类型：0=eMMC, 1=UFS, 2=SD, 3=MMC, 6=UFS_CARD
                    let storage_type = self
                        .get_emmc_info()
                        .map(|info| match info.emmc_type.as_str() {
                            "UFS" => 1u32,
                            "SD" => 2u32,
                            "MMC" => 3u32,
                            "UFS_CARD" => 6u32,
                            _ => 0u32, // 默认 eMMC
                        })
                        .unwrap_or(0u32);
                    // Python 第 1256 行：CUSTOM_ACK 成功后立即调用 custom_set_storage
                    if self
                        .send_devctrl(
                            DA_EXTENSIONS_DEVCTRL_SET_STORAGE,
                            Some(&storage_type.to_le_bytes()),
                        )
                        .is_ok()
                    {
                        let type_name = match storage_type {
                            1 => "UFS",
                            2 => "SD",
                            3 => "MMC",
                            6 => "UFS_CARD",
                            _ => "eMMC",
                        };
                        info!("DA Extensions 加载成功，存储类型已设置为 {}", type_name);
                        self.daext = true;
                    } else {
                        warn!("custom_set_storage 失败，extensions 功能可能受限");
                        self.daext = true;
                    }
                } else {
                    warn!(
                        "DA extensions CUSTOM_ACK 验证失败 (magic=0x{:08X}, 期望=0x{:08X})",
                        magic, DA_EXTENSIONS_ACK_MAGIC
                    );
                }
            } else {
                warn!("DA extensions CUSTOM_ACK 响应为空 (len={})", ack.len());
            }
        }
    }
}

// =============================================================================
// 工具函数
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 清空 USB 输入缓冲区（带 50ms 短超时），丢弃 reinit() 后的残留数据
    ///
    /// 与 Python xflash_lib.py 第 1240 行附近的手动清理一致：
    /// reinit() 后设备可能发送残留数据，如果不清空，会污染 BOOT_TO 响应。
    fn drain_usb_input(&mut self) {
        let mut drain_buf = [0u8; 512];
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader.device.set_timeout(Duration::from_millis(50));
        loop {
            match self.preloader.device.read(&mut drain_buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    trace!("[DRAIN] discarded {} bytes", n);
                }
            }
        }
        self.preloader.device.set_timeout(orig_timeout);
    }

    /// 事件驱动清空 IN 管道：50ms 短超时轮询读取并消费所有数据，
    /// 直到管道连续静默（`QUIET_TICKS` 次读超时）或到达 `max_wait_ms` 上限。
    ///
    /// 替代 DA2 启动后的盲等 `sleep(800)`：DA2 在 ~0.5-1s 后发 SYNC 包，
    /// 盲等要么浪费时间（SYNC 早到），要么提前退出丢包（SYNC 晚到，污染后续命令）。
    /// 轮询消费 SYNC 后再保持短静默即停止，既省时又正确。
    fn drain_until_quiet(&mut self, max_wait_ms: u64) {
        const TICK_MS: u64 = 50;
        const QUIET_TICKS: u32 = 3; // 连续 ~150ms 无数据即判定静默
        let start = Instant::now();
        let mut quiet = 0u32;
        let mut buf = [0u8; 512];
        let orig = self.preloader.device.get_timeout();
        loop {
            self.preloader
                .device
                .set_timeout(Duration::from_millis(TICK_MS));
            match self.preloader.device.read(&mut buf) {
                Ok(0) | Err(_) => {
                    quiet += 1;
                    if quiet >= QUIET_TICKS {
                        break;
                    }
                }
                Ok(_) => {
                    quiet = 0;
                }
            }
            if start.elapsed().as_millis() as u64 >= max_wait_ms {
                break;
            }
        }
        self.preloader.device.set_timeout(orig);
    }
}

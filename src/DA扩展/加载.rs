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
use std::time::Duration;

use crate::DA扩展::DAXFlash;

// =============================================================================
// 公开常量 — 复用子模块的 DA extensions 相关 magic / devctrl
// =============================================================================

/// DA extensions CUSTOM_ACK 期望的响应 magic（对齐刷机匣 xflash_lib.py:1253）
pub const DA_EXTENSIONS_ACK_MAGIC: u32 = 0xA1A2A3A4;
/// CUSTOM_ACK devctrl id
const DA_EXTENSIONS_DEVCTRL_ACK: u32 = 0x0F0000;
/// CUSTOM_SET_STORAGE devctrl id（对齐刷机匣 xflash_lib.py:1256）
const DA_EXTENSIONS_DEVCTRL_SET_STORAGE: u32 = 0x0F0005;

// =============================================================================
// DA 加载主流程
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 上传 DA（完整流程，对齐 Python upload_da）
    pub fn upload_da(&mut self) -> Result<bool, String> {
        trace!("开始 DA 加载流程...");

        if !self.upload_da1()? {
            return Err("Stage1 上传失败".to_string());
        }

        match self.get_expire_date() {
            Ok(d) if !d.is_empty() => trace!("  过期日期: {:02X?}", d),
            Err(e) => warn!("get_expire_date 失败 (可能不支持): {}", e),
            _ => {}
        }

        if let Err(e) = self.set_reset_key(0x68) {
            warn!("set_reset_key 失败 (可能不支持): {}", e);
        }

        if let Err(e) = self.set_checksum_level(0x0) {
            warn!("set_checksum_level 失败 (可能不支持): {}", e);
        }

        let conn_agent = match self.get_connection_agent() {
            Ok(agent) => agent,
            Err(e) => {
                warn!("get_connection_agent 失败: {}", e);
                "brom".to_string()
            }
        };
        trace!("  连接代理: {}", conn_agent);

        if conn_agent == "brom" {
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

        match self.get_sla_status() {
            Ok(sla) => {
                if sla != 0 {
                    trace!("  DA SLA 已启用: 0x{:08X}", sla);
                } else {
                    trace!("  DA SLA 未启用");
                }
            }
            Err(e) => warn!("get_sla_status 失败: {}", e),
        }

        if let Err(e) = self.reinit() {
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
            match self.boot_to(0x4FFF0000, &ext_data, true, 0.5) {
                Ok(_) => self.load_da_extensions_after_boot(),
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
        // 给 extensions 一点初始化时间
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(ack) = self.send_devctrl(DA_EXTENSIONS_DEVCTRL_ACK, None) {
            // Python 第 1252 行：send_devctrl 后还要读一次 status
            let status = self.status();
            let status_ok = match status {
                Ok(s) => s == 0,
                Err(_) => false,
            };
            if ack.len() >= 4 {
                let magic = u32::from_le_bytes([ack[0], ack[1], ack[2], ack[3]]);
                if status_ok && magic == DA_EXTENSIONS_ACK_MAGIC {
                    // Python 第 1256 行：CUSTOM_ACK 成功后立即调用 custom_set_storage
                    // CUSTOM_SET_STORAGE 参数：0=eMMC, 1=UFS
                    if self
                        .send_devctrl(DA_EXTENSIONS_DEVCTRL_SET_STORAGE, Some(&0u32.to_le_bytes()))
                        .is_ok()
                    {
                        info!("DA Extensions 加载成功，存储类型已设置为 eMMC");
                        self.daext = true;
                    } else {
                        warn!("custom_set_storage 失败，extensions 功能可能受限");
                        self.daext = true;
                    }
                } else {
                    warn!(
                        "DA extensions CUSTOM_ACK 验证失败 (status={:?}, magic=0x{:08X})",
                        status, magic
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
        let mut drain_buf = [0u8; 64];
        loop {
            let orig_timeout = self.preloader.device.get_timeout();
            self.preloader.device.set_timeout(Duration::from_millis(50));
            match self.preloader.device.read(&mut drain_buf) {
                Ok(0) | Err(_) => {
                    self.preloader.device.set_timeout(orig_timeout);
                    break;
                }
                Ok(n) => {
                    trace!("[DRAIN] discarded {} bytes", n);
                }
            }
            self.preloader.device.set_timeout(orig_timeout);
        }
    }
}

//! XFlash 协议原语层
//!
//! 本模块承载 XFlash 协议层最底层的数据包/状态机原语，被 DAXFlash 的所有
//! 高层方法（setup/upload/read/erase/...）复用：
//! - 常量定义（CMD_MAGIC、所有 CMD_* 命令码）
//! - 包头打包（pack3）
//! - 同步/状态读取（xflash_sync、status、xread、xread_data）
//! - ACK 响应（ack、send_ack）
//! - 设备控制命令（send_devctrl、get/set_*_devctrl 包装）
//!
//! 设计原则：保持与 Python mtkclient xflash_lib.py 的协议层一一对应，
//! 不依赖任何上层 DAXFlash 业务逻辑，方便单测与回溯。

use log::{info, trace, warn};
use std::time::Duration;

use crate::da::xflash::DAXFlash;

// =============================================================================
// XFlash 命令常量
// =============================================================================

/// 所有 XFlash 包头的 magic 标识
pub const CMD_MAGIC: u32 = 0xFEEEEEEF;
const CMD_SYNC_SIGNAL: u32 = 0x434E5953;
const CMD_SETUP_ENVIRONMENT: u32 = 0x010100;
const CMD_SETUP_HW_INIT_PARAMS: u32 = 0x010101;
const CMD_INIT_EXT_RAM: u32 = 0x01000A;
const CMD_BOOT_TO: u32 = 0x010008;
pub const CMD_READ_DATA: u32 = 0x010005; // XFlash 读分区命令
pub const CMD_WRITE_DATA: u32 = 0x010004; // 写入数据命令

pub const CMD_FORMAT: u32 = 0x010003; // 格式化命令
pub const SET_META_BOOT_MODE: u32 = 0x020006;

// =============================================================================
// 包头工具
// =============================================================================

/// pack3: 生成 XFlash 参数包头 (magic(4) + data_type(4) + length(4))
pub fn pack3(magic: u32, data_type: u32, length: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&magic.to_le_bytes());
    buf[4..8].copy_from_slice(&data_type.to_le_bytes());
    buf[8..12].copy_from_slice(&length.to_le_bytes());
    buf
}

// =============================================================================
// ACK 响应枚举 — 替代裸 u32 返回值
// =============================================================================

/// ACK 响应枚举 — 替代裸 u32 返回值
#[derive(Debug, PartialEq)]
pub enum AckResult {
    /// status == 0，设备就绪，继续传输
    Continue,
    /// status != 0，传输终止或设备报错
    Terminated(u32),
}

// =============================================================================
// 协议原语 — 块 1: 状态 / 数据读取
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 读取 XFlash 协议数据
    /// 流程：读取 12 字节头（magic + type + length）→ 验证 magic → 读取数据
    /// 返回：读取到的数据长度（如果是 4 字节则返回 u32 值）
    pub(crate) fn xread(&mut self) -> Result<u32, String> {
        // 设备处理 setup_hw_init 后可能需要数百毫秒才返回响应
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader
            .device
            .set_timeout(Duration::from_millis(1000));

        let result = self.xread_inner();
        self.preloader.device.set_timeout(orig_timeout);
        result
    }

    fn xread_inner(&mut self) -> Result<u32, String> {
        // 读取 12 字节的 XFlash 头
        let mut header = [0; 12];
        self.preloader.device.read_exact(&mut header)?;

        let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let _data_type = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let length = u32::from_le_bytes(header[8..12].try_into().unwrap());

        if magic != CMD_MAGIC {
            return Err(format!("XFlash 头 magic 错误: 0x{:08X}", magic));
        }

        // 读取数据
        if length > 0 {
            let mut data = vec![0; length as usize];
            self.preloader.device.read_exact(&mut data)?;

            // 如果数据是 4 字节，返回 u32 值
            if length == 4 {
                return Ok(u32::from_le_bytes(data[0..4].try_into().unwrap()));
            }
        }

        Ok(0)
    }

    /// 读取 status (4 字节小端)
    pub(crate) fn status(&mut self) -> Result<u32, String> {
        let mut hdr = [0u8; 12];
        self.preloader.device.read_exact(&mut hdr)?;
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        if magic != CMD_MAGIC {
            return Err(format!("status magic error: 0x{:08X}", magic));
        }
        if length > 0 {
            let mut tmp = vec![0u8; length as usize];
            self.preloader.device.read_exact(&mut tmp)?;
            if length == 4 {
                let val = u32::from_le_bytes(tmp[..4].try_into().unwrap());
                // Python 特殊情况：如果 status == 0xFEEEEEEF，返回 0
                if val == 0xFEEEEEEF {
                    return Ok(0);
                }
                return Ok(val);
            } else if length == 2 {
                return Ok(u16::from_le_bytes(tmp[..2].try_into().unwrap()) as u32);
            }
        }
        Ok(0)
    }

    /// 读取 XFlash 数据并返回 Vec
    pub(crate) fn xread_data(&mut self) -> Result<Vec<u8>, String> {
        let mut hdr = [0u8; 12];
        self.preloader.device.read_exact(&mut hdr)?;
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        if magic != CMD_MAGIC {
            return Err(format!("xread magic error: 0x{:08X}", magic));
        }
        if length > 0 {
            let mut data = vec![0u8; length as usize];
            self.preloader.device.read_exact(&mut data)?;
            Ok(data)
        } else {
            Ok(vec![])
        }
    }
}

// =============================================================================
// 协议原语 — 块 2: ACK 读写
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 发送 ACK 并读取设备响应
    /// 写入 12B header(CMD_MAGIC + 0x01 + 4) + 4B 零值 → 读取 status
    /// 返回 AckResult::Continue（可继续）或 AckResult::Terminated（终止）
    pub(crate) fn ack(&mut self) -> AckResult {
        if let Err(e) = self.send_ack() {
            trace!("[ack] send_ack failed: {}", e);
            return AckResult::Terminated(1);
        }
        match self.status() {
            Ok(0) => AckResult::Continue,
            Ok(n) => AckResult::Terminated(n),
            Err(_) => AckResult::Terminated(3),
        }
    }

    fn send_ack(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader
            .device
            .write(&hdr)
            .map_err(|e| format!("send_ack write hdr: {}", e))?;
        self.preloader
            .device
            .write(&0u32.to_le_bytes())
            .map_err(|e| format!("send_ack write data: {}", e))?;
        Ok(())
    }
}

// =============================================================================
// 协议原语 — 块 3: 同步 / devctrl
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// XFlash 同步命令
    /// Python: sync() → 只 xsend(CMD_SYNC_SIGNAL)，不读 status 也不读 response
    pub(crate) fn xflash_sync(&mut self) -> Result<bool, String> {
        trace!("执行 XFlash 同步命令...");

        // Python: self.sync() → self.xsend(self.Cmd.SYNC_SIGNAL) = 0x434E5953
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_SYNC_SIGNAL.to_le_bytes())?;

        trace!("[SYNC] 已发送 SYNC_SIGNAL (0x434E5953)");
        Ok(true)
    }

    /// 发送 devctrl 命令
    /// Python: 任何阶段都返回 b""，不抛异常
    pub(crate) fn send_devctrl(
        &mut self,
        cmd: u32,
        param: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        // xsend(Cmd.DEVICE_CTRL) — DEVICE_CTRL = 0x010009
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;

        let st = self.status()?;
        if st != 0 {
            if st != 0xC0010004 {
                warn!("send_devctrl DEVICE_CTRL 阶段1 状态: 0x{:08X}", st);
            }
            return Ok(vec![]);
        }

        // xsend(cmd)
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&cmd.to_le_bytes())?;

        let st2 = self.status()?;
        if st2 != 0 {
            if st2 != 0xC0010004 {
                warn!("send_devctrl(0x{:06X}) 状态: 0x{:08X}", cmd, st2);
            }
            return Ok(vec![]);
        }

        if let Some(p) = param {
            let pkt3 = pack3(CMD_MAGIC, 0x01, p.len() as u32);
            self.preloader.device.write(&pkt3)?;
            self.preloader.device.write(p)?;
            let st3 = self.status()?;
            if st3 != 0 {
                warn!("send_devctrl param 状态: 0x{:08X}", st3);
                return Ok(vec![]);
            }
        } else {
            let resp = self.xread_data()?;
            trace!(
                "[send_devctrl] cmd=0x{:06X} xread returned {} bytes",
                cmd,
                resp.len()
            );
            return Ok(resp);
        }

        Ok(vec![])
    }

    /// 获取连接代理（brom 或 preloader）
    /// Python: 返回 b"" 或 None 时视为失败
    pub(crate) fn get_connection_agent(&mut self) -> Result<String, String> {
        let data = self.send_devctrl(0x010102, None)?; // GET_CONNECTION_AGENT
        if data.is_empty() {
            return Err("get_connection_agent returned empty".to_string());
        }
        Ok(String::from_utf8_lossy(&data).to_string())
    }

    /// 设置重置键
    pub(crate) fn set_reset_key(&mut self, key: u32) -> Result<(), String> {
        self.send_devctrl(0x010103, Some(&key.to_le_bytes()))?;
        Ok(())
    }

    /// 设置校验级别
    pub(crate) fn set_checksum_level(&mut self, level: u32) -> Result<(), String> {
        self.send_devctrl(0x010104, Some(&level.to_le_bytes()))?;
        Ok(())
    }

    /// 获取过期日期
    pub(crate) fn get_expire_date(&mut self) -> Result<Vec<u8>, String> {
        let data = self.send_devctrl(0x010105, None)?;
        if data.is_empty() {
            return Err("get_expire_date returned empty".to_string());
        }
        Ok(data)
    }

    /// 获取 SLA 状态
    pub(crate) fn get_sla_status(&mut self) -> Result<u32, String> {
        let data = self.send_devctrl(0x01010E, None)?; // SLA_ENABLED_STATUS
        if data.len() >= 4 {
            Ok(u32::from_le_bytes(data[..4].try_into().unwrap()))
        } else {
            Err("sla_status empty".to_string())
        }
    }

    /// 设置 OEM 解锁开关状态（对齐 C# 版）
    /// 命令 0x040009 用于控制 OEM 解锁状态
    /// enable: true=解锁, false=锁定
    pub fn set_oem_unlock(&mut self, enable: bool) -> Result<(), String> {
        let value = if enable { 1u32 } else { 0u32 };
        let _ = self.send_devctrl(0x040009, Some(&value.to_le_bytes()))?;
        info!(
            "OEM 解锁状态已设置: {}",
            if enable { "解锁" } else { "锁定" }
        );
        Ok(())
    }
}

// =============================================================================
// 单元测试 — 协议原语回归验证
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack3_layout() {
        let pkt = pack3(0xDEADBEEF, 0x12345678, 0x9ABCDEF0);
        // magic
        assert_eq!(pkt[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
        // data_type
        assert_eq!(pkt[4..8], [0x78, 0x56, 0x34, 0x12]);
        // length
        assert_eq!(pkt[8..12], [0xF0, 0xDE, 0xBC, 0x9A]);
    }

    #[test]
    fn ack_result_equality() {
        assert_eq!(AckResult::Continue, AckResult::Continue);
        assert_ne!(AckResult::Continue, AckResult::Terminated(1));
    }
}

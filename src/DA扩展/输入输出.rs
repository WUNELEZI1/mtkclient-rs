//! DA 设备 IO 与控制命令
//!
//! 包含 DAXFlash 的设备读写、复位、关闭命令：
//! - `readflash_data`：通过 XFlash 协议读取 flash 原始字节
//! - `ack_silent`：发送 ACK 不读响应（避免偷吃下一个包）
//! - `reset_device`：通过 DA 重启设备（CMD_RESET）
//! - `close_device`：关闭设备（可选 jump_bl 重启）
//! - `patch_vbmeta`：vbmeta 修补占位
//!
//! 拆分动机：这些是"已加载 DA 之后"与设备交互的命令，与 DA 加载主流程
//! 完全解耦，独立成模块便于审查和单元测试。

use log::{info, trace, warn};

use crate::DA扩展::DAXFlash;
use crate::DA扩展::protocol::{CMD_MAGIC, CMD_READ_DATA, pack3};

// =============================================================================
// Flash 数据读取
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 读取 flash 数据，返回原始字节
    /// 对齐 Python xflash_lib.py:879-891 (filename="" 分支):
    ///   get_packet_length → cmd_read_data → xread 循环 (header+data) → ack
    /// 注意：filename="" 分支没有 readflash_final 包，设备不会发送 final
    pub(crate) fn readflash_data(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        // 1. get_packet_length (send_devctrl 0x040007 + status)
        // Python: get_packet_length() → send_devctrl → if resp != "": status()
        let _ = self.send_devctrl(0x040007, None);
        let _ = self.status();

        // 2. cmd_read_data: xsend(CMD_READ_DATA) → status → send_param → status
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "readflash xsend")?;
        self.write_with_retry(&CMD_READ_DATA.to_le_bytes(), "readflash CMD")?;

        let st = self.status()?;
        if st != 0 {
            return Err(format!("READ_DATA status=0x{:08X}", st));
        }

        // send_param: storage(4) + parttype(4) + addr(8) + size(8) + NandExtension(32)
        // 这里的 size 保持调用方传入的"实际分区大小"，不要改成请求读取长度
        let mut param = Vec::with_capacity(56);
        param.extend_from_slice(&1u32.to_le_bytes()); // storage = 1 (eMMC)
        param.extend_from_slice(&8u32.to_le_bytes()); // parttype = 8 (USER)
        param.extend_from_slice(&addr.to_le_bytes());
        param.extend_from_slice(&size.to_le_bytes());
        param.extend_from_slice(&[0u8; 32]); // NandExtension 全零
        let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.write_with_retry(&param_pkt, "readflash param_hdr")?;
        self.write_with_retry(&param, "readflash param")?;

        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("send_param status=0x{:08X}", st2));
        }

        // 3. 数据读取循环 — 对齐 Python xflash_lib.py:879-891 (filename="" 分支)
        // 预分配 buffer，避免循环内多次重新分配（对大分区如 super 显著提升性能）
        let mut buffer = Vec::with_capacity(size as usize);
        let mut remaining = size as usize;

        while remaining > 0 {
            // 读 12 字节头
            let mut hdr = [0u8; 12];
            match self.preloader.device.read_exact(&mut hdr) {
                Ok(0) => {
                    // ZLP 或空响应 — 设备无更多数据，正常结束
                    trace!("[readflash_data] ZLP on header read, ending loop");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    trace!(
                        "[readflash_data] read header error (end of transfer): {}",
                        e
                    );
                    break;
                }
            }

            let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let slength = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);

            if magic != CMD_MAGIC {
                // 读到非预期数据，可能是残留状态包，容错退出
                trace!(
                    "[readflash_data] bad magic: 0x{:08X} at offset {}, ending loop",
                    magic,
                    buffer.len()
                );
                break;
            }

            // 读数据
            let mut data = vec![0u8; slength as usize];
            if slength > 0
                && let Err(e) = self.preloader.device.read_exact(&mut data)
            {
                trace!("[readflash_data] read data error: {}", e);
                break;
            }

            // 追加数据（包括心跳包的 4 字节零值）
            buffer.extend_from_slice(&data);
            remaining = remaining.saturating_sub(data.len());

            // 发送 ACK（只发不读）
            // 关键：不在此处读 status！下一个数据包的包头就是 DA 对 ACK 的响应
            // 如果读 status，会偷吃下一个数据包
            if let Err(e) = self.ack_silent() {
                trace!("[readflash_data] send_ack failed: {}", e);
                break;
            }
        }

        trace!("[readflash_data] total read {} bytes", buffer.len());
        Ok(buffer)
    }

    /// 静默 ACK（仅发不读），用于 readflash_data 循环中不偷吃下一个包
    fn ack_silent(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&hdr, "ack_silent hdr")?;
        self.write_with_retry(&0u32.to_le_bytes(), "ack_silent data")?;
        Ok(())
    }
}

// =============================================================================
// 设备重置 / 关闭 / vbmeta 修补
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 通过 DA 重启设备（XFlash CMD_RESET）
    /// 对齐刷机匣：0x010007 + param(storage=1, value=0x64)
    pub fn reset_device(&mut self) -> Result<(), String> {
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010007u32.to_le_bytes())?;
        let st = self.status()?;
        if st != 0 {
            return Err(format!("CMD_RESET status: 0x{:08X}", st));
        }

        // param: storage(4) + value(4) + zeros(20) = 28 字节
        let mut param = vec![0u8; 28];
        param[0..4].copy_from_slice(&1u32.to_le_bytes()); // storage=eMMC
        param[4..8].copy_from_slice(&100u32.to_le_bytes()); // value=0x64
        let param_pkt = pack3(CMD_MAGIC, 0x01, 28);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("CMD_RESET param status: 0x{:08X}", st2));
        }
        info!("设备已通过 DA 重启");
        Ok(())
    }

    pub fn close_device(&mut self, reset: bool) {
        if reset {
            if let Err(e) = self.preloader.jump_bl() {
                warn!("jump_bl 失败: {}", e);
            } else {
                info!("已发送 JUMP_BL 命令，设备将重启");
            }
        }
    }

    /// 修补 vbmeta
    /// 委托给 安全::vbmeta::vbmeta_disable 实现（自动处理 vbmeta_a / vbmeta_b / vbmeta）
    pub fn patch_vbmeta(&mut self, mode: u32) -> Result<(), String> {
        crate::安全::vbmeta::vbmeta_disable(self, mode)
    }
}

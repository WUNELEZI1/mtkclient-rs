//! 协议原语 — 块 1: 状态 / 数据读取

use super::{CMD_MAGIC, DA_CTRL_TIMEOUT_MS};
use log::trace;
use std::time::Duration;

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// 读取 XFlash 协议数据
    /// 流程：读取 12 字节头（magic + type + length）→ 验证 magic → 读取数据
    /// 返回：读取到的数据长度（如果是 4 字节则返回 u32 值）
    pub(crate) fn xread(&mut self) -> Result<u32, String> {
        // 设备处理 setup_hw_init 后可能需要数百毫秒才返回响应。
        // 用 max(当前超时, DA_CTRL_TIMEOUT_MS) 等待首字节，避免慢速串口 status 超时；
        // 若调用方已设更短超时（with_short_timeout 快速失败），保留之。
        let orig_timeout = self.preloader.device.get_timeout();
        let eff = orig_timeout.max(Duration::from_millis(DA_CTRL_TIMEOUT_MS));
        self.preloader.device.set_timeout(eff);

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
            trace!("[xread] bad magic: 0x{:08X}", magic);
            return Err(format!("XFlash 头 magic 错误: 0x{:08X}", magic));
        }

        trace!("[xread] dt={:08X} len={}", _data_type, length);

        // 读取数据
        if length > 0 {
            let mut data = vec![0; length as usize];
            self.preloader.device.read_exact(&mut data)?;

            // 如果数据是 4 字节，返回 u32 值
            if length == 4 {
                let val = u32::from_le_bytes(data[0..4].try_into().unwrap());
                trace!("[xread] val=0x{:08X}", val);
                return Ok(val);
            }
        }

        Ok(0)
    }

    /// 读取 status (4 字节小端)
    pub(crate) fn status(&mut self) -> Result<u32, String> {
        // DA 控制读：慢速串口下 DA 回 status 可能超默认 1s，用 max(当前, DA_CTRL_TIMEOUT_MS)
        // 等待首字节，避免 status 读取 Operation timed out。调用方已设更短超时时保留之。
        let orig_timeout = self.preloader.device.get_timeout();
        let eff = orig_timeout.max(Duration::from_millis(DA_CTRL_TIMEOUT_MS));
        self.preloader.device.set_timeout(eff);

        let result = (|| {
            let mut hdr = [0u8; 12];
            self.preloader.device.read_exact(&mut hdr)?;
            let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let _data_type = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
            let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
            if magic != CMD_MAGIC {
                trace!("[status] bad magic: 0x{:08X}", magic);
                return Err(format!("status magic error: 0x{:08X}", magic));
            }
            if length > 0 {
                let mut tmp = vec![0u8; length as usize];
                self.preloader.device.read_exact(&mut tmp)?;
                if length == 4 {
                    let val = u32::from_le_bytes(tmp[..4].try_into().unwrap());
                    // Python 特殊情况：如果 status == 0xFEEEEEEF，返回 0
                    if val == 0xFEEEEEEF {
                        trace!("[status] 0xFEEEEEEF → 0");
                        return Ok(0);
                    }
                    trace!(
                        "[status] dt={:08X} len={} val=0x{:08X}",
                        _data_type, length, val
                    );
                    return Ok(val);
                } else if length == 2 {
                    let val = u16::from_le_bytes(tmp[..2].try_into().unwrap()) as u32;
                    trace!(
                        "[status] dt={:08X} len={} val=0x{:04X}",
                        _data_type, length, val
                    );
                    return Ok(val);
                }
                trace!(
                    "[status] dt={:08X} len={} (non-u32/u16)",
                    _data_type, length
                );
            }
            trace!("[status] dt={:08X} len={} (no payload)", _data_type, length);
            Ok(0)
        })();

        self.preloader.device.set_timeout(orig_timeout);
        result
    }

    /// 读取 XFlash 数据并返回 Vec
    pub(crate) fn xread_data(&mut self) -> Result<Vec<u8>, String> {
        // DA 控制读：同 status()，用 max(当前, DA_CTRL_TIMEOUT_MS) 避免慢速串口超时
        let orig_timeout = self.preloader.device.get_timeout();
        let eff = orig_timeout.max(Duration::from_millis(DA_CTRL_TIMEOUT_MS));
        self.preloader.device.set_timeout(eff);

        let result = (|| {
            let mut hdr = [0u8; 12];
            self.preloader.device.read_exact(&mut hdr)?;
            let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let _data_type = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
            let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
            if magic != CMD_MAGIC {
                trace!("[xread_data] bad magic: 0x{:08X}", magic);
                return Err(format!("xread magic error: 0x{:08X}", magic));
            }
            trace!("[xread_data] dt={:08X} len={}", _data_type, length);
            if length > 0 {
                let mut data = vec![0u8; length as usize];
                self.preloader.device.read_exact(&mut data)?;
                Ok(data)
            } else {
                Ok(vec![])
            }
        })();

        self.preloader.device.set_timeout(orig_timeout);
        result
    }
}

//! DA/XML 分区读写操作
//!
//! 对齐 Python mtkclient Library/DA/xml/xml_lib.py 中的 read/write/erase/format

use log::{debug, info, warn};

use crate::da::xml::protocol::{cmd, send_xml_cmd, read_xml_data, send_xml_data_blocks};
use crate::da::xml::DAXML;

impl<'a> DAXML<'a> {
    /// 读取分区数据（对齐 XFlash 的 readflash_data）
    pub fn readflash_data(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        if size == 0 {
            return Ok(Vec::new());
        }

        info!("[XML DA] 读取闪存: addr=0x{:X}, size={} 字节", addr, size);

        const CHUNK_SIZE: u64 = 0x100000; // 1MB 块
        let mut result = Vec::with_capacity(size as usize);
        let mut offset = 0u64;

        while offset < size {
            let to_read = (size - offset).min(CHUNK_SIZE);

            let params = vec![
                ("address".to_string(), format!("0x{:X}", addr + offset)),
                ("length".to_string(), format!("{}", to_read)),
            ];

            let resp = send_xml_cmd(self.preloader, cmd::READ_FLASH, &params)?;
            if !resp.is_ok {
                return Err(format!(
                    "READ-FLASH 失败 @ 0x{:X}: {:?}",
                    addr + offset,
                    resp
                ));
            }

            // 读取实际数据
            let chunk = read_xml_data(self.preloader, to_read as usize, 30000)?;
            if chunk.len() != to_read as usize {
                warn!(
                    "[XML DA] 数据长度不匹配: 期望 {}，实际 {}",
                    to_read,
                    chunk.len()
                );
            }
            result.extend_from_slice(&chunk);
            offset += chunk.len() as u64;

            debug!(
                "[XML DA] 读取进度: {}/{} 字节 ({:.1}%)",
                offset,
                size,
                (offset as f64 / size as f64) * 100.0
            );
        }

        info!("[XML DA] 读取完成: {} 字节", result.len());
        Ok(result)
    }

    /// 写入分区数据
    pub fn write_flash_data(&mut self, addr: u64, data: &[u8]) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }

        info!(
            "[XML DA] 写入闪存: addr=0x{:X}, size={} 字节",
            addr,
            data.len()
        );

        const CHUNK_SIZE: usize = 0x100000; // 1MB 块
        let mut offset = 0usize;

        for chunk in data.chunks(CHUNK_SIZE) {
            let params = vec![
                ("address".to_string(), format!("0x{:X}", addr + offset as u64)),
                ("length".to_string(), format!("{}", chunk.len())),
            ];

            // 发送 WRITE-FLASH 命令
            let resp = send_xml_cmd(self.preloader, cmd::WRITE_FLASH, &params)?;
            if !resp.is_ok {
                return Err(format!(
                    "WRITE-FLASH 命令失败 @ 0x{:X}: {:?}",
                    addr + offset as u64,
                    resp
                ));
            }

            // 发送数据
            send_xml_data_blocks(self.preloader, chunk)?;

            // 读取确认
            let resp = send_xml_cmd(self.preloader, cmd::WRITE_FLASH, &[])?;
            if !resp.is_ok {
                return Err(format!(
                    "WRITE-FLASH 确认失败 @ 0x{:X}: {:?}",
                    addr + offset as u64,
                    resp
                ));
            }

            offset += chunk.len();
            debug!(
                "[XML DA] 写入进度: {}/{} 字节",
                offset,
                data.len()
            );
        }

        info!("[XML DA] 写入完成: {} 字节", data.len());
        Ok(())
    }

    /// 擦除分区
    pub fn erase_flash(&mut self, addr: u64, size: u64) -> Result<(), String> {
        info!(
            "[XML DA] 擦除闪存: addr=0x{:X}, size={} 字节",
            addr,
            size
        );

        let params = vec![
            ("address".to_string(), format!("0x{:X}", addr)),
            ("length".to_string(), format!("{}", size)),
        ];

        let resp = send_xml_cmd(self.preloader, cmd::ERASE_FLASH, &params)?;
        if !resp.is_ok {
            return Err(format!("ERASE-FLASH 失败: {:?}", resp));
        }

        info!("[XML DA] 擦除完成");
        Ok(())
    }

    /// 格式化分区
    pub fn format_flash(&mut self, addr: u64, size: u64) -> Result<(), String> {
        info!(
            "[XML DA] 格式化闪存: addr=0x{:X}, size={} 字节",
            addr,
            size
        );

        let params = vec![
            ("address".to_string(), format!("0x{:X}", addr)),
            ("length".to_string(), format!("{}", size)),
        ];

        let resp = send_xml_cmd(self.preloader, cmd::FORMAT_FLASH, &params)?;
        if !resp.is_ok {
            return Err(format!("FORMAT-FLASH 失败: {:?}", resp));
        }

        info!("[XML DA] 格式化完成");
        Ok(())
    }
}

//! DAXFlash 底层写入原语
//!
//! - `write_flash_data` — 按原始地址写入数据（带进度显示）
//! - `get_packet_length` — 获取写包长度
//! - `cmd_write_data`   — 发送写命令

use log::info;

use crate::da::xflash::{CMD_MAGIC, CMD_WRITE_DATA, DAXFlash, pack3};

impl<'a> DAXFlash<'a> {
    /// 按原始地址写入一段数据，供分区写入、seccfg/frp 等场景复用。
    /// 带进度显示：每 10% 输出一次进度，最后输出总耗时和速度。
    pub(crate) fn write_flash_data(
        &mut self,
        addr: u64,
        data: &[u8],
        storage: u32,
        parttype: u32,
    ) -> Result<(), String> {
        self.cmd_write_data(addr, data.len() as u64, storage, parttype)?;

        let write_packet_size = self.get_packet_length()?;
        let mut pos = 0;
        let total = data.len();
        let start_time = std::time::Instant::now();
        let mut last_pct = 0;
        // 预分配最大 param buffer，循环内复用避免重复分配
        let max_param_len = 8 + write_packet_size;
        let mut param = Vec::with_capacity(max_param_len);
        while pos < total {
            let dsize = std::cmp::min(write_packet_size, total - pos);
            let chunk = &data[pos..pos + dsize];
            let checksum: u16 = chunk.iter().map(|&b| b as u16).sum::<u16>();

            param.clear();
            param.extend_from_slice(&0u32.to_le_bytes());
            param.extend_from_slice(&(checksum as u32).to_le_bytes());
            param.extend_from_slice(chunk);

            let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
            self.preloader.device.write(&param_pkt)?;
            self.preloader.device.write(&param)?;

            pos += dsize;

            // 进度显示：每 10% 输出一次
            let pct = (pos * 100 / total) as u32;
            if pct >= last_pct + 10 {
                info!("  写入进度: {}% ({}/{} 字节)", pct, pos, total);
                last_pct = pct;
            }
        }

        let st = self.status()?;
        if st != 0 {
            return Err(format!("writeflash status error: 0x{:08X}", st));
        }

        self.send_devctrl(0x800005, None)?;

        let elapsed = start_time.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            total as f64 / elapsed / 1024.0 / 1024.0
        } else {
            0.0
        };
        info!(
            "  写入完成: {} 字节, 耗时 {:.2}s, 速度 {:.2} MB/s",
            total, elapsed, speed
        );
        Ok(())
    }

    /// 获取写包长度（对齐 Python get_packet_length）
    fn get_packet_length(&mut self) -> Result<usize, String> {
        // 发送 GET_PACKET_LENGTH (0x040007) 通过 devctrl
        let data = self.send_devctrl(0x040007, None)?;
        if data.len() >= 4 {
            let plen = u32::from_le_bytes(data[..4].try_into().unwrap());
            return Ok(plen as usize);
        }
        // 默认值（对齐 Python 默认行为）
        Ok(0x40000)
    }

    /// 发送写命令（对齐 Python cmd_write_data）
    fn cmd_write_data(
        &mut self,
        addr: u64,
        size: u64,
        storage: u32,
        parttype: u32,
    ) -> Result<bool, String> {
        // xsend(WRITE_DATA)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&CMD_WRITE_DATA.to_le_bytes())?;

        let st = self.status()?;
        if st == 0 {
            let mut param = Vec::with_capacity(56);
            param.extend_from_slice(&storage.to_le_bytes());
            param.extend_from_slice(&parttype.to_le_bytes());
            param.extend_from_slice(&addr.to_le_bytes());
            param.extend_from_slice(&size.to_le_bytes());
            param.extend_from_slice(&[0u8; 32]); // NandExtension 全零
            let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
            self.preloader.device.write(&param_pkt)?;
            self.preloader.device.write(&param)?;
            let st2 = self.status()?;
            return Ok(st2 == 0);
        }
        Err(format!("cmd_write_data status error: 0x{:08X}", st))
    }
}

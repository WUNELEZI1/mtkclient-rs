//! DAXFlash 上的分区读写擦操作
//!
//! - `read_gpt`   — 读取并保存 GPT
//! - `find_partition_addr` — 按名称查找分区地址
//! - `read_partition` — 读分区到文件
//! - `write_partition` — 写文件到分区
//! - `erase_partition` — 擦除分区
//! - `write_flash_data` / `cmd_write_data` / `get_packet_length` — 底层写入原语

use log::info;
use std::fs::File;
use std::io::Write;

use crate::DA扩展::{CMD_FORMAT, CMD_MAGIC, CMD_WRITE_DATA, DAXFlash, pack3};

use super::GPT::GptInfo;

impl<'a> DAXFlash<'a> {
    /// 读取 GPT 分区表（USB 版本）
    pub fn read_gpt(&mut self) -> Result<(), String> {
        info!("读取 GPT 分区表...");

        // 第一步：先读 32KB，覆盖大多数 GPT 头 + 分区表
        let initial_len: u64 = 32768;
        let mut gpt_data = self.readflash_data(0, initial_len)?;
        if gpt_data.len() < 604 {
            return Err("GPT 头数据不足（需要至少 604 字节）".to_string());
        }

        // 复用 GptInfo::parse 解析 GPT 头（自动搜索 EFI PART 签名定位基址）
        {
            let gpt_info = GptInfo::parse(&gpt_data)?;
            let num_entries = gpt_info.num_part_entries as u64;
            let entry_size = gpt_info.part_entry_size as u64;
            let needed_len = 512 + num_entries * entry_size;

            info!(
                "  GPT 分区数: {}, 分区项大小: {} 字节, 需要: {} 字节",
                num_entries, entry_size, needed_len
            );

            // 第二步：如果 32KB 不够，再扩展读取完整大小
            if needed_len > initial_len {
                gpt_data = self.readflash_data(0, needed_len)?;
            }
        }
        info!("  读取 GPT 数据: {} 字节", gpt_data.len());

        // 保存原始数据供调试模式使用
        self.last_gpt_data = Some(gpt_data.clone());

        // 写入完整原始数据到文件
        std::fs::write("gpt_full.bin", &gpt_data).expect("写 gpt_full.bin 失败");
        info!("已写入 gpt_full.bin, {} 字节", gpt_data.len());

        // 备份 gpt.bin（对齐 mtkclient 行为）
        std::fs::write("gpt.bin", &gpt_data).expect("写 gpt.bin 失败");
        info!("已写入 gpt.bin, {} 字节", gpt_data.len());

        // 解析 GPT
        super::分区表::parse_gpt_from_data(&gpt_data)
    }

    /// 查找分区的物理地址和大小（需要 GPT 数据）
    pub(crate) fn find_partition_addr(&mut self, partition: &str) -> Result<(u64, u64), String> {
        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据，请先运行 printgpt".to_string())?;

        let gpt_info = GptInfo::parse(gpt_data)?;

        if let Some(entry) = gpt_info.find_partition(partition) {
            Ok((entry.start_addr, entry.size))
        } else {
            Err(format!("未找到分区: {}", partition))
        }
    }

    /// 读取分区数据到文件
    pub fn 读取分区(&mut self, 分区名: &str, 输出文件: &str) -> Result<(), String> {
        if self.last_gpt_data.is_none() {
            self.read_gpt()?;
        }
        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据，请先运行 printgpt".to_string())?;

        let gpt_info = GptInfo::parse(gpt_data)?;

        let entry = gpt_info
            .find_partition(分区名)
            .ok_or_else(|| format!("未找到分区: {}", 分区名))?;

        info!(
            "找到分区 {}，起始地址: 0x{:X}，大小: {} 字节",
            分区名, entry.start_addr, entry.size
        );

        let data = self.readflash_data(entry.start_addr, entry.size)?;
        info!("  读取到 {} 字节", data.len());

        let mut file = File::create(输出文件).map_err(|e| format!("创建文件失败: {}", e))?;
        file.write_all(&data)
            .map_err(|e| format!("写入文件失败: {}", e))?;

        info!("  已保存到: {}", 输出文件);
        Ok(())
    }

    /// 写入文件到分区（带校验）
    pub fn 写入分区带校验(
        &mut self, 分区名: &str, 输入文件: &str
    ) -> Result<(), String> {
        // 先写入
        self.写入分区(分区名, 输入文件)?;

        // 读取回来校验
        info!("  开始校验写入数据...");
        let 原始数据 = std::fs::read(输入文件).map_err(|e| format!("读取原始文件失败: {}", e))?;

        // 读取刚写入的数据
        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据".to_string())?;
        let gpt_info = GptInfo::parse(gpt_data)?;
        let entry = gpt_info
            .find_partition(分区名)
            .ok_or_else(|| format!("未找到分区: {}", 分区名))?;

        let 验证数据 = self.readflash_data(entry.start_addr, 原始数据.len() as u64)?;

        if 原始数据 == 验证数据 {
            info!("  校验通过 ✓");
            Ok(())
        } else {
            Err("校验失败：写入数据与原始数据不匹配".to_string())
        }
    }

    /// 按原始地址写入一段数据，供分区写入、seccfg/frp 等场景复用。
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
        }

        let st = self.status()?;
        if st != 0 {
            return Err(format!("writeflash status error: 0x{:08X}", st));
        }

        self.send_devctrl(0x800005, None)?;
        Ok(())
    }

    /// 写入文件到分区
    /// 对齐 Python writeflash (xflash_lib.py:writeflash)
    /// 协议: cmd_write_data → 循环分包写入 [0x0(4B)][checksum(4B)][data] → CC_OPTIONAL_DOWNLOAD_ACT → status
    pub fn 写入分区(&mut self, 分区名: &str, 输入文件: &str) -> Result<(), String> {
        info!("写入文件 {} 到分区 {}...", 输入文件, 分区名);

        // 如果无 GPT 缓存，自动读取 GPT 数据
        if self.last_gpt_data.is_none() {
            self.read_gpt()?;
        }

        // 读取文件
        let 文件数据 = std::fs::read(输入文件).map_err(|e| format!("无法读取文件: {}", e))?;
        let 文件大小 = 文件数据.len();

        // 找到分区地址和大小
        let (地址, _分区大小) = self.find_partition_addr(分区名)?;

        let mut 数据 = 文件数据;
        // 对齐到 512 字节（Python: 如果长度不是 512 的倍数，补零）
        let 填充 = if 数据.len() % 512 != 0 {
            512 - (数据.len() % 512)
        } else {
            0
        };
        if 填充 > 0 {
            数据.resize(数据.len() + 填充, 0);
        }
        self.write_flash_data(地址, &数据, 1, 8)?;

        info!("  写入完成: {} 字节写入分区 {}", 文件大小, 分区名);
        Ok(())
    }

    /// 擦除分区
    /// 对齐 Python formatflash (xflash_lib.py:formatflash)
    /// 协议: FORMAT 命令 → send_param → 等待 STATUS_COMPLETE (0x40040005)
    pub fn 擦除分区(&mut self, 分区名: &str) -> Result<(), String> {
        info!("擦除分区 {}...", 分区名);

        // 如果无 GPT 缓存，自动读取 GPT 数据
        if self.last_gpt_data.is_none() {
            self.read_gpt()?;
        }

        // 找到分区地址和大小
        let (地址, 大小) = self.find_partition_addr(分区名)?;

        // 发送 FORMAT 命令
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&CMD_FORMAT.to_le_bytes())?;

        let st = self.status()?;
        if st != 0 {
            return Err(format!("FORMAT 命令 status error: 0x{:08X}", st));
        }

        // 发送参数: storage(4) + parttype(4) + addr(8) + size(8) + NandExtension(32)
        let mut param = Vec::with_capacity(56);
        param.extend_from_slice(&1u32.to_le_bytes()); // storage = EMMC
        param.extend_from_slice(&8u32.to_le_bytes()); // parttype = USER
        param.extend_from_slice(&地址.to_le_bytes());
        param.extend_from_slice(&大小.to_le_bytes());
        // NandExtension 全零（对齐 Python xflash_flash_param.py）
        param.extend_from_slice(&[0u8; 32]);

        let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        // 等待 STATUS_COMPLETE (0x40040005) 或 STATUS_CONTINUE (0x40040004)
        const STATUS_COMPLETE: u32 = 0x40040005;
        const STATUS_CONTINUE: u32 = 0x40040004;

        let mut status = self.status()?;
        while status == STATUS_CONTINUE {
            // STATUS_CONTINUE 包含等待时间（毫秒）
            let wait_ms = self.status()?;
            std::thread::sleep(std::time::Duration::from_millis(wait_ms as u64));
            let _ = self.ack(); // AckResult 的 Debug 输出已满足日志需求
            status = self.status()?;
        }

        if status != STATUS_COMPLETE {
            return Err(format!("擦除失败: status=0x{:08X}", status));
        }

        info!("  擦除完成: 分区 {} (0x{:X} @ 0x{:X})", 分区名, 大小, 地址);
        Ok(())
    }

    /// 解锁 Bootloader
    /// 对齐 Python mtkclient: unlock 命令
    /// 流程: 读取 seccfg 分区 → 解析 V4/V3 结构 → 修改 lock_state → 重新签名 → 写回
    pub fn unlock_bootloader(&mut self) -> Result<(), String> {
        crate::安全::seccfg::unlock_bootloader(self)
    }

    /// 锁定 Bootloader
    // 预留：unlock/lock 命令使用
    pub fn lock_bootloader(&mut self) -> Result<(), String> {
        crate::安全::seccfg::lock_bootloader(self)
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

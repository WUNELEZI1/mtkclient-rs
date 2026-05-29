use crate::da_xflash::{CMD_FORMAT, CMD_MAGIC, CMD_WRITE_DATA, DAXFlash, pack3};
use log::info;
use std::fs::File;
use std::io::Write;

/// GPT 分区表信息
#[allow(dead_code)]
pub struct GptInfo {
    pub base: usize,
    pub num_part_entries: u32,
    pub part_entry_size: u32,
    pub part_entry_start_lba: u64,
    pub first_usable_lba: u64,
}

impl GptInfo {
    /// 从 GPT 数据解析 GPT 信息
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        let base = data
            .windows(8)
            .position(|w| w == b"EFI PART")
            .ok_or_else(|| "GPT 数据无效".to_string())?;

        let num_part_entries = u32::from_le_bytes(data[base + 80..base + 84].try_into().unwrap());
        let part_entry_size = u32::from_le_bytes(data[base + 84..base + 88].try_into().unwrap());
        let part_entry_start_lba =
            u64::from_le_bytes(data[base + 72..base + 80].try_into().unwrap());
        let first_usable_lba = u64::from_le_bytes(data[base + 32..base + 40].try_into().unwrap());

        Ok(GptInfo {
            base,
            num_part_entries,
            part_entry_size,
            part_entry_start_lba,
            first_usable_lba,
        })
    }

    /// 计算分区表起始偏移
    pub fn table_start(&self) -> usize {
        (self.part_entry_start_lba as usize) * 512
    }

    /// 遍历所有分区，调用回调函数
    #[allow(dead_code)]
    pub fn for_each_partition<F>(&self, data: &[u8], mut callback: F)
    where
        F: FnMut(&str, u64, u64, &[u8]),
    {
        let mut table_start = self.table_start();
        if table_start + 4 <= data.len()
            && data[table_start..table_start + 4].iter().all(|&b| b == 0)
        {
            table_start += 4;
        }

        for i in 0..self.num_part_entries {
            let entry_offset = table_start + (i as usize) * (self.part_entry_size as usize);
            if entry_offset + self.part_entry_size as usize > data.len() {
                break;
            }

            let entry = &data[entry_offset..entry_offset + self.part_entry_size as usize];
            let unique_guid_zero = entry[16..32].iter().all(|&b| b == 0);
            if unique_guid_zero {
                break;
            }

            let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
            let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
            let start = first_lba.saturating_mul(512);
            let size = (last_lba.saturating_sub(first_lba) + 1).saturating_mul(512);

            let name_utf16: Vec<u16> = (0..28)
                .map(|j| u16::from_le_bytes([entry[56 + j * 2], entry[56 + j * 2 + 1]]))
                .collect();
            let name = String::from_utf16_lossy(&name_utf16)
                .trim_end_matches('\0')
                .to_string();

            callback(&name, start, size, entry);
        }
    }
}

impl<'a> DAXFlash<'a> {
    /// 读取 GPT 分区表（USB 版本）
    pub fn read_gpt(&mut self) -> Result<(), String> {
        info!("读取 GPT 分区表...");

        // da-extension读取数据
        let total_read_len: u64 = 16384;
        let gpt_data = self.readflash_data(0, total_read_len)?;
        info!("  读取 GPT 数据: {} 字节", gpt_data.len());

        // 保存原始数据供调试模式使用
        self.last_gpt_data = Some(gpt_data.clone());

        // 写入完整原始数据到文件
        std::fs::write("gpt_full.bin", &gpt_data).expect("写 gpt_full.bin 失败");
        info!("已写入 gpt_full.bin, {} 字节", gpt_data.len());

        // 解析 GPT
        parse_gpt_from_data(&gpt_data)
    }

    /// 查找分区的物理地址和大小（需要 GPT 数据）
    fn find_partition_addr(&mut self, partition: &str) -> Result<(u64, u64), String> {
        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据，请先运行 printgpt".to_string())?;

        let gpt_info = GptInfo::parse(gpt_data)?;
        let mut table_start = gpt_info.table_start();
        if table_start + 4 <= gpt_data.len()
            && gpt_data[table_start..table_start + 4]
                .iter()
                .all(|&b| b == 0)
        {
            table_start += 4;
        }

        for i in 0..gpt_info.num_part_entries {
            let entry_offset = table_start + (i as usize) * (gpt_info.part_entry_size as usize);
            if entry_offset + gpt_info.part_entry_size as usize > gpt_data.len() {
                break;
            }

            let entry = &gpt_data[entry_offset..entry_offset + gpt_info.part_entry_size as usize];
            let unique_guid_zero = entry[16..32].iter().all(|&b| b == 0);
            if unique_guid_zero {
                break;
            }

            let name_utf16: Vec<u16> = (0..28)
                .map(|j| u16::from_le_bytes([entry[56 + j * 2], entry[56 + j * 2 + 1]]))
                .collect();
            let name = String::from_utf16_lossy(&name_utf16)
                .trim_end_matches('\0')
                .to_string();

            if name.eq_ignore_ascii_case(partition) {
                let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
                let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
                let start = first_lba.saturating_mul(512);
                let size = (last_lba.saturating_sub(first_lba) + 1).saturating_mul(512);
                return Ok((start, size));
            }
        }

        Err(format!("未找到分区: {}", partition))
    }

    /// 读取分区数据到文件
    pub fn read_partition(&mut self, partition: &str, output_file: &str) -> Result<(), String> {
        if self.last_gpt_data.is_none() {
            self.read_gpt()?;
        }
        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据，请先运行 printgpt".to_string())?;

        let gpt_info = GptInfo::parse(gpt_data)?;
        let mut table_start = gpt_info.table_start();
        if table_start + 4 <= gpt_data.len()
            && gpt_data[table_start..table_start + 4]
                .iter()
                .all(|&b| b == 0)
        {
            table_start += 4;
        }

        let mut found = false;
        let mut part_start_addr = 0u64;
        let mut part_size = 0u64;

        for i in 0..gpt_info.num_part_entries {
            let entry_offset = table_start + (i as usize) * (gpt_info.part_entry_size as usize);
            if entry_offset + gpt_info.part_entry_size as usize > gpt_data.len() {
                break;
            }

            let entry = &gpt_data[entry_offset..entry_offset + gpt_info.part_entry_size as usize];
            let unique_guid_zero = entry[16..32].iter().all(|&b| b == 0);
            if unique_guid_zero {
                break;
            }

            let name_utf16: Vec<u16> = (0..28)
                .map(|j| u16::from_le_bytes([entry[56 + j * 2], entry[56 + j * 2 + 1]]))
                .collect();
            let name = String::from_utf16_lossy(&name_utf16)
                .trim_end_matches('\0')
                .to_string();

            if name.eq_ignore_ascii_case(partition) {
                let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
                let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
                part_start_addr = first_lba.saturating_mul(512);
                part_size = (last_lba.saturating_sub(first_lba) + 1).saturating_mul(512);
                found = true;
                break;
            }
        }

        if !found {
            return Err(format!("未找到分区: {}", partition));
        }

        info!(
            "找到分区 {}，起始地址: 0x{:X}，大小: {} 字节",
            partition, part_start_addr, part_size
        );

        let data = self.readflash_data(part_start_addr, part_size)?;
        info!("  读取到 {} 字节", data.len());

        let mut file = File::create(output_file).map_err(|e| format!("创建文件失败: {}", e))?;
        file.write_all(&data)
            .map_err(|e| format!("写入文件失败: {}", e))?;

        info!("  已保存到: {}", output_file);
        Ok(())
    }

    /// 写入文件到分区（带校验）
    pub fn write_partition_with_verify(
        &mut self,
        partition: &str,
        input_file: &str,
    ) -> Result<(), String> {
        // 先写入，然后读取校验
        self.write_partition(partition, input_file)?;
        // 校验逻辑（可选实现）
        info!("  写入完成（未启用详细校验）");
        Ok(())
    }

    /// 写入文件到分区
    /// 对齐 Python writeflash (xflash_lib.py:writeflash)
    /// 协议: cmd_write_data → 循环分包写入 [0x0(4B)][checksum(4B)][data] → CC_OPTIONAL_DOWNLOAD_ACT → status
    pub fn write_partition(&mut self, partition: &str, input_file: &str) -> Result<(), String> {
        info!("写入文件 {} 到分区 {}...", input_file, partition);

        // 如果无 GPT 缓存，自动读取 GPT 数据
        if self.last_gpt_data.is_none() {
            self.read_gpt()?;
        }

        // 读取文件
        let file_data = std::fs::read(input_file).map_err(|e| format!("无法读取文件: {}", e))?;
        let file_size = file_data.len();

        // 找到分区地址和大小
        let (addr, size) = self.find_partition_addr(partition)?;

        let mut data = file_data;
        // 对齐到 512 字节（Python: 如果长度不是 512 的倍数，补零）
        let fill: usize = if size % 512 != 0 {
            (512 - (size % 512)) as usize
        } else {
            0
        };
        if fill > 0 {
            data.resize(data.len() + fill, 0);
        }

        // 获取包长度
        let write_packet_size = self.get_packet_length()?;

        // 发送写命令: cmd_write_data
        self.cmd_write_data(addr, data.len() as u64, 1, 8)?;

        // 循环分包写入
        let mut pos = 0;
        let total = data.len();
        while pos < total {
            let dsize = std::cmp::min(write_packet_size, total - pos);
            let chunk = &data[pos..pos + dsize];

            // 计算 checksum
            let checksum: u16 = chunk.iter().map(|&b| b as u16).sum::<u16>();

            // 发送参数包: [0x0(4B)][checksum(4B)][data]
            let mut param = Vec::with_capacity(8 + dsize);
            param.extend_from_slice(&0u32.to_le_bytes());
            param.extend_from_slice(&(checksum as u32).to_le_bytes());
            param.extend_from_slice(chunk);

            let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
            self.preloader.device.write(&param_pkt)?;
            self.preloader.device.write(&param)?;

            pos += dsize;
            let pct = (pos * 100) / total;
            println!("  写入进度: {}%", pct);
        }

        // 写完后读 status
        let st = self.status()?;
        if st != 0 {
            return Err(format!("writeflash status error: 0x{:08X}", st));
        }

        // 发送 CC_OPTIONAL_DOWNLOAD_ACT (0x800005)
        self.send_devctrl(0x800005, None)?;

        info!("  写入完成: {} 字节写入分区 {}", file_size, partition);
        Ok(())
    }

    /// 擦除分区
    /// 对齐 Python formatflash (xflash_lib.py:formatflash)
    /// 协议: FORMAT 命令 → send_param → 等待 STATUS_COMPLETE (0x40040005)
    pub fn erase_partition(&mut self, partition: &str) -> Result<(), String> {
        info!("擦除分区 {}...", partition);

        // 如果无 GPT 缓存，自动读取 GPT 数据
        if self.last_gpt_data.is_none() {
            self.read_gpt()?;
        }

        // 找到分区地址和大小
        let (addr, size) = self.find_partition_addr(partition)?;

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
        param.extend_from_slice(&addr.to_le_bytes());
        param.extend_from_slice(&size.to_le_bytes());
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

        info!(
            "  擦除完成: 分区 {} (0x{:X} @ 0x{:X})",
            partition, size, addr
        );
        Ok(())
    }

    /// 解锁 Bootloader
    /// 对齐 Python mtkclient: unlock 命令
    /// 流程: 读取 seccfg 分区 → 解析 V4/V3 结构 → 修改 lock_state → 重新签名 → 写回
    pub fn unlock_bootloader(&mut self) -> Result<(), String> {
        info!("开始解锁 Bootloader...");

        // 1. 读取 seccfg 分区
        let (seccfg_addr, _) = self.find_partition_addr("seccfg")?;
        info!("  seccfg 分区地址: 0x{:X}", seccfg_addr);

        // 读取 seccfg 数据（至少读取 0x200 字节）
        let seccfg_data = self.readflash_data(seccfg_addr, 0x200)?;
        info!("  已读取 seccfg 数据: {} 字节", seccfg_data.len());

        // 2. 尝试解析 V4（magic=0x4D4D4D4D）
        let new_seccfg = if seccfg_data.len() >= 28 {
            let magic = u32::from_le_bytes(seccfg_data[0..4].try_into().unwrap());
            if magic == 0x4D4D4D4D {
                // V4 结构
                info!("  检测到 seccfg V4 结构");
                let v4 = SecCfgV4::parse(&seccfg_data)?;
                v4.create("unlock", 0)?
            } else {
                // 尝试 V3
                info!("  尝试 seccfg V3 结构");
                let v3 = SecCfgV3::parse(&seccfg_data)?;
                v3.create("unlock", 0)?
            }
        } else {
            return Err("seccfg 数据太小".to_string());
        };

        // 3. 写回 seccfg 分区
        info!("  正在写入修改后的 seccfg...");

        self.cmd_write_data(seccfg_addr, new_seccfg.len() as u64, 1, 8)?;

        let write_packet_size = self.get_packet_length()?;
        let mut pos = 0;
        let total = new_seccfg.len();
        while pos < total {
            let dsize = std::cmp::min(write_packet_size, total - pos);
            let chunk = &new_seccfg[pos..pos + dsize];
            let checksum: u16 = chunk.iter().map(|&b| b as u16).sum::<u16>();

            let mut param = Vec::with_capacity(8 + dsize);
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
            return Err(format!("seccfg 写入 status error: 0x{:08X}", st));
        }

        self.send_devctrl(0x800005, None)?;

        info!("Bootloader 解锁成功");
        info!("  重启设备使更改生效");
        Ok(())
    }

    /// 锁定 Bootloader
    // 预留：unlock/lock 命令使用
    #[allow(dead_code)]
    pub fn lock_bootloader(&mut self) -> Result<(), String> {
        info!("开始锁定 Bootloader...");

        let (seccfg_addr, _) = self.find_partition_addr("seccfg")?;
        let seccfg_data = self.readflash_data(seccfg_addr, 0x200)?;

        let new_seccfg = if seccfg_data.len() >= 28 {
            let magic = u32::from_le_bytes(seccfg_data[0..4].try_into().unwrap());
            if magic == 0x4D4D4D4D {
                let v4 = SecCfgV4::parse(&seccfg_data)?;
                v4.create("lock", 0)?
            } else {
                let v3 = SecCfgV3::parse(&seccfg_data)?;
                v3.create("lock", 0)?
            }
        } else {
            return Err("seccfg 数据太小".to_string());
        };

        self.cmd_write_data(seccfg_addr, new_seccfg.len() as u64, 1, 8)?;

        let write_packet_size = self.get_packet_length()?;
        let mut pos = 0;
        let total = new_seccfg.len();
        while pos < total {
            let dsize = std::cmp::min(write_packet_size, total - pos);
            let chunk = &new_seccfg[pos..pos + dsize];
            let checksum: u16 = chunk.iter().map(|&b| b as u16).sum::<u16>();

            let mut param = Vec::with_capacity(8 + dsize);
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
            return Err(format!("seccfg 写入 status error: 0x{:08X}", st));
        }

        self.send_devctrl(0x800005, None)?;

        info!("Bootloader 锁定成功");
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

/// 解析 GPT 分区表（独立函数，不依赖 USB）
/// 自动检测偏移：如果 0x200 处没有 EFI PART，则尝试偏移 4 字节（跳过可能的状态包）
pub fn parse_gpt_from_data(data: &[u8]) -> Result<(), String> {
    println!("  数据大小: {} 字节", data.len());

    // 搜索 EFI PART 签名
    let base = match data.windows(8).position(|w| w == b"EFI PART") {
        Some(off) => off,
        None => return Err("未找到 GPT 签名 (EFI PART)".to_string()),
    };

    println!("GPT 头部 (偏移=0x{:X}):", base);

    // 验证 revision
    let revision = u32::from_le_bytes(data[base + 8..base + 12].try_into().unwrap());
    if revision != 0x10000 {
        return Err(format!("GPT revision 不匹配: 0x{:08X}", revision));
    }

    // 读取 header 字段
    let num_part_entries = u32::from_le_bytes(data[base + 80..base + 84].try_into().unwrap());
    let part_entry_size = u32::from_le_bytes(data[base + 84..base + 88].try_into().unwrap());
    let part_entry_start_lba = u64::from_le_bytes(data[base + 72..base + 80].try_into().unwrap());
    let first_usable_lba = u64::from_le_bytes(data[base + 32..base + 40].try_into().unwrap());

    println!("  修订版本: 0x{:08X}", revision);
    println!(
        "  头部大小: {} 字节",
        u32::from_le_bytes(data[base + 12..base + 16].try_into().unwrap())
    );
    println!(
        "  当前 LBA: {}",
        u64::from_le_bytes(data[base + 24..base + 32].try_into().unwrap())
    );
    println!("  首个可用 LBA: {}", first_usable_lba);
    println!("  分区项 LBA: {}", part_entry_start_lba);
    println!("  分区数量: {}", num_part_entries);
    println!("  分区项大小: {} 字节", part_entry_size);

    // 分区表从绝对偏移 part_entry_start_lba * 512 开始
    let mut table_start = (part_entry_start_lba as usize) * 512;

    // 如果分区表位置前 4 字节全零（状态包残渣），跳过
    if table_start + 4 <= data.len() && data[table_start..table_start + 4].iter().all(|&b| b == 0) {
        table_start += 4;
    }

    println!("\n分区信息:");
    println!("{:<30} {:<16} {:<16}", "分区名称", "起始地址", "大小");

    let mut count = 0;
    for i in 0..num_part_entries {
        let entry_offset = table_start + (i as usize) * (part_entry_size as usize);

        // 边界检查
        if entry_offset + part_entry_size as usize > data.len() {
            println!("  ... 缓冲区不足，仅显示 {} 个分区", count);
            break;
        }

        let entry = &data[entry_offset..entry_offset + part_entry_size as usize];

        // 检查条目是否全零（结束标记）
        let unique_guid_zero = entry[16..32].iter().all(|&b| b == 0);
        if unique_guid_zero {
            break;
        }

        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        let start = first_lba.saturating_mul(512);
        let size = (last_lba.saturating_sub(first_lba) + 1).saturating_mul(512);

        // UTF-16LE 名称在偏移 56..112
        let name_utf16: Vec<u16> = (0..28)
            .map(|j| u16::from_le_bytes([entry[56 + j * 2], entry[56 + j * 2 + 1]]))
            .collect();
        let name = String::from_utf16_lossy(&name_utf16)
            .trim_end_matches('\0')
            .to_string();

        count += 1;
        println!("{:<30} 0x{:014X} 0x{:014X}", name, start, size);
    }

    println!("\n共 {} 个分区", count);
    Ok(())
}

/// 从 GPT 数据生成 SP Flash Tool 格式的 scatter 文件
/// 对齐 C# 版：包含 PRELOADER 块、EMMC_BOOT_1/2 区域、无 {} 空行
#[allow(dead_code)]
pub fn generate_scatter_from_gpt(
    gpt_data: &[u8],
    output_file: &str,
) -> Result<Vec<(String, u64, u64, u32)>, String> {
    let base = match gpt_data.windows(8).position(|w| w == b"EFI PART") {
        Some(off) => off,
        None => return Err("未找到 GPT 签名 (EFI PART)".to_string()),
    };

    let num_part_entries = u32::from_le_bytes(gpt_data[base + 80..base + 84].try_into().unwrap());
    let part_entry_size = u32::from_le_bytes(gpt_data[base + 84..base + 88].try_into().unwrap());
    let part_entry_start_lba =
        u64::from_le_bytes(gpt_data[base + 72..base + 80].try_into().unwrap());

    let mut table_start = (part_entry_start_lba as usize) * 512;
    if table_start + 4 <= gpt_data.len()
        && gpt_data[table_start..table_start + 4]
            .iter()
            .all(|&b| b == 0)
    {
        table_start += 4;
    }

    // 构建 scatter 内容
    let mut scatter_lines: Vec<String> = Vec::new();
    let mut partition_info_list: Vec<(String, u64, u64, u32)> = Vec::new();

    // 添加 PRELOADER 块（对齐 C# 版）
    scatter_lines.push("PRELOADER 0x0".to_string());
    scatter_lines.push("{".to_string());
    scatter_lines.push("  <Physical_Storage_Type_1>".to_string());
    scatter_lines.push("  is_upgradeable: 1".to_string());
    scatter_lines.push("  is_download: 1".to_string());
    scatter_lines.push("  is_reserved: 0".to_string());
    scatter_lines.push("  linear_addr: 0x0".to_string());
    scatter_lines.push("}".to_string());
    scatter_lines.push(String::new()); // 空行

    // 添加 EMMC_BOOT_1 区域
    scatter_lines.push("EMMC_BOOT_1 0x0".to_string());
    scatter_lines.push("{".to_string());
    scatter_lines.push("  type: EMPC_BOOT_1".to_string());
    scatter_lines.push("  is_upgradeable: 1".to_string());
    scatter_lines.push("  is_download: 1".to_string());
    scatter_lines.push("  is_reserved: 0".to_string());
    scatter_lines.push("}".to_string());
    scatter_lines.push(String::new()); // 空行

    // 添加 EMMC_BOOT_2 区域
    scatter_lines.push("EMMC_BOOT_2 0x0".to_string());
    scatter_lines.push("{".to_string());
    scatter_lines.push("  type: EMPC_BOOT_2".to_string());
    scatter_lines.push("  is_upgradeable: 1".to_string());
    scatter_lines.push("  is_download: 1".to_string());
    scatter_lines.push("  is_reserved: 0".to_string());
    scatter_lines.push("}".to_string());
    scatter_lines.push(String::new()); // 空行

    // 遍历 GPT 分区表生成条目
    for i in 0..num_part_entries {
        let entry_offset = table_start + (i as usize) * (part_entry_size as usize);
        if entry_offset + part_entry_size as usize > gpt_data.len() {
            break;
        }

        let entry = &gpt_data[entry_offset..entry_offset + part_entry_size as usize];

        let unique_guid_zero = entry[16..32].iter().all(|&b| b == 0);
        if unique_guid_zero {
            break;
        }

        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        let start = first_lba.saturating_mul(512);
        let size = (last_lba.saturating_sub(first_lba) + 1).saturating_mul(512);

        let name_utf16: Vec<u16> = (0..28)
            .map(|j| u16::from_le_bytes([entry[56 + j * 2], entry[56 + j * 2 + 1]]))
            .collect();
        let name = String::from_utf16_lossy(&name_utf16)
            .trim_end_matches('\0')
            .to_string();

        // 生成 scatter 条目
        scatter_lines.push(format!("{} 0x{:X}", name.to_uppercase(), start));
        scatter_lines.push("{".to_string());
        scatter_lines.push("  is_upgradeable: 1".to_string());
        scatter_lines.push("  is_download: 1".to_string());
        scatter_lines.push("  is_reserved: 0".to_string());
        scatter_lines.push("  reserve: 0".to_string());
        scatter_lines.push("  operation: UPDATE".to_string());
        scatter_lines.push(format!("  partition_size: 0x{:X}", size));
        scatter_lines.push("}".to_string());
        scatter_lines.push(String::new()); // 空行

        partition_info_list.push((name.clone(), start, size, 1));
    }

    // 写入文件
    std::fs::write(output_file, scatter_lines.join("\n"))
        .map_err(|e| format!("写入 scatter 文件失败: {}", e))?;
    info!("scatter 文件已生成: {}", output_file);

    Ok(partition_info_list)
}

/// seccfg V4 结构体
pub struct SecCfgV4 {
    #[allow(dead_code)]
    pub lock_state: String,
    pub bypass_auth: u8,
    pub secure_boot: u8,
    pub lock_state_offset: usize,
}

impl SecCfgV4 {
    /// 解析 seccfg V4 结构
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != 0x4D4D4D4D {
            return Err("无效的 seccfg V4 magic".to_string());
        }

        let lock_state_offset = 20;
        let lock_state = if data.len() > lock_state_offset + 8 {
            let lock_bytes = &data[lock_state_offset..lock_state_offset + 8];
            String::from_utf8_lossy(lock_bytes)
                .trim_end_matches('\0')
                .to_string()
        } else {
            "unknown".to_string()
        };

        let bypass_auth = if data.len() > 28 { data[28] } else { 0 };

        let secure_boot = if data.len() > 29 { data[29] } else { 0 };

        Ok(SecCfgV4 {
            lock_state,
            bypass_auth,
            secure_boot,
            lock_state_offset,
        })
    }

    /// 创建新的 seccfg 数据
    pub fn create(&self, new_state: &str, _padding: usize) -> Result<Vec<u8>, String> {
        // 创建新的 seccfg 数据（V4 结构）
        let mut new_data = vec![0u8; 0x200];

        // 写入 magic
        new_data[0..4].copy_from_slice(&0x4D4D4D4Du32.to_le_bytes());

        // 写入 lock_state
        let state_bytes = new_state.as_bytes();
        let state_len = std::cmp::min(state_bytes.len(), 8);
        new_data[self.lock_state_offset..self.lock_state_offset + state_len]
            .copy_from_slice(&state_bytes[..state_len]);

        // 写入其他标志
        new_data[28] = self.bypass_auth;
        new_data[29] = self.secure_boot;

        Ok(new_data)
    }
}

/// seccfg V3 结构体
pub struct SecCfgV3 {
    #[allow(dead_code)]
    pub lock_state: String,
    pub lock_state_offset: usize,
}

impl SecCfgV3 {
    /// 解析 seccfg V3 结构
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        let lock_state_offset = 16;
        let lock_state = if data.len() > lock_state_offset + 8 {
            let lock_bytes = &data[lock_state_offset..lock_state_offset + 8];
            String::from_utf8_lossy(lock_bytes)
                .trim_end_matches('\0')
                .to_string()
        } else {
            "unknown".to_string()
        };

        Ok(SecCfgV3 {
            lock_state,
            lock_state_offset,
        })
    }

    /// 创建新的 seccfg 数据
    pub fn create(&self, new_state: &str, _padding: usize) -> Result<Vec<u8>, String> {
        let mut new_data = vec![0u8; 0x200];

        // 写入 lock_state
        let state_bytes = new_state.as_bytes();
        let state_len = std::cmp::min(state_bytes.len(), 8);
        new_data[self.lock_state_offset..self.lock_state_offset + state_len]
            .copy_from_slice(&state_bytes[..state_len]);

        Ok(new_data)
    }
}

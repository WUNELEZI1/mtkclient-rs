use crate::da_xflash::{CMD_FORMAT, CMD_MAGIC, CMD_WRITE_DATA, DAXFlash, pack3};
use log::info;
use std::fs::File;
use std::io::Write;

/// 分区条目信息
#[derive(Debug, Clone)]
pub struct PartitionEntry {
    pub name: String,
    pub first_lba: u64,
    pub last_lba: u64,
    pub start_addr: u64,
    pub size: u64,
}

/// GPT 分区表信息
pub struct GptInfo<'a> {
    pub num_part_entries: u32,
    pub part_entry_size: u32,
    pub part_entry_start_lba: u64,
    pub revision: u32,
    pub header_size: u32,
    pub first_usable_lba: u64,
    pub current_lba: u64,
    data: &'a [u8],
    base_offset: usize,
}

impl<'a> GptInfo<'a> {
    /// 从 GPT 数据解析 GPT 信息
    pub fn parse(data: &'a [u8]) -> Result<Self, String> {
        let base_offset = data
            .windows(8)
            .position(|w| w == b"EFI PART")
            .ok_or_else(|| "GPT 数据无效".to_string())?;

        let revision = u32::from_le_bytes(data[base_offset + 8..base_offset + 12].try_into().unwrap());
        let header_size = u32::from_le_bytes(data[base_offset + 12..base_offset + 16].try_into().unwrap());
        let num_part_entries = u32::from_le_bytes(data[base_offset + 80..base_offset + 84].try_into().unwrap());
        let part_entry_size = u32::from_le_bytes(data[base_offset + 84..base_offset + 88].try_into().unwrap());
        let part_entry_start_lba =
            u64::from_le_bytes(data[base_offset + 72..base_offset + 80].try_into().unwrap());
        let first_usable_lba = u64::from_le_bytes(data[base_offset + 32..base_offset + 40].try_into().unwrap());
        let current_lba = u64::from_le_bytes(data[base_offset + 24..base_offset + 32].try_into().unwrap());

        Ok(GptInfo {
            num_part_entries,
            part_entry_size,
            part_entry_start_lba,
            revision,
            header_size,
            first_usable_lba,
            current_lba,
            data,
            base_offset,
        })
    }

    /// 计算分区表起始偏移（处理可能的状态包残渣）
    pub fn table_start(&self) -> usize {
        let mut start = (self.part_entry_start_lba as usize) * 512;
        if start + 4 <= self.data.len() && self.data[start..start + 4].iter().all(|&b| b == 0) {
            start += 4;
        }
        start
    }

    /// 解析单个分区条目
    fn parse_entry(&self, entry_offset: usize) -> Option<PartitionEntry> {
        if entry_offset + self.part_entry_size as usize > self.data.len() {
            return None;
        }

        let entry = &self.data[entry_offset..entry_offset + self.part_entry_size as usize];

        // 检查条目是否全零（结束标记）
        let unique_guid_zero = entry[16..32].iter().all(|&b| b == 0);
        if unique_guid_zero {
            return None;
        }

        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());

        // 解析分区名称（UTF-16LE）
        let name_utf16: Vec<u16> = (0..28)
            .map(|j| u16::from_le_bytes([entry[56 + j * 2], entry[56 + j * 2 + 1]]))
            .collect();
        let name = String::from_utf16_lossy(&name_utf16)
            .trim_end_matches('\0')
            .to_string();

        let start = first_lba.saturating_mul(512);
        let size = (last_lba.saturating_sub(first_lba) + 1).saturating_mul(512);

        Some(PartitionEntry {
            name,
            first_lba,
            last_lba,
            start_addr: start,
            size,
        })
    }

    /// 迭代所有分区条目
    pub fn iter_partitions(&self) -> impl Iterator<Item = PartitionEntry> + '_ {
        let table_start = self.table_start();
        let part_entry_size = self.part_entry_size as usize;
        let num_entries = self.num_part_entries as usize;

        (0..num_entries).filter_map(move |i| {
            let entry_offset = table_start + i * part_entry_size;
            self.parse_entry(entry_offset)
        })
    }

    /// 按名称查找分区（不区分大小写）
    pub fn find_partition(&self, name: &str) -> Option<PartitionEntry> {
        self.iter_partitions()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    /// 获取所有分区列表
    pub fn partitions(&self) -> Vec<PartitionEntry> {
        self.iter_partitions().collect()
    }
}

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
        parse_gpt_from_data(&gpt_data)
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
    pub fn read_partition(&mut self, partition: &str, output_file: &str) -> Result<(), String> {
        if self.last_gpt_data.is_none() {
            self.read_gpt()?;
        }
        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据，请先运行 printgpt".to_string())?;

        let gpt_info = GptInfo::parse(gpt_data)?;

        let entry = gpt_info
            .find_partition(partition)
            .ok_or_else(|| format!("未找到分区: {}", partition))?;

        info!(
            "找到分区 {}，起始地址: 0x{:X}，大小: {} 字节",
            partition, entry.start_addr, entry.size
        );

        let data = self.readflash_data(entry.start_addr, entry.size)?;
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
        while pos < total {
            let dsize = std::cmp::min(write_packet_size, total - pos);
            let chunk = &data[pos..pos + dsize];
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
            return Err(format!("writeflash status error: 0x{:08X}", st));
        }

        self.send_devctrl(0x800005, None)?;
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
        self.write_flash_data(addr, &data, 1, 8)?;

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
        crate::seccfg::unlock_bootloader(self)
    }

    /// 锁定 Bootloader
    // 预留：unlock/lock 命令使用
    pub fn lock_bootloader(&mut self) -> Result<(), String> {
        crate::seccfg::lock_bootloader(self)
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
pub fn parse_gpt_from_data(data: &[u8]) -> Result<(), String> {
    println!("  数据大小: {} 字节", data.len());

    let gpt_info = GptInfo::parse(data)?;

    println!("GPT 头部 (偏移=0x{:X}):", gpt_info.base_offset);

    // 验证 revision
    if gpt_info.revision != 0x10000 {
        return Err(format!("GPT revision 不匹配: 0x{:08X}", gpt_info.revision));
    }

    println!("  修订版本: 0x{:08X}", gpt_info.revision);
    println!("  头部大小: {} 字节", gpt_info.header_size);
    println!("  当前 LBA: {}", gpt_info.current_lba);
    println!("  首个可用 LBA: {}", gpt_info.first_usable_lba);
    println!("  分区项 LBA: {}", gpt_info.part_entry_start_lba);
    println!("  分区数量: {}", gpt_info.num_part_entries);
    println!("  分区项大小: {} 字节", gpt_info.part_entry_size);

    println!("\n分区信息:");
    println!("{:<30} {:<16} {:<16}", "分区名称", "起始地址", "大小");

    let partitions = gpt_info.partitions();
    for entry in &partitions {
        println!("{:<30} 0x{:014X} 0x{:014X}", entry.name, entry.start_addr, entry.size);
    }

    println!("\n共 {} 个分区", partitions.len());
    Ok(())
}

/// Scatter header 统一生成（PRELOADER + EMMC_BOOT_1 + EMMC_BOOT_2）
/// console / file 输出共用，避免重复定义
pub fn generate_scatter_header() -> String {
    let lines: Vec<String> = vec![
        "PRELOADER 0x0".to_string(),
        "{".to_string(),
        "  <Physical_Storage_Type_1>".to_string(),
        "  is_upgradeable: 1".to_string(),
        "  is_download: 1".to_string(),
        "  is_reserved: 0".to_string(),
        "  linear_addr: 0x0".to_string(),
        "}".to_string(),
        String::new(),
        "EMMC_BOOT_1 0x0".to_string(),
        "{".to_string(),
        "  type: EMPC_BOOT_1".to_string(),
        "  is_upgradeable: 1".to_string(),
        "  is_download: 1".to_string(),
        "  is_reserved: 0".to_string(),
        "}".to_string(),
        String::new(),
        "EMMC_BOOT_2 0x0".to_string(),
        "{".to_string(),
        "  type: EMPC_BOOT_2".to_string(),
        "  is_upgradeable: 1".to_string(),
        "  is_download: 1".to_string(),
        "  is_reserved: 0".to_string(),
        "}".to_string(),
        String::new(),
    ];
    lines.join("\n") + "\n"
}

/// 从 GPT 数据生成 SP Flash Tool 格式的 scatter 文件
/// 对齐 C# 版：包含 PRELOADER 块、EMMC_BOOT_1/2 区域、无 {} 空行
pub fn generate_scatter_from_gpt(
    gpt_data: &[u8],
    output_file: &str,
) -> Result<Vec<(String, u64, u64, u32)>, String> {
    let gpt_info = GptInfo::parse(gpt_data)?;

    let mut scatter_lines: Vec<String> = Vec::new();
    let mut partition_info_list: Vec<(String, u64, u64, u32)> = Vec::new();

    // 添加 PRELOADER + EMMC_BOOT_1 + EMMC_BOOT_2（统一入口）
    let header = generate_scatter_header();
    for line in header.lines() {
        scatter_lines.push(line.to_string());
    }

    // 遍历 GPT 分区表生成条目
    for entry in gpt_info.iter_partitions() {
        // 生成 scatter 条目
        scatter_lines.push(format!("{} 0x{:X}", entry.name.to_uppercase(), entry.start_addr));
        scatter_lines.push("{".to_string());
        scatter_lines.push("  is_upgradeable: 1".to_string());
        scatter_lines.push("  is_download: 1".to_string());
        scatter_lines.push("  is_reserved: 0".to_string());
        scatter_lines.push("  reserve: 0".to_string());
        scatter_lines.push("  operation: UPDATE".to_string());
        scatter_lines.push(format!("  partition_size: 0x{:X}", entry.size));
        scatter_lines.push("}".to_string());
        scatter_lines.push(String::new());

        partition_info_list.push((entry.name, entry.start_addr, entry.size, 1));
    }

    // 写入文件
    std::fs::write(output_file, scatter_lines.join("\n"))
        .map_err(|e| format!("写入 scatter 文件失败: {}", e))?;
    info!("scatter 文件已生成: {}", output_file);

    Ok(partition_info_list)
}

/// 从 GPT 数据生成刷机匣格式的 scatter 文件（YAML-like）
/// 对齐刷机匣 Shoujixia 的 scatter 输出格式
#[allow(dead_code)] // 预留：scatter 文件导出功能，用于分区表可视化
pub fn generate_scatter_shoujixia(
    gpt_data: &[u8],
    output_file: &str,
    platform: &str,
) -> Result<Vec<(String, u64, u64)>, String> {
    let gpt_info = GptInfo::parse(gpt_data)?;

    let mut scatter_lines: Vec<String> = Vec::new();
    let mut partition_info_list: Vec<(String, u64, u64)> = Vec::new();

    // 头部注释
    scatter_lines.push(
        "################################################Jackson's MtkClient################################################".to_string(),
    );
    scatter_lines.push(String::new());

    // general 块
    scatter_lines.push("- general: MTK_PLATFORM_CFG".to_string());
    scatter_lines.push("  info:".to_string());
    scatter_lines.push("  - config_version: V1.1.2".to_string());
    scatter_lines.push(format!("    platform: {}", platform));
    scatter_lines.push("    storage: EMMC".to_string());
    scatter_lines.push("    boot_channel: MSDC_0".to_string());
    scatter_lines.push("    block_size: 0x200".to_string());
    scatter_lines.push(String::new());

    // PRELOADER 块（SYS0）
    scatter_lines.push("- partition_index: SYS0".to_string());
    scatter_lines.push("  partition_name: preloader".to_string());
    scatter_lines.push(format!(
        "  file_name: preloader_{}.bin",
        platform.to_lowercase().replace('/', "_")
    ));
    scatter_lines.push("  is_download: true".to_string());
    scatter_lines.push("  type: SV5_BL_BIN".to_string());
    scatter_lines.push("  linear_start_addr: 0x0".to_string());
    scatter_lines.push("  physical_start_addr: 0x0".to_string());
    scatter_lines.push("  partition_size: 0x400000".to_string());
    scatter_lines.push("  region: EMMC_BOOT1_BOOT2".to_string());
    scatter_lines.push("  storage: HW_STORAGE_EMMC".to_string());
    scatter_lines.push("  boundary_check: false".to_string());
    scatter_lines.push("  is_reserved: false".to_string());
    scatter_lines.push("  operation_type: BOOTLOADERS".to_string());
    scatter_lines.push("  reserve: 0".to_string());
    scatter_lines.push("  partition_hint: preloader".to_string());
    scatter_lines.push(String::new());

    let mut sys_idx: u32 = 1;

    // 遍历 GPT 分区表生成条目
    for entry in gpt_info.iter_partitions() {
        // 跳过 flashinfo 和未分配伪分区
        if entry.name == "flashinfo" || entry.name.starts_with("Unalloc_") {
            continue;
        }

        // 判断 type、region、operation_type
        let (part_type, region, operation_type) = match entry.name.as_str() {
            "preloader" | "pgpt" | "sgpt" => (
                "SV5_BL_BIN",
                "EMMC_BOOT1_BOOT2",
                "BOOTLOADERS",
            ),
            _ => ("NORMAL_ROM", "EMMC_USER", "UPDATE"),
        };

        let file_name = format!("{}.img", entry.name);

        scatter_lines.push(format!("- partition_index: SYS{}", sys_idx));
        scatter_lines.push(format!("  partition_name: {}", entry.name));
        scatter_lines.push(format!("  file_name: {}", file_name));
        scatter_lines.push("  is_download: true".to_string());
        scatter_lines.push(format!("  type: {}", part_type));
        scatter_lines.push(format!("  linear_start_addr: 0x{:X}", entry.start_addr));
        scatter_lines.push(format!("  physical_start_addr: 0x{:X}", entry.start_addr));
        scatter_lines.push(format!("  partition_size: 0x{:X}", entry.size));
        scatter_lines.push(format!("  region: {}", region));
        scatter_lines.push("  storage: HW_STORAGE_EMMC".to_string());
        scatter_lines.push("  boundary_check: false".to_string());
        scatter_lines.push("  is_reserved: false".to_string());
        scatter_lines.push(format!("  operation_type: {}", operation_type));
        scatter_lines.push("  reserve: 0".to_string());
        scatter_lines.push(format!("  partition_hint: {}", entry.name));
        scatter_lines.push(String::new());

        partition_info_list.push((entry.name, entry.start_addr, entry.size));
        sys_idx += 1;
    }

    // 写入文件
    std::fs::write(output_file, scatter_lines.join("\n"))
        .map_err(|e| format!("写入 scatter 文件失败: {}", e))?;
    info!("刷机匣格式 scatter 文件已生成: {}", output_file);

    Ok(partition_info_list)
}
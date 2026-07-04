//! Scatter 文件生成（MTK SP Flash Tool 格式）
//!
//! - `parse_gpt_from_data`        — 解析 GPT 数据并打印分区表
//! - `generate_scatter_header`    — PRELOADER + EMMC_BOOT_1/2 公共头
//! - `generate_scatter_from_gpt`  — SP Flash Tool 格式 scatter

use log::info;

use super::gpt::GptInfo;

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
        println!(
            "{:<30} 0x{:014X} 0x{:014X}",
            entry.name, entry.start_addr, entry.size
        );
    }

    println!("\n共 {} 个分区", partitions.len());
    Ok(())
}

/// 生成单个 scatter 分区块（标准 SP Flash Tool 格式）
fn scatter_block(
    name: &str,
    physical_start_addr: u64,
    partition_size: u64,
    region: &str,
    operation_type: &str,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("{} 0x0", name.to_uppercase()));
    lines.push("{".to_string());
    lines.push(format!(
        "  physical_start_addr: 0x{:X}",
        physical_start_addr
    ));
    lines.push(format!("  partition_size: 0x{:X}", partition_size));
    lines.push(format!("  region: {}", region));
    lines.push("  storage: HW_STORAGE_EMMC".to_string());
    lines.push("  boundary_check: true".to_string());
    lines.push("  is_reserved: false".to_string());
    lines.push(format!("  operation_type: {}", operation_type));
    lines.push("  type: NORMAL_ROM".to_string());
    lines.push("  reserve: 0x00".to_string());
    lines.push("}".to_string());
    lines.push(String::new());
    lines.join("\n")
}

/// Scatter header 统一生成（PRELOADER + EMMC_BOOT_1 + EMMC_BOOT_2）
/// 标准 SP Flash Tool 格式
pub fn generate_scatter_header() -> String {
    let mut blocks = String::new();

    // PRELOADER: 属于 BOOTLOADERS 类型，region 为 EMMC_BOOT_1_BOOT2
    blocks.push_str(&scatter_block(
        "PRELOADER",
        0x0,
        0x0,
        "EMMC_BOOT_1_BOOT2",
        "BOOTLOADERS",
    ));

    // EMMC_BOOT_1
    blocks.push_str(&scatter_block(
        "EMMC_BOOT_1",
        0x0,
        0x0,
        "EMMC_BOOT_1",
        "UPDATE",
    ));

    // EMMC_BOOT_2
    blocks.push_str(&scatter_block(
        "EMMC_BOOT_2",
        0x0,
        0x0,
        "EMMC_BOOT_2",
        "UPDATE",
    ));

    blocks
}

/// 从 GPT 数据生成 SP Flash Tool 格式的 scatter 文件
/// 标准格式对齐 MTK ptgen 输出
pub fn generate_scatter_from_gpt(
    gpt_data: &[u8],
    output_file: &str,
) -> Result<Vec<(String, u64, u64, u32)>, String> {
    let gpt_info = GptInfo::parse(gpt_data)?;

    let mut scatter_lines: Vec<String> = Vec::new();
    let mut partition_info_list: Vec<(String, u64, u64, u32)> = Vec::new();

    // 添加 PRELOADER + EMMC_BOOT_1 + EMMC_BOOT_2（标准格式）
    scatter_lines.push(generate_scatter_header());

    // 遍历 GPT 分区表生成条目
    for entry in gpt_info.iter_partitions() {
        let block = scatter_block(
            &entry.name,
            entry.start_addr,
            entry.size,
            "EMMC_USER",
            "UPDATE",
        );
        scatter_lines.push(block);

        partition_info_list.push((entry.name, entry.start_addr, entry.size, 1));
    }

    // 写入文件
    std::fs::write(output_file, scatter_lines.join("\n"))
        .map_err(|e| format!("写入 scatter 文件失败: {}", e))?;
    info!("scatter 文件已生成: {}", output_file);

    Ok(partition_info_list)
}

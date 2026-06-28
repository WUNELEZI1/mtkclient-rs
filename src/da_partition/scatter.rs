//! Scatter 文件生成（MTK SP Flash 格式 + 刷机匣 YAML 格式）
//!
//! - `parse_gpt_from_data`        — 解析 GPT 数据并打印分区表
//! - `generate_scatter_header`    — PRELOADER + EMMC_BOOT_1/2 公共头
//! - `generate_scatter_from_gpt`  — SP Flash Tool 格式 scatter
//! - `generate_scatter_shoujixia` — 刷机匣 YAML-like scatter

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
        scatter_lines.push(format!(
            "{} 0x{:X}",
            entry.name.to_uppercase(),
            entry.start_addr
        ));
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
            "preloader" | "pgpt" | "sgpt" => ("SV5_BL_BIN", "EMMC_BOOT1_BOOT2", "BOOTLOADERS"),
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

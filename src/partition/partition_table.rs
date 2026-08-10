//! Scatter 文件生成（MTK SP Flash Tool YAML 格式）
//!
//! - `parse_gpt_from_data`        — 解析 GPT 数据并打印分区表
//! - `generate_scatter_header`    — PRELOADER 公共头
//! - `generate_scatter_from_gpt`  — SP Flash Tool YAML 格式 scatter

use log::info;

use super::gpt::GptInfo;

/// scatter 文件分隔符（对齐刷机匣 MT6768_Android_scatter.txt 格式）
const SEPARATOR: &str = "################################################mtkclient-rs################################################";


/// scatter 分区块参数
struct ScatterEntry<'a> {
    idx: usize,
    name: &'a str,
    addr: u64,
    size: u64,
    region: &'a str,
    operation_type: &'a str,
    blk_type: &'a str,
    is_upgradable: bool,
    is_reserved: bool,
}

/// 生成单个 scatter YAML 分区块（标准 SP Flash Tool 格式）
fn scatter_block(e: &ScatterEntry) -> String {
    format!(
        "- partition_index: SYS{}\n\
         partition_name: {}\n\
         file_name: NONE\n\
         is_download: false\n\
         type: {}\n\
         linear_start_addr: 0x{:X}\n\
         physical_start_addr: 0x{:X}\n\
         partition_size: 0x{:X}\n\
         region: {}\n\
         storage: HW_STORAGE_EMMC\n\
         boundary_check: false\n\
         is_reserved: {}\n\
         operation_type: {}\n\
         is_upgradable: {}\n\
         empty_boot_needed: false\n\
         reserve: 0x00\n",
        e.idx,
        e.name.to_lowercase(),
        e.blk_type,
        e.addr,
        e.addr,
        e.size,
        e.region,
        e.is_reserved,
        e.operation_type,
        e.is_upgradable,
    )
}

/// Scatter header 统一生成（PRELOADER）
pub fn generate_scatter_header() -> String {
    scatter_block(&ScatterEntry {
        idx: 0,
        name: "preloader",
        addr: 0x0,
        size: 0x400000,
        region: "EMMC_BOOT1_BOOT2",
        operation_type: "BOOTLOADERS",
        blk_type: "SV5_BL_BIN",
        is_upgradable: true,
        is_reserved: false,
    })
}

/// 从 GPT 数据生成 SP Flash Tool 格式的 scatter 文件（YAML 格式）
pub fn generate_scatter_from_gpt(
    gpt_data: &[u8],
    output_file: &str,
) -> Result<Vec<(String, u64, u64, u32)>, String> {
    let gpt_info = GptInfo::parse(gpt_data)?;

    let mut lines: Vec<String> = Vec::new();
    let mut partition_info_list: Vec<(String, u64, u64, u32)> = Vec::new();

    // === General Setting ===
    lines.push(SEPARATOR.to_string());
    lines.push("#".to_string());
    lines.push("#  General Setting".to_string());
    lines.push("#".to_string());
    lines.push(SEPARATOR.to_string());
    lines.push("- general: MTK_PLATFORM_CFG".to_string());
    lines.push("  info:".to_string());
    lines.push("  - config_version: V1.1.2".to_string());
    lines.push("    platform: MT6768".to_string());
    lines.push("    project: mtkclient-rs".to_string());
    lines.push("    storage: EMMC".to_string());
    lines.push("    boot_channel: MSDC_0".to_string());
    lines.push("    block_size: 0x200".to_string());
    lines.push("    check_bootloaders_consistency: false".to_string());
    lines.push(String::new());

    // === Layout Setting ===
    lines.push(SEPARATOR.to_string());
    lines.push("#".to_string());
    lines.push("#  Layout Setting".to_string());
    lines.push("#".to_string());
    lines.push(SEPARATOR.to_string());
    lines.push(String::new());

    // PRELOADER 块
    lines.push(generate_scatter_header());
    lines.push(String::new());

    // 特殊分区 operation_type 映射（对齐刷机匣 MT6768 scatter）
    let special_ops: &[(&str, &str, bool)] = &[
        ("nvcfg", "PROTECTED", false),
        ("nvdata", "PROTECTED", false),
        ("protect1", "PROTECTED", false),
        ("protect2", "PROTECTED", false),
        ("proinfo", "PROTECTED", false),
        ("flashinfo", "RESERVED", false),
    ];

    // 遍历 GPT 分区表生成条目
    for (i, entry) in gpt_info.iter_partitions().enumerate() {
        let idx = i + 1;

        let (op_type, upgradable) = special_ops
            .iter()
            .find(|(name, _, _)| entry.name.eq_ignore_ascii_case(name))
            .map(|(_, op, up)| (*op, *up))
            .unwrap_or(("UPDATE", true));

        let is_reserved = entry.name.eq_ignore_ascii_case("flashinfo");

        lines.push(scatter_block(&ScatterEntry {
            idx,
            name: &entry.name,
            addr: entry.start_addr,
            size: entry.size,
            region: "EMMC_USER",
            operation_type: op_type,
            blk_type: "NORMAL_ROM",
            is_upgradable: upgradable,
            is_reserved,
        }));
        lines.push(String::new());

        partition_info_list.push((entry.name, entry.start_addr, entry.size, 1));
    }

    std::fs::write(output_file, lines.join("\n"))
        .map_err(|e| format!("写入 scatter 文件失败: {}", e))?;
    info!("scatter 文件已生成: {}", output_file);

    Ok(partition_info_list)
}

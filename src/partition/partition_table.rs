//! Scatter file generation (MTK SP Flash Tool YAML format)
//!
//! - `parse_gpt_from_data`        — parse GPT data and print the partition table
//! - `generate_scatter_header`    — common PRELOADER header
//! - `generate_scatter_from_gpt`  — SP Flash Tool YAML scatter

use log::info;

use super::gpt::GptInfo;

/// Scatter file separator (matches the MT6768_Android_scatter.txt format)
const SEPARATOR: &str = "################################################mtkclient-rs################################################";

/// Scatter partition block parameters
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

/// Build one scatter YAML partition block (standard SP Flash Tool format)
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

/// Infer the SP Flash Tool `type` field from the partition name
///
/// Filesystem partitions are marked EXT4 so SPFT formats them; everything else
/// (boot/recovery/preloader, etc.) is NORMAL_ROM. As in the reference scatter, only
/// filesystem partitions may be format-written by SPFT.
fn fs_type_for(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "system" | "system_ext" | "system_other" | "vendor" | "product" | "odm" | "userdata"
        | "cache" | "metadata" | "persist" | "cust" | "version" => "EXT4",
        _ => "NORMAL_ROM",
    }
}

/// Common scatter header generation (PRELOADER)
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

/// Map a chip hw_code to the SP Flash Tool scatter `platform:` string.
///
/// Matches both `ChipConfig.hw_code` and `ChipConfig.da_code` forms
/// (e.g. MT6768 hw_code=0x0707 and da_code=0x6768 both yield "MT6768"),
/// so either chip code resolves correctly. Unknown chips fall back to an
/// `MTxxxx` form so SPFT does not refuse to load due to a missing/invalid platform field.
fn hw_code_to_platform(hw_code: u32) -> String {
    match hw_code {
        // MT6768 / MT6769 (Helio P65/G85)
        0x6768 | 0x0707 => "MT6768".to_string(),
        // MT6771 / MT8385 / MT8183 / MT8666 (Helio P60/P70/G80)
        0x6771 | 0x0788 => "MT6771".to_string(),
        other => format!("MT{:04X}", other),
    }
}

/// Generate an SP Flash Tool scatter file (YAML) from GPT data
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
    // platform must match the real chip, otherwise SPFT refuses to load on non-MT6768 devices.
    // Read the device-reported hw_code (device chip) from DA session state, then look up the platform name.
    let platform = hw_code_to_platform(
        crate::connection::session::SessionState::load()
            .map(|s| s.hw_code as u32)
            .unwrap_or(0),
    );
    lines.push(format!("    platform: {}", platform));
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

    // PRELOADER blocks
    lines.push(generate_scatter_header());
    lines.push(String::new());

    // Special partition operation_type mapping (matching the MT6768 scatter)
    let special_ops: &[(&str, &str, bool)] = &[
        ("nvcfg", "PROTECTED", false),
        ("nvdata", "PROTECTED", false),
        ("protect1", "PROTECTED", false),
        ("protect2", "PROTECTED", false),
        ("proinfo", "PROTECTED", false),
        ("flashinfo", "RESERVED", false),
    ];

    // Generate entries by walking the GPT partition table
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
            blk_type: fs_type_for(&entry.name),
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

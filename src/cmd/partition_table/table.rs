//! GPT 分区表格式化输出
//!
//! - `print_gpt_table` — ASCII 表格风格打印分区表

use colored::Colorize;
use unicode_width::UnicodeWidthStr;

use super::emmc;
use crate::partition::GptInfo;

/// 打印 GPT 表格到控制台（含 eMMC_Boot1/Boot2 显示）
/// 使用 ASCII 表格风格: | 和 - 作为分隔符，列标签中文
fn pad_display_width(input: &str, width: usize, align_right: bool) -> String {
    let display_width = UnicodeWidthStr::width(input);
    if display_width >= width {
        input.to_string()
    } else {
        let padding = " ".repeat(width - display_width);
        if align_right {
            format!("{}{}", padding, input)
        } else {
            format!("{}{}", input, padding)
        }
    }
}

pub fn print_gpt_table(
    data: &[u8],
    boot1_size: u64,
    boot2_size: u64,
    super_metadata: Option<&crate::partition::lp::SuperMetadata>,
) {
    let gpt_info = match GptInfo::parse(data) {
        Ok(info) => info,
        Err(_) => return,
    };

    let partitions = gpt_info.partitions();
    if let Ok(report) = gpt_info.crc_report() {
        println!();
        println!("{}", "GPT 校验:".bright_white().bold());
        if report.header_ok() {
            println!(
                "  HeaderCRC: {}",
                format!("0x{:08X} OK", report.stored_header_crc32).green()
            );
        } else {
            println!(
                "  HeaderCRC: {}",
                format!(
                    "失败 原=0x{:08X} 计算=0x{:08X}",
                    report.stored_header_crc32, report.calculated_header_crc32
                )
                .yellow()
            );
        }
        if report.partition_entries_ok() {
            println!(
                "  分区条目CRC: {}",
                format!("0x{:08X} OK", report.stored_partition_entries_crc32).green()
            );
        } else {
            println!(
                "  分区条目CRC: {}",
                format!(
                    "失败 原=0x{:08X} 计算=0x{:08X}",
                    report.stored_partition_entries_crc32,
                    report.calculated_partition_entries_crc32
                )
                .yellow()
            );
        }
    }

    // 列宽定义（地址按 0x%016X = 18 字符，含 0x 前缀）
    const W_IDX: usize = 4; // "01" 等
    const W_NAME: usize = 22; // 分区名
    const W_ADDR: usize = 18; // 0x00000000000000
    const W_SIZE: usize = 10; // "1024.00 MB"
    const W_BYTES: usize = 17; // "1,073,741,824"

    // 构建分隔线: +----+--------+...
    let sep = format!(
        "+-{}-+-{}-+-{}-+-{}-+-{}-+-{}-+",
        "-".repeat(W_IDX),
        "-".repeat(W_NAME),
        "-".repeat(W_ADDR),
        "-".repeat(W_ADDR),
        "-".repeat(W_SIZE),
        "-".repeat(W_BYTES)
    );

    // 表头行（先格式化定宽纯文本，再整体上色）
    // 注意：表头含中文字符，必须用 pad_display_width 计算显示宽度
    let header_plain = format!(
        "| {} | {} | {} | {} | {} | {} |",
        pad_display_width("编号", W_IDX, false),
        pad_display_width("名称", W_NAME, false),
        pad_display_width("起始地址", W_ADDR, true),
        pad_display_width("结束地址", W_ADDR, true),
        pad_display_width("大小", W_SIZE, true),
        pad_display_width("字节数", W_BYTES, true),
    );

    // 格式化一行为定宽纯文本（不含颜色代码）
    fn fmt_row(idx: &str, name: &str, start: &str, end: &str, size: &str, bytes: &str) -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} |",
            pad_display_width(idx, 4, false),
            pad_display_width(name, 22, false),
            pad_display_width(start, 18, true),
            pad_display_width(end, 18, true),
            pad_display_width(size, 10, true),
            pad_display_width(bytes, 17, true)
        )
    }

    println!();
    println!("{}", sep);
    println!("{}", header_plain.bright_white().bold());
    println!("{}", sep);

    let mut row = 0usize;

    // eMMC Boot 区域（灰色显示，标记为 eMMC_Boot1 / eMMC_Boot2）
    if boot1_size > 0 {
        row += 1;
        let line = fmt_row(
            &format!("{:02}", row),
            "eMMC_Boot1",
            &format!("0x{:016X}", 0u64),
            &format!("0x{:016X}", boot1_size.saturating_sub(1)),
            &emmc::format_size(boot1_size),
            &emmc::format_bytes_comma(boot1_size),
        );
        println!("{}", line.dimmed());
    }
    if boot2_size > 0 {
        row += 1;
        let line = fmt_row(
            &format!("{:02}", row),
            "eMMC_Boot2",
            &format!("0x{:016X}", 0u64),
            &format!("0x{:016X}", boot2_size.saturating_sub(1)),
            &emmc::format_size(boot2_size),
            &emmc::format_bytes_comma(boot2_size),
        );
        println!("{}", line.dimmed());
    }

    // GPT 分区（交替亮度，先 format 定宽再整体上色）
    for (i, entry) in partitions.iter().enumerate() {
        row += 1;
        let start_addr = entry.start_addr;
        let end_addr = start_addr.saturating_add(entry.size).saturating_sub(1);
        let line = fmt_row(
            &format!("{:02}", row),
            &entry.name,
            &format!("0x{:016X}", start_addr),
            &format!("0x{:016X}", end_addr),
            &emmc::format_size(entry.size),
            &emmc::format_bytes_comma(entry.size),
        );

        if i % 2 == 0 {
            println!("{}", line);
        } else {
            println!("{}", line.bright_white());
        }

        // 遇到 super 分区时，立即展开其 logical partitions
        if entry.name.to_lowercase() == "super" {
            if let Some(ref meta) = super_metadata {
                if !meta.partitions.is_empty() {
                    let total_lp = meta.partitions.len();
                    for (j, lp) in meta.partitions.iter().enumerate() {
                        let (off, size) = meta.find_partition(&lp.name).unwrap_or((0, 0));
                        let is_last = j + 1 == total_lp;
                        let branch = if is_last { "└─" } else { "├─" };
                        let ab_tag = if lp.is_slot_suffixed() {
                            let suffix = if lp.name.ends_with("_a") {
                                " [A-slot]".cyan()
                            } else if lp.name.ends_with("_b") {
                                " [B-slot]".magenta()
                            } else {
                                "".into()
                            };
                            suffix
                        } else {
                            "".into()
                        };
                        let lp_line = format!(
                            "     {} {}  offset=0x{:08X}  size=0x{:08X} ({}){}",
                            branch.cyan(),
                            pad_display_width(&lp.name, 20, false),
                            off,
                            size,
                            emmc::format_size(size),
                            ab_tag,
                        );
                        println!("{}", lp_line.dimmed());
                    }
                }
            }
        }
    }

    // 底部分隔线 + 总计
    println!("{}", sep);
    let total_label = format!("| 共 {} 个分区", row);
    let sep_display_width = UnicodeWidthStr::width(sep.as_str());
    let label_display_width = UnicodeWidthStr::width(total_label.as_str());
    let padding_needed = sep_display_width.saturating_sub(label_display_width + 1); // +1 for trailing " |"
    let total_text = format!("{}{} |", total_label, " ".repeat(padding_needed));
    println!("{}", total_text.green().bold());
    println!("{}", sep);
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn pad_display_width_counts_chinese_as_double_width() {
        let padded = pad_display_width("起始地址", 10, false);

        assert_eq!(UnicodeWidthStr::width(padded.as_str()), 10);
    }

    #[test]
    fn pad_display_width_right_aligns_ascii_values() {
        assert_eq!(pad_display_width("1 MB", 8, true), "    1 MB");
    }
}

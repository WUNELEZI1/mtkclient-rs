//! GPT partition table formatted output (render facade).
//!
//! - `print_gpt_table_public` — entry point reused by the ops layer and the cmd layer
//! - `print_gpt_table`        — prints the partition table in ASCII table style
//!
//! This facade sits below the command layer so `ops::printgpt` no longer imports
//! `cmd`. Output formatting (including every Chinese label) is unchanged.

use crate::color::Colorize;

use crate::partition::GptInfo;

/// Table rendering reused by the ops layer (keeps output consistent with the legacy `printgpt` command).
pub fn print_gpt_table_public(
    data: &[u8],
    boot1_size: u64,
    boot2_size: u64,
    super_metadata: Option<&crate::partition::lp::SuperMetadata>,
) {
    print_gpt_table(data, boot1_size, boot2_size, super_metadata);
}

// eMMC capacity formatting helpers (reused by GPT table rendering).

/// Format a byte count as a human-readable string (KB/MB/GB, 2 decimals)
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    if bytes >= (1024 * 1024 * 1024) {
        format!("{:.2} GB", bytes as f64 / GB)
    } else if bytes >= (1024 * 1024) {
        format!("{:.2} MB", bytes as f64 / MB)
    } else if bytes >= 1024 {
        format!("{:.2} KB", bytes as f64 / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// Format a byte count with thousands separators
fn format_bytes_comma(bytes: u64) -> String {
    let s = bytes.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}

/// Print the GPT table to the console (including eMMC_Boot1/Boot2)
/// Uses ASCII table style: | and - as separators, Chinese column labels
fn pad_display_width(input: &str, width: usize, align_right: bool) -> String {
    let display_width = crate::util::display_width(input);
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

    // Column widths (address printed as 0x%016X = 18 chars, including the 0x prefix)
    const W_IDX: usize = 4; // "01" etc.
    const W_NAME: usize = 22; // partition name
    const W_ADDR: usize = 18; // 0x00000000000000
    const W_SIZE: usize = 10; // "1024.00 MB"
    const W_BYTES: usize = 17; // "1,073,741,824"

    // Build the separator line: +----+--------+...
    let sep = format!(
        "+-{}-+-{}-+-{}-+-{}-+-{}-+-{}-+",
        "-".repeat(W_IDX),
        "-".repeat(W_NAME),
        "-".repeat(W_ADDR),
        "-".repeat(W_ADDR),
        "-".repeat(W_SIZE),
        "-".repeat(W_BYTES)
    );

    // Header row (format as fixed-width plain text first, then color the whole row)
    // Note: the header contains Chinese characters, so pad_display_width must be used to compute display width
    let header_plain = format!(
        "| {} | {} | {} | {} | {} | {} |",
        pad_display_width("编号", W_IDX, false),
        pad_display_width("名称", W_NAME, false),
        pad_display_width("起始地址", W_ADDR, true),
        pad_display_width("结束地址", W_ADDR, true),
        pad_display_width("大小", W_SIZE, true),
        pad_display_width("字节数", W_BYTES, true),
    );

    // Format one row as fixed-width plain text (without color codes)
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

    // eMMC Boot area (shown dimmed, labeled eMMC_Boot1 / eMMC_Boot2)
    if boot1_size > 0 {
        row += 1;
        let line = fmt_row(
            &format!("{:02}", row),
            "eMMC_Boot1",
            &format!("0x{:016X}", 0u64),
            &format!("0x{:016X}", boot1_size.saturating_sub(1)),
            &format_size(boot1_size),
            &format_bytes_comma(boot1_size),
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
            &format_size(boot2_size),
            &format_bytes_comma(boot2_size),
        );
        println!("{}", line.dimmed());
    }

    // GPT partitions (alternating brightness; format fixed width first, then color the whole row)
    for (i, entry) in partitions.iter().enumerate() {
        row += 1;
        let start_addr = entry.start_addr;
        let end_addr = start_addr.saturating_add(entry.size).saturating_sub(1);
        let line = fmt_row(
            &format!("{:02}", row),
            &entry.name,
            &format!("0x{:016X}", start_addr),
            &format!("0x{:016X}", end_addr),
            &format_size(entry.size),
            &format_bytes_comma(entry.size),
        );

        if i % 2 == 0 {
            println!("{}", line);
        } else {
            println!("{}", line.bright_white());
        }

        // When reaching the super partition, expand its logical partitions immediately
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
                            format_size(size),
                            ab_tag,
                        );
                        println!("{}", lp_line.dimmed());
                    }
                }
            }
        }
    }

    // Bottom separator line + total
    println!("{}", sep);
    let total_label = format!("| 共 {} 个分区", row);
    let sep_display_width = crate::util::display_width(sep.as_str());
    let label_display_width = crate::util::display_width(total_label.as_str());
    let padding_needed = sep_display_width.saturating_sub(label_display_width + 1); // +1 for trailing " |"
    let total_text = format!("{}{} |", total_label, " ".repeat(padding_needed));
    println!("{}", total_text.green().bold());
    println!("{}", sep);
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_display_width_counts_chinese_as_double_width() {
        let padded = pad_display_width("起始地址", 10, false);

        assert_eq!(crate::util::display_width(padded.as_str()), 10);
    }

    #[test]
    fn pad_display_width_right_aligns_ascii_values() {
        assert_eq!(pad_display_width("1 MB", 8, true), "    1 MB");
    }
}
